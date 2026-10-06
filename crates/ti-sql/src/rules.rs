//! W10: one-pass SQL bitmap alert rules and deterministic historical replay.
use crate::{core_error, SqlSession, TelemetryExec};
use datafusion::common::{DataFusionError, Result};
pub use datafusion::common::DataFusionError as RuleError;
pub type RuleResult<T> = datafusion::common::Result<T>;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use ti_contracts::{AlertRule, BucketIx, Catalog, Document, DocumentIndex, Predicate, RoaringTreemap, VesselOrd};
use ti_store::{DocStore, Store};

/// Inject Lume BM25 while allowing a dry run to use an unpersisted document set.
pub type RuleIndexFactory = dyn Fn(DocStore, Arc<dyn Catalog>, u64)
    -> ti_contracts::Result<Arc<dyn DocumentIndex>> + Send + Sync;

fn error(message: impl Into<String>) -> DataFusionError {
    DataFusionError::Plan(message.into())
}
fn quote(value: &str) -> String { format!("'{}'", value.replace('\'', "''")) }
fn identifier(value: &str) -> String { format!("\"{}\"", value.replace('"', "\"\"")) }
fn timestamp(bucket: u64, width: u64) -> Result<i64> {
    i64::try_from(i128::from(ti_contracts::EPOCH) + i128::from(bucket) * i128::from(width))
        .map_err(|_| error("rule timestamp overflow"))
}
fn expression(predicate: &str) -> Result<String> {
    use datafusion::sql::sqlparser::{dialect::GenericDialect, parser::Parser, tokenizer::Token};
    let mut parser = Parser::new(&GenericDialect {}).try_with_sql(predicate)
        .map_err(|e| error(e.to_string()))?;
    let expr = parser.parse_expr().map_err(|e| error(e.to_string()))?;
    if parser.peek_token().token != Token::EOF {
        return Err(error("rule when must contain exactly one SQL boolean expression"));
    }
    Ok(expr.to_string())
}
fn placeholders(template: &str) -> Result<Vec<String>> {
    let mut columns = BTreeSet::new();
    let mut rest = template;
    while let Some((literal, after)) = rest.split_once('{') {
        if literal.contains('}') { return Err(error("unmatched message brace")); }
        let (column, tail) = after.split_once('}').ok_or_else(|| error("unclosed message placeholder"))?;
        if column.is_empty() || column.contains('{') { return Err(error("invalid message placeholder")); }
        columns.insert(column.to_owned());
        rest = tail;
    }
    if rest.contains('}') { return Err(error("unmatched message brace")); }
    Ok(columns.into_iter().collect())
}

