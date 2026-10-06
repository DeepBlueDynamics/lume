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
fn aliases(path: &str) -> &'static str {
    match path {
        "navigation.speedOverGround" => "SOG GPS speed",
        "navigation.speedThroughWater" => "STW water speed",
        "navigation.courseOverGroundTrue" => "COG true course",
        "navigation.courseOverGroundMagnetic" => "COG magnetic course",
        "environment.wind.speedApparent" => "AWS apparent wind speed",
        "environment.wind.angleApparent" => "AWA apparent wind angle",
        "environment.wind.speedTrue" => "TWS true wind speed",
        "environment.wind.angleTrueWater" => "TWA true wind angle",
        "environment.depth.belowKeel" => "UKC under keel clearance",
        _ => "",
    }
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
}
impl PathsResolver {
    pub fn new(catalog: &ti_sql::SqlCatalog) -> Self {
        let mut fields = catalog.fields.clone();
        fields.sort_by_key(ti_sql::field_name);
        let columns: Vec<_> = fields
            .into_iter()
            .map(|field| {
                let path = field.path.split('#').next().unwrap_or(&field.path);
                let path = path.strip_suffix("$source").unwrap_or(path);
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
                    title: format!("{} {} {acronym} {aggregate}", c.name, words(&c.field.path)),
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
        Self {
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
        let mut hits: Vec<_> = self
            .index
            .search_quiet(phrase, SearchVariant::Classic, &Bm25Params::default(), None)
            .into_iter()
            .map(|h| (h.section_index, h.score))
            .collect();
        hits.sort_by(|a, b| {
            b.1.total_cmp(&a.1)
                .then_with(|| self.columns[a.0].name.cmp(&self.columns[b.0].name))
        });
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
        let vessel = if let Some(vessel) = vessel {
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
