//! Offline lexical path resolution using Lume BM25 and pinned Signal K metadata.
use crate::bm25::{Bm25Index, Bm25Params, SearchVariant, Section};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{sync::OnceLock, time::Instant};
use ti_contracts::{FieldSpec, Predicate, RoaringBitmap};

#[derive(Deserialize)]
struct SpecPath {
    pattern: String,
    description: String,
    units: Option<String>,
}
#[derive(Deserialize)]
struct Bundle {
    entries: Vec<SpecPath>,
}
fn descriptions() -> &'static [SpecPath] {
    static PATHS: OnceLock<Bundle> = OnceLock::new();
    &PATHS
        .get_or_init(|| {
            serde_json::from_str(include_str!("ti_resolve/signalk_paths.json"))
                .expect("bundled Signal K metadata")
        })
        .entries
}
fn matches(pattern: &str, path: &str) -> bool {
    let a: Vec<_> = pattern.split('.').collect();
    let b: Vec<_> = path.split('.').collect();
    a.len() == b.len() && a.iter().zip(b).all(|(a, b)| *a == "*" || *a == b)
}
fn words(path: &str) -> String {
    let chars: Vec<_> = path.chars().collect();
    let mut out = String::new();
    for (i, c) in chars.iter().enumerate() {
        if c.is_uppercase()
            && i > 0
            && (chars[i - 1].is_lowercase()
                || (chars[i - 1].is_uppercase()
                    && chars.get(i + 1).is_some_and(|n| n.is_lowercase())))
        {
            out.push(' ');
        }
        out.push(if matches!(c, '.' | '_' | '@' | '$' | '#') {
            ' '
        } else {
            *c
        });
    }
    out
}
#[derive(Deserialize)]
struct Vocabulary {
    entries: Vec<Alias>,
}
#[derive(Deserialize)]
struct Alias {
    pattern: String,
    aliases: String,
    units: Option<String>,
}
fn vocabulary() -> &'static [Alias] {
    static DATA: OnceLock<Vocabulary> = OnceLock::new();
    &DATA
        .get_or_init(|| {
            serde_json::from_str(include_str!("ti_resolve/vocabulary.json"))
                .expect("bundled nautical vocabulary")
        })
        .entries
}
fn aliases(path: &str) -> String {
    vocabulary()
        .iter()
        .filter(|a| matches(&a.pattern, path))
        .map(|a| a.aliases.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}
fn unit_words(unit: Option<&str>) -> &'static str {
    match unit {
        Some("m/s") => "speed knots knot kts metres meters second",
        Some("m") => "metres meters depth distance fathoms feet",
        Some("V") => "volts volt voltage",
        Some("A") => "amps amp amperes current",
        Some("W") => "watts watt kilowatts power",
        Some("K") => "kelvin Celsius centigrade temperature heat hot degrees",
        Some("Pa") => "pascals pressure millibars mbar hpa barometer",
        Some("Hz") => "hertz frequency RPM revs revolutions",
        Some("rad") => "radians radian degrees angle",
        Some("ratio") | Some("%") => "percent percentage fraction",
        _ => "",
    }
}
fn tokens(text: &str) -> Vec<String> {
    words(text)
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(String::from)
        .collect()
}
/// Single edits including adjacent transpositions; no broad fuzzy expansions.
fn near(a: &str, b: &str) -> bool {
    let a: Vec<_> = a.chars().collect();
    let b: Vec<_> = b.chars().collect();
    if a.len().abs_diff(b.len()) > 1 {
        return false;
    }
    if a.len() == b.len() {
        let differing: Vec<_> = (0..a.len()).filter(|&i| a[i] != b[i]).collect();
        return differing.len() == 1
            || (differing.len() == 2
                && differing[1] == differing[0] + 1
                && a[differing[0]] == b[differing[1]]
                && a[differing[1]] == b[differing[0]]);
    }
    let (short, long) = if a.len() < b.len() {
        (&a, &b)
    } else {
        (&b, &a)
    };
    let (mut i, mut j, mut skipped) = (0, 0, false);
    while i < short.len() && j < long.len() {
        if short[i] == long[j] {
            i += 1;
            j += 1;
        } else if skipped {
            return false;
        } else {
            skipped = true;
            j += 1;
        }
    }
    true
}
struct Column {
    field: FieldSpec,
    name: String,
    description: String,
}
/// An immutable search index over columns present in a planning catalog snapshot.
pub struct PathsResolver {
    columns: Vec<Column>,
    index: Bm25Index,
    lexicon: std::collections::BTreeSet<String>,
}
impl PathsResolver {
    pub fn new(catalog: &ti_sql::SqlCatalog) -> Self {
        let mut fields = catalog.fields.clone();
        fields.sort_by_key(ti_sql::field_name);
        let columns: Vec<_> = fields
            .into_iter()
            .map(|mut field| {
                let path = field.path.split('#').next().unwrap_or(&field.path);
                let path = path.strip_suffix("$source").unwrap_or(path);
                let spec = descriptions()
                    .iter()
                    .filter(|p| matches(&p.pattern, path))
                    .max_by_key(|p| p.pattern.split('.').filter(|s| *s != "*").count());
                if field.units.is_none() {
                    field.units = spec.and_then(|p| p.units.clone()).or_else(|| {
                        vocabulary()
                            .iter()
                            .find(|a| matches(&a.pattern, path))
                            .and_then(|a| a.units.clone())
                    });
                }
                let description = descriptions()
                    .iter()
                    .filter(|p| matches(&p.pattern, path))
                    .max_by_key(|p| p.pattern.split('.').filter(|s| *s != "*").count())
                    .map(|p| p.description.clone())
                    .unwrap_or_else(|| format!("Store telemetry column: {}", words(&field.path)));
                let description = if field.path.ends_with("$source") {
                    format!("Reporting sources and provenance for: {description}")
                } else {
                    description
                };
                Column {
                    name: ti_sql::field_name(&field),
                    field,
                    description,
                }
            })
            .collect();
        let sections = columns
            .iter()
            .map(|c| {
                let leaf = words(c.field.path.rsplit('.').next().unwrap_or(&c.field.path));
                let acronym: String = leaf
                    .split_whitespace()
                    .filter_map(|w| w.chars().next())
                    .collect();
                let agg = c.name.split_once('@').map(|(_, agg)| agg).unwrap_or("");
                let aggregate = match agg {
                    "mean" => "mean average",
                    "min" => "min minimum lowest",
                    "max" => "max maximum highest",
                    "last" => "last latest",
                    "count" => "count samples",
                    "starts" => "starts engine transitions",
                    "edges" => "edges rising transitions",
                    _ => "",
                };
                Section {
                    title: format!(
                        "{} {} {acronym} {aggregate} {} {}",
                        c.name,
                        words(&c.field.path),
                        aliases(&c.field.path),
                        unit_words(c.field.units.as_deref())
                    ),
                    body: format!(
                        "{} {} {}",
                        c.description,
                        aliases(&c.field.path),
                        c.field.units.as_deref().unwrap_or("")
                    ),
                    line_number: 0,
                    filename: None,
                    entities: vec![],
                }
            })
            .collect();
        let lexicon = columns
            .iter()
            .flat_map(|c| {
                tokens(&format!(
                    "{} {} {}",
                    words(&c.field.path),
                    aliases(&c.field.path),
                    unit_words(c.field.units.as_deref())
                ))
            })
            .collect();
        Self {
            lexicon,
            columns,
            index: Bm25Index::build(sections, None),
        }
    }
    /// Rank lexical matches deterministically; the column set comes only from the store.
    pub fn rank(&self, phrase: &str, limit: usize) -> Vec<(String, f64)> {
        self.hits(phrase)
            .into_iter()
            .take(limit)
            .map(|(i, s)| (self.columns[i].name.clone(), s))
            .collect()
    }
    fn hits(&self, phrase: &str) -> Vec<(usize, f64)> {
        let query: Vec<_> = tokens(phrase)
            .into_iter()
            .map(|word| {
                let replacement = match word.as_str() {
                    "left" => Some("port"),
                    "right" | "stbd" => Some("starboard"),
                    "domestic" | "service" => Some("house"),
                    _ => None,
                };
                if let Some(replacement) = replacement {
                    return replacement.to_string();
                }
                if word.len() >= 4 && !self.lexicon.contains(&word) {
                    let close: Vec<_> = self
                        .lexicon
                        .iter()
                        .filter(|candidate| near(&word, candidate))
                        .collect();
                    if close.len() == 1 {
                        return close[0].clone();
                    }
                }
                word
            })
            .collect();
        let phrase = query.join(" ");
        let mut hits: Vec<_> = self
            .index
            .search_quiet(
                &phrase,
                SearchVariant::Classic,
                &Bm25Params::default(),
                None,
            )
            .into_iter()
            .map(|h| (h.section_index, h.score))
            .collect();
        // Source metadata is useful only when explicitly requested. Normal requests
        // should not lose their three candidate slots to aggregate/source variants.
        for (i, score) in &mut hits {
            if self.columns[*i].field.path.ends_with("$source")
                && !query
                    .iter()
                    .any(|w| matches!(w.as_str(), "source" | "sources" | "provenance"))
            {
                *score *= 0.1;
            }
        }
        hits.sort_by(|a, b| {
            b.1.total_cmp(&a.1)
                .then_with(|| self.columns[a.0].name.cmp(&self.columns[b.0].name))
        });
        let mut seen = std::collections::BTreeSet::new();
        hits.retain(|(i, _)| seen.insert(self.columns[*i].field.path.clone()));
        hits
    }
    pub async fn resolve(&self, engine: &ti_sql::TiEngine, args: &Value) -> Result<Value, String> {
        let started = Instant::now();
        let phrase = args
            .get("phrase")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .ok_or("phrase is required")?;
        if phrase.len() > 4096 {
            return Err("phrase exceeds 4096 bytes".into());
        }
        let limit = args
            .get("limit")
            .map(|v| {
                v.as_u64()
                    .filter(|n| *n > 0)
                    .ok_or("limit must be a positive integer")
            })
            .transpose()?
            .unwrap_or(8)
            .min(500) as usize;
        let vessel = args
            .get("vessel")
            .map(|v| v.as_str().ok_or("vessel must be a string"))
            .transpose()?;
        // Names/MMSIs in a natural phrase qualify latest-value lookup too.
        let qualified: Vec<_> = engine
            .session
            .catalog
            .vessels
            .values()
            .filter(|v| {
                [Some(v.urn.as_str()), v.name.as_deref(), v.mmsi.as_deref()]
                    .into_iter()
                    .flatten()
                    .any(|name| {
                        let needle = tokens(name);
                        let haystack = tokens(phrase);
                        !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
                    })
            })
            .collect();
        let inferred = if qualified.len() == 1 {
            Some(qualified[0].urn.as_str())
        } else {
            None
        };
        let vessel = if let Some(vessel) = vessel.or(inferred) {
            let exact = engine
                .session
                .catalog
                .vessels
                .values()
                .find(|v| v.urn == vessel);
            let matched: Vec<_> = engine
                .session
                .catalog
                .vessels
                .values()
                .filter(|v| v.name.as_deref() == Some(vessel) || v.mmsi.as_deref() == Some(vessel))
                .collect();
            let found = exact
                .or_else(|| {
                    if matched.len() == 1 {
                        matched.first().copied()
                    } else {
                        None
                    }
                })
                .ok_or("vessel must identify one known URN, name or MMSI")?;
            Some(found.ord)
        } else {
            None
        };
        let mut keys =
            engine
                .session
                .source
                .shards(vessel.as_ref().map(std::slice::from_ref), 0, u32::MAX);
        keys.sort_by(|a, b| b.shard.cmp(&a.shard).then_with(|| a.vessel.cmp(&b.vessel)));
        let mut candidates = Vec::new();
        for (i, score) in self.hits(phrase) {
            let column = &self.columns[i];
            let mut latest: Option<(ti_contracts::ShardKey, u32)> = None;
            for key in &keys {
                if latest
                    .as_ref()
                    .is_some_and(|(old, _)| old.shard > key.shard)
                {
                    break;
                }
                let cols = engine
                    .session
                    .source
                    .eval(*key, &Predicate::Present(column.field.id))
                    .map_err(|e| e.to_string())?;
                if let Some(col) = cols.max() {
                    if latest
                        .as_ref()
                        .is_none_or(|(old, old_col)| key.shard > old.shard || col > *old_col)
                    {
                        latest = Some((*key, col));
                    }
                }
            }
            if vessel.is_some() && latest.is_none() {
                continue;
            }
            let mut row = serde_json::Map::new();
            if let Some((key, col)) = latest {
                let batch = engine
                    .session
                    .source
                    .read(key, &RoaringBitmap::from_iter([col]), &[column.field.id])
                    .map_err(|e| e.to_string())?;
                row = ti_sql::rows_json(&[batch])
                    .map_err(|e| e.to_string())?
                    .into_iter()
                    .next()
                    .unwrap_or_default();
            }
            let agg = column.name.split_once('@').map(|(_, a)| a);
            candidates.push(json!({"path":column.field.path,"column":column.name,"agg":agg,"units":column.field.units,
                "description":column.description,"score":score,"last_value":row.get(&column.name),"last_ts":row.get("ts"),"last_vessel":row.get("vessel")}));
            if candidates.len() == limit {
                break;
            }
        }
        let mut reply = json!({"phrase":phrase,"candidates":candidates,"elapsed_ms":started.elapsed().as_millis() as u64,"truncated":false,"hint":null});
        loop {
            let text = serde_json::to_string(&reply).map_err(|e| e.to_string())?;
            if serde_json::to_vec(&json!({"content":[{"type":"text","text":text}]}))
                .map_err(|e| e.to_string())?
                .len()
                <= ti_sql::MAX_BYTES - 256
            {
                break;
            }
            if reply["candidates"]
                .as_array_mut()
                .ok_or("missing candidates")?
                .pop()
                .is_none()
            {
                return Err("resolve metadata exceeds 64 KiB".into());
            }
            reply["truncated"] = json!(true);
            reply["hint"] = json!("Narrow the phrase or request fewer candidates.");
        }
        Ok(reply)
    }
}