#[derive(Clone, Serialize, Deserialize)]
struct Episode {
    start: BucketIx,
    last: BucketIx,
    opened: bool,
    suppressed: bool,
    message: String,
}
/// Persistable state, with a separate document result set for historical/dry runs.
#[derive(Serialize, Deserialize)]
pub struct RuleRunner {
    rules: Vec<AlertRule>,
    episodes: Vec<BTreeMap<String, Episode>>,
    hourly: Vec<BTreeMap<i64, u32>>,
    seen: BTreeMap<String, BucketIx>,
    #[serde(skip)]
    documents: BTreeMap<(String, String), Document>,
}
impl RuleRunner {
    pub fn new(rules: Vec<AlertRule>) -> Result<Self> {
        ti_contracts::TiConfig { rules: rules.clone(), ..Default::default() }
            .validate().map_err(core_error)?;
        for rule in &rules {
            expression(&rule.when)?;
            placeholders(&rule.message)?;
        }
        let count = rules.len();
        Ok(Self { rules, episodes: vec![BTreeMap::new(); count],
            hourly: vec![BTreeMap::new(); count], seen: BTreeMap::new(), documents: BTreeMap::new() })
    }
    pub fn rules(&self) -> &[AlertRule] { &self.rules }
    pub fn is_fresh(&self) -> bool { self.seen.is_empty() }
    pub fn documents(&self) -> Vec<Document> { self.documents.values().cloned().collect() }
    pub fn load(root: &Path, rules: Vec<AlertRule>) -> Result<Self> {
        let path = root.join("rules/state.json");
        if path.exists() {
            let state: Self = serde_json::from_slice(&std::fs::read(path)?)
                .map_err(|e| error(format!("rules state: {e}")))?;
            if state.rules == rules { return Ok(state); }
        }
        Self::new(rules)
    }
    pub fn save(&self, root: &Path) -> Result<()> {
        let dir = root.join("rules");
        std::fs::create_dir_all(&dir)?;
        let temporary = dir.join("state.json.new");
        let bytes = serde_json::to_vec(self).map_err(|e| error(e.to_string()))?;
        let mut file = std::fs::File::create(&temporary)?;
        std::io::Write::write_all(&mut file, &bytes)?;
        file.sync_all()?;
        drop(file);
        let destination = dir.join("state.json");
        std::fs::rename(temporary, destination)?;
        #[cfg(unix)]
        if let Ok(directory) = std::fs::File::open(&dir) { directory.sync_all()?; }
        Ok(())
    }
    async fn plans(&self, session: &SqlSession, scope: Option<(BucketIx, BucketIx)>) -> Result<Vec<TelemetryExec>> {
        let mut plans = Vec::new();
        for rule in &self.rules {
            for column in placeholders(&rule.message)? {
                if session.catalog.field(&column).is_none() {
                    return Err(error(format!("rule {}: unknown message column {column}", rule.name)));
                }
            }
            let vessel = rule.vessel.as_deref().map(|v| format!(" AND vessel = {}", quote(v))).unwrap_or_default();
            let range = if let Some((from, to)) = scope {
                let start = timestamp(u64::from(from), session.catalog.width_seconds)?;
                let end = timestamp(u64::from(to) + 1, session.catalog.width_seconds)?;
                let convert = |ts| datafusion::arrow::temporal_conversions::timestamp_s_to_datetime(ts)
                    .ok_or_else(|| error("rule range timestamp is out of range"));
                format!(" AND ts >= TIMESTAMP '{}' AND ts < TIMESTAMP '{}'", convert(start)?, convert(end)?)
            } else { String::new() };
            let sql = format!("SELECT vessel, ts FROM telemetry WHERE ({}){vessel}{range}", expression(&rule.when)?);
            let plan = session.prepare(&sql).await?.create_physical_plan().await?;
            plans.push(crate::intervals::bitmap_scan(&plan).ok_or_else(|| {
                error(format!("rule {} requires an exact bitmap predicate (residual SQL is unsupported)", rule.name))
            })?);
        }
        Ok(plans)
    }
    fn document(&self, rule: usize, vessel: &str, episode: &Episode, end: Option<i64>, width: u64) -> Result<Document> {
        let rule = &self.rules[rule];
        let start = timestamp(u64::from(episode.start), width)?;
        Ok(Document {
            id: format!("rules/{}/{start}", rule.name), vessel: vessel.into(), kind: "alerts".into(),
            ts_start: start, ts_end: end, title: format!("{} ({})", rule.name, rule.severity),
            body: format!("rule: {}\n{}\nseverity: {}\nwhen: {}", rule.name, episode.message, rule.severity, rule.when),
        })
    }
    fn publish(&mut self, index: &dyn DocumentIndex, document: Document) -> Result<()> {
        index.upsert(&document).map_err(core_error)?;
        self.documents.insert((document.vessel.clone(), document.id.clone()), document);
        Ok(())
    }
    async fn message(&self, rule: usize, session: &SqlSession, vessel: &str, bucket: BucketIx) -> Result<String> {
        let template = &self.rules[rule].message;
        let columns = placeholders(template)?;
        if columns.is_empty() { return Ok(template.clone()); }
        let start = timestamp(u64::from(bucket), session.catalog.width_seconds)?;
        let time = datafusion::arrow::temporal_conversions::timestamp_s_to_datetime(start)
            .ok_or_else(|| error("rule message timestamp is out of range"))?;
        let sql = format!("SELECT {} FROM telemetry WHERE vessel = {} AND ts = TIMESTAMP '{}'",
            columns.iter().map(|c| identifier(c)).collect::<Vec<_>>().join(", "), quote(vessel), time);
        let rows = crate::rows_json(&session.query(&sql).await?)?;
        let row = rows.first().ok_or_else(|| error("rule message bucket disappeared"))?;
        let mut message = template.clone();
        for column in columns {
            let value = row.get(&column).or_else(|| {
                session.catalog.field(&column).and_then(|f| row.get(&crate::field_name(f)))
            }).unwrap_or(&serde_json::Value::Null);
            let rendered = match value { serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Null => "NULL".into(), _ => value.to_string() };
            message = message.replace(&format!("{{{column}}}"), &rendered);
        }
        Ok(message)
    }
    async fn bucket(
        &mut self, session: &SqlSession, index: &dyn DocumentIndex,
        plans: &[TelemetryExec], static_hits: Option<&[Option<RoaringTreemap>]>,
        vessel: VesselOrd, bucket: BucketIx,
    ) -> Result<()> {
        let urn = session.catalog.vessels.get(&vessel).ok_or_else(|| error("rule vessel missing"))?.urn.clone();
        if self.seen.get(&urn).is_some_and(|last| *last >= bucket) { return Ok(()); }
        let width = session.catalog.width_seconds;
        for (rule_number, plan) in plans.iter().enumerate() {
            let mut episode = self.episodes[rule_number].remove(&urn);
            if episode.as_ref().is_some_and(|e| u64::from(e.last) + 1 != u64::from(bucket)) {
                if let Some(old) = episode.take().filter(|e| e.opened) {
                    let end = timestamp(u64::from(old.last) + 1, width)?;
                    self.publish(index, self.document(rule_number, &urn, &old, Some(end), width)?)?;
                }
            }
            let matches = if let Some(hits) = static_hits.and_then(|all| all[rule_number].as_ref()) {
                hits.contains((u64::from(vessel) << 32) | u64::from(bucket))
            } else {
                let key = ti_contracts::shard_key(vessel, bucket);
                plan.keys.contains(&key) && plan.rule_bitmap(key)?.contains(ti_contracts::local_col(bucket))
            };
            if matches {
                let mut current = episode.unwrap_or(Episode { start: bucket, last: bucket,
                    opened: false, suppressed: false, message: String::new() });
                current.last = bucket;
                let elapsed = i128::from(u64::from(bucket) + 1 - u64::from(current.start))
                    * i128::from(width) * 1_000_000_000;
                if !current.opened && !current.suppressed
                    && elapsed >= self.rules[rule_number].hold_nanoseconds().map_err(core_error)? {
                    let hour = timestamp(u64::from(bucket), width)?.div_euclid(3600);
                    let count = self.hourly[rule_number].entry(hour).or_default();
                    if *count >= self.rules[rule_number].max_per_hour {
                        current.suppressed = true;
                    } else {
                        *count += 1;
                        current.message = self.message(rule_number, session, &urn, bucket).await?;
                        current.opened = true;
                    }
                }
                if current.opened {
                    self.publish(index, self.document(rule_number, &urn, &current, None, width)?)?;
                }
                self.episodes[rule_number].insert(urn.clone(), current);
            } else if let Some(old) = episode.filter(|e| e.opened) {
                let end = timestamp(u64::from(old.last) + 1, width)?;
                self.publish(index, self.document(rule_number, &urn, &old, Some(end), width)?)?;
            }
        }
        self.seen.insert(urn, bucket);
        Ok(())
    }
    /// Incremental inclusive range, evaluated once per bucket in configuration order.
    pub async fn on_closed(
        &mut self, session: &SqlSession, index: &dyn DocumentIndex,
        vessel: VesselOrd, from: BucketIx, to: BucketIx,
    ) -> Result<()> {
        if from > to { return Err(error("closed bucket range is reversed")); }
        let plans = self.plans(session, Some((from, to))).await?;
        for bucket in from..=to {
            self.bucket(session, index, &plans, None, vessel, bucket).await?;
        }
        Ok(())
    }
    /// Full history, with ordinary predicates evaluated once as compressed bitmaps.
    /// Text predicates stay dynamic so earlier rules' documents affect later rules.
    pub async fn history(&mut self, session: &SqlSession, index: &dyn DocumentIndex) -> Result<()> {
        let plans = self.plans(session, None).await?;
        let mut hits = Vec::new();
        for (rule, plan) in self.rules.iter().zip(&plans) {
            if rule.when.to_ascii_lowercase().contains("match") {
                hits.push(None);
            } else {
                let mut all = RoaringTreemap::new();
                for key in &plan.keys {
                    for col in plan.rule_bitmap(*key)? {
                        all.insert((u64::from(key.vessel) << 32) | ((u64::from(key.shard) << 16) + u64::from(col)));
                    }
                }
                hits.push(Some(all));
            }
        }
        let mut buckets = Vec::new();
        for key in session.source.shards(None, 0, u32::MAX) {
            for col in session.source.eval(key, &Predicate::All).map_err(core_error)? {
                buckets.push(((key.shard << 16) | col, key.vessel));
            }
        }
        buckets.sort_unstable();
        for (bucket, vessel) in buckets {
            self.bucket(session, index, &plans, Some(&hits), vessel, bucket).await?;
        }
        // A historical snapshot is closed at its last observed bucket boundary.
        // Keep episode state so a subsequent adjacent live bucket retains its ID.
        let width = session.catalog.width_seconds;
        for number in 0..self.rules.len() {
            let active: Vec<_> = self.episodes[number].iter().filter(|(_, e)| e.opened)
                .map(|(v, e)| (v.clone(), e.clone())).collect();
            for (vessel, episode) in active {
                let end = timestamp(u64::from(episode.last) + 1, width)?;
                self.publish(index, self.document(number, &vessel, &episode, Some(end), width)?)?;
            }
        }
        Ok(())
    }
}

/// Open a SQL snapshot and a shared document index for this evaluation pass.
pub async fn open_session(
    root: &Path, width: u64, builder: &RuleIndexFactory, documents: DocStore,
) -> Result<(SqlSession, Arc<dyn DocumentIndex>)> {
    let store = Store::open_or_create(root, width).map_err(core_error)?;
    let catalog: Arc<dyn Catalog> = store.catalog().clone();
    let index = builder(documents, catalog, width).map_err(core_error)?;
    let text: Arc<dyn ti_contracts::TextIndex> = index.clone();
    let session = crate::session_from_store(Arc::new(store.with_text_index(text)), width).await?;
    session.register_documents(index.clone())?;
    Ok((session, index))
}

/// Evaluate complete history. Dry runs never alter the durable documents or state.
pub async fn backfill(
    root: &Path, width: u64, rules: Vec<AlertRule>, builder: &RuleIndexFactory, dry_run: bool,
) -> Result<Vec<Document>> {
    let existing = DocStore::open(root).map_err(core_error)?;
    let prefixes: Vec<_> = rules.iter().map(|r| format!("rules/{}/", r.name)).collect();
    let own = |d: &Document| prefixes.iter().any(|p| d.id.starts_with(p));
    let mut snapshot = DocStore::in_memory();
    snapshot.upsert_all(existing.iter().filter(|d| !own(d)).cloned()).map_err(core_error)?;
    let (session, index) = open_session(root, width, builder, snapshot).await?;
    let mut runner = RuleRunner::new(rules)?;
    runner.history(&session, index.as_ref()).await?;
    let documents = runner.documents();
    if !dry_run {
        let mut durable = DocStore::open(root).map_err(core_error)?;
        let identities: BTreeSet<_> = documents.iter().map(|d| (d.vessel.clone(), d.id.clone())).collect();
        let obsolete: Vec<_> = durable.iter().filter(|d| own(d) && !identities.contains(&(d.vessel.clone(), d.id.clone())))
            .map(|d| (d.vessel.clone(), d.id.clone())).collect();
        durable.upsert_all(documents.clone()).map_err(core_error)?;
        for (vessel, id) in obsolete { durable.delete(&vessel, &id).map_err(core_error)?; }
        runner.save(root)?;
    }
    Ok(documents)
}

/// Fresh durable alert status, including externally ingested notification updates.
pub fn alert_status(root: &Path) -> Result<serde_json::Value> {
    let store = DocStore::open(root).map_err(core_error)?;
    let alerts: Vec<_> = store.iter().filter(|d| d.kind == "alerts").collect();
    let mut active: Vec<_> = alerts.iter().copied()
        .filter(|d| d.ts_end.is_none() && !ti_ingest::notifications::is_closed_point(d)).collect();
    active.sort_by_key(|d| std::cmp::Reverse(d.ts_start));
    let total = active.len();
    let mut summaries = Vec::new();
    let mut bytes = 0;
    for doc in active {
        let summary = serde_json::json!({"id":doc.id,"vessel":doc.vessel,"ts_start":doc.ts_start,
            "title":doc.title.chars().take(200).collect::<String>()});
        let size = serde_json::to_vec(&summary).map_err(|e| error(e.to_string()))?.len();
        if summaries.len() == crate::MAX_ROWS || bytes + size > 16 * 1024 { break; }
        bytes += size;
        summaries.push(summary);
    }
    Ok(serde_json::json!({"document_count":store.len(),"alert_count":alerts.len(),
        "active_alert_count":total,"active_alerts_truncated":summaries.len() < total,"active_alerts":summaries}))
}
