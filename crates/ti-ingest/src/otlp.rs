//! Hand-written OTLP/HTTP JSON subset. No protobuf runtime or exporter dependency.
use crate::{normalize::NormalizedValue, WatermarkBucketer};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
    time::Instant,
};
use ti_contracts::{Catalog, Document, Error, Result, ShardSink, TiConfig};
use ti_store::{DocStore, Store};

pub const STORE_DIR: &str = "stores/agents";
pub const MAX_BODY: usize = 8 * 1024 * 1024;
pub const RETENTION_SECONDS: u64 = 90 * 86400;

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Resource {
    #[serde(default)]
    attributes: Vec<Attribute>,
}
#[derive(Deserialize)]
struct Attribute {
    key: String,
    value: Value,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MetricsRequest {
    resource_metrics: Vec<ResourceMetrics>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResourceMetrics {
    #[serde(default)]
    resource: Resource,
    #[serde(default)]
    scope_metrics: Vec<ScopeMetrics>,
}
#[derive(Deserialize)]
struct ScopeMetrics {
    #[serde(default)]
    metrics: Vec<Metric>,
}
#[derive(Deserialize)]
struct Metric {
    name: String,
    #[serde(default)]
    unit: String,
    gauge: Option<Points>,
    sum: Option<Points>,
    histogram: Option<Histograms>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Points {
    #[serde(default)]
    is_monotonic: bool,
    aggregation_temporality: Option<u32>,
    #[serde(default)]
    data_points: Vec<Point>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Point {
    time_unix_nano: Value,
    start_time_unix_nano: Option<Value>,
    #[serde(default)]
    attributes: Vec<Attribute>,
    as_double: Option<f64>,
    as_int: Option<Value>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Histograms {
    #[serde(default)]
    data_points: Vec<Histogram>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Histogram {
    time_unix_nano: Value,
    #[serde(default)]
    attributes: Vec<Attribute>,
    sum: Option<f64>,
    count: Value,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LogsRequest {
    resource_logs: Vec<ResourceLogs>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResourceLogs {
    #[serde(default)]
    resource: Resource,
    #[serde(default)]
    scope_logs: Vec<ScopeLogs>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ScopeLogs {
    #[serde(default)]
    log_records: Vec<LogRecord>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LogRecord {
    time_unix_nano: Option<Value>,
    observed_time_unix_nano: Option<Value>,
    #[serde(default)]
    event_name: String,
    #[serde(default)]
    attributes: Vec<Attribute>,
    body: Option<Value>,
}
fn invalid(message: impl Into<String>) -> Error {
    Error::InvalidInput(message.into())
}
fn parse<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    if bytes.len() > MAX_BODY {
        return Err(invalid("OTLP body exceeds 8 MiB"));
    }
    serde_json::from_slice(bytes).map_err(|e| invalid(format!("Invalid OTLP JSON: {e}")))
}
fn unsigned(v: &Value) -> Result<u64> {
    v.as_u64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .ok_or_else(|| invalid("OTLP integer must be an unsigned decimal string or integer"))
}
fn timestamp(v: &Value) -> Result<i64> {
    let ts = i64::try_from(unsigned(v)? / 1_000_000_000)
        .map_err(|_| invalid("OTLP timestamp overflow"))?;
    ti_contracts::bucket_of(ts, 10)?;
    Ok(ts)
}
fn number(v: &Value) -> Result<f64> {
    let n = v
        .as_f64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .ok_or_else(|| invalid("Invalid OTLP numeric value"))?;
    if !n.is_finite() {
        return Err(invalid("OTLP numeric value must be finite"));
    }
    Ok(n)
}
fn attributes(attrs: &[Attribute]) -> BTreeMap<String, Value> {
    attrs
        .iter()
        .map(|a| {
            let v = a
                .value
                .as_object()
                .and_then(|o| o.values().next())
                .cloned()
                .unwrap_or_else(|| a.value.clone());
            (a.key.clone(), v)
        })
        .collect()
}
fn entity(resource: &Resource) -> Result<String> {
    let attrs = attributes(&resource.attributes);
    let id = ["service.instance.id", "pane", "pane.id", "service.name"]
        .iter()
        .find_map(|key| {
            attrs
                .get(*key)
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
        })
        .ok_or_else(|| {
            invalid("OTLP resource requires service.instance.id, pane, or service.name")
        })?;
    let urn = format!("agent.urn:{id}");
    ti_contracts::validate_entity_urn(&urn)?;
    Ok(urn)
}
// Attribute components are percent-escaped so different dimension sets cannot alias.
fn component(value: &str) -> String {
    let mut out = String::new();
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || b == b'_' || b == b'-' {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
fn metric_path(name: &str, attrs: &[Attribute]) -> String {
    let mut path = name.to_string();
    let mut attrs = attributes(attrs);
    if let Some(value) = attrs.remove("type") {
        let value = value
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| value.to_string());
        path.push_str(&format!(".{}", component(&value)));
    }
    for (key, value) in attrs {
        let value = value
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| value.to_string());
        if key == "type" {
            path.push_str(&format!(".{}", component(&value)));
        } else {
            path.push_str(&format!(".{}.{}", component(&key), component(&value)));
        }
    }
    path
}
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CounterState {
    total: f64,
    raw: f64,
    start: u64,
    offset: f64,
    end: u64,
}
const MAX_COUNTERS: usize = 4096;
const MAX_CHECKPOINT: u64 = 8 * 1024 * 1024;
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CounterCheckpoint {
    version: u32,
    counters: Vec<CounterEntry>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CounterEntry {
    entity: String,
    path: String,
    state: CounterState,
}
fn load_counters(path: &Path) -> Result<BTreeMap<(String, String), CounterState>> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(e.into()),
    };
    let corrupt = |reason: String| {
        invalid(format!(
            "Invalid OTLP counters file {}: {reason}; refusing to reset totals",
            path.display()
        ))
    };
    if file.metadata()?.len() > MAX_CHECKPOINT {
        return Err(corrupt("exceeds 8 MiB".into()));
    }
    let checkpoint: CounterCheckpoint =
        serde_json::from_reader(file).map_err(|e| corrupt(e.to_string()))?;
    if checkpoint.version != 1 || checkpoint.counters.len() > MAX_COUNTERS {
        return Err(corrupt(
            "unsupported version or more than 4096 series".into(),
        ));
    }
    let mut totals = BTreeMap::new();
    for entry in checkpoint.counters {
        let state = &entry.state;
        if !entry.entity.starts_with("agent.urn:")
            || ti_contracts::validate_entity_urn(&entry.entity).is_err()
            || entry.path.trim().is_empty()
            || [state.total, state.raw, state.offset]
                .iter()
                .any(|n| !n.is_finite() || *n < 0.0)
            || state.offset > state.total
            || ti_contracts::to_fixed(state.total, 6).is_err()
            || (state.end != 0
                && ti_contracts::bucket_of((state.end / 1_000_000_000) as i64, 10).is_err())
            || totals
                .insert((entry.entity, entry.path), entry.state)
                .is_some()
        {
            return Err(corrupt("invalid or duplicate counter series".into()));
        }
    }
    Ok(totals)
}
fn persist_counters(path: &Path, totals: &BTreeMap<(String, String), CounterState>) -> Result<()> {
    use std::io::Write;
    let checkpoint = CounterCheckpoint {
        version: 1,
        counters: totals
            .iter()
            .map(|((entity, path), state)| CounterEntry {
                entity: entity.clone(),
                path: path.clone(),
                state: state.clone(),
            })
            .collect(),
    };
    let bytes = serde_json::to_vec(&checkpoint).map_err(|e| invalid(e.to_string()))?;
    if totals.len() > MAX_COUNTERS || bytes.len() as u64 > MAX_CHECKPOINT {
        return Err(invalid(
            "OTLP counters checkpoint exceeds its bounded capacity",
        ));
    }
    let temp = path.with_extension("json.tmp");
    let result = (|| -> Result<()> {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, path)?;
        #[cfg(unix)]
        std::fs::File::open(path.parent().expect("checkpoint has a parent"))?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}
struct CounterPoint {
    temporality: u32,
    start: u64,
    end: u64,
}
fn counter_value(state: &mut CounterState, point: &CounterPoint, value: f64) -> Result<f64> {
    if value < 0.0 {
        return Err(invalid("Monotonic OTLP sums must be nonnegative"));
    }
    if point.end < state.end {
        return Err(invalid("Out-of-order monotonic OTLP sum"));
    }
    // Export retries with the same event timestamp/value are idempotent.
    if point.end == state.end && point.start == state.start && value == state.raw {
        return Ok(state.total);
    }
    match point.temporality {
        1 => state.total += value,
        2 => {
            if state.end != 0 && point.start != state.start {
                state.offset = state.total;
            } else if state.end != 0 && value < state.raw {
                return Err(invalid(
                    "Cumulative OTLP sum decreased without startTimeUnixNano reset",
                ));
            }
            state.total = state.offset + value;
        }
        _ => {
            return Err(invalid(
                "Monotonic sums require aggregationTemporality 1 (delta) or 2 (cumulative)",
            ))
        }
    }
    if !state.total.is_finite() {
        return Err(invalid("OTLP running total overflow"));
    }
    state.raw = value;
    state.start = point.start;
    state.end = point.end;
    Ok(state.total)
}
struct Sample {
    entity: String,
    path: String,
    unit: String,
    ts: i64,
    value: f64,
    counter: Option<CounterPoint>,
}

/// One receiver owns its bucketer. The idle caller leads and flushes immediately.
/// Batches that arrive during that flush stage, and the next leader takes at most
/// [`GROUP_LIMIT`] of them. A caller returns only after its group's fsync.
pub struct AgentStore {
    durable: Mutex<Durable>,
    admit: Mutex<Admit>,
    cv: Condvar,
    #[cfg(test)]
    pause: Mutex<Option<Arc<TestPause>>>,
}

struct Durable {
    store: Store,
    catalog: Arc<dyn Catalog>,
    bucketer: WatermarkBucketer,
    config: TiConfig,
    docs_root: PathBuf,
    totals: BTreeMap<(String, String), CounterState>,
}

struct Admit {
    running: bool,
    staged: VecDeque<Arc<Batch>>,
    by_id: HashMap<[u8; 32], Arc<Batch>>,
    seen: VecDeque<[u8; 32]>,
    /// Counter totals including staged metrics not yet checkpointed.
    shadow: BTreeMap<(String, String), CounterState>,
    commits: u64,
    max_group: usize,
}

struct Batch {
    id: [u8; 32],
    metrics: bool,
    body: Mutex<Option<BatchBody>>,
    slot: Mutex<Option<std::result::Result<usize, String>>>,
    slot_cv: Condvar,
}

enum BatchBody {
    Metrics(PreparedMetrics),
    Logs(PreparedLogs),
}

struct PreparedMetrics {
    samples: Vec<Sample>,
    latest: Option<i64>,
    totals_after: BTreeMap<(String, String), CounterState>,
}

struct PreparedLogs {
    docs: Vec<Document>,
    latest: Option<i64>,
}

const GROUP_LIMIT: usize = 32;

#[cfg(test)]
struct TestPause {
    hold: Mutex<bool>,
    cv: Condvar,
    tripped: std::sync::atomic::AtomicBool,
}

impl AgentStore {
    pub fn open(root: &Path) -> Result<Self> {
        let durable = Durable::load(root)?;
        let shadow = durable.totals.clone();
        Ok(Self {
            durable: Mutex::new(durable),
            admit: Mutex::new(Admit {
                running: false,
                staged: VecDeque::new(),
                by_id: HashMap::new(),
                seen: VecDeque::new(),
                shadow,
                commits: 0,
                max_group: 0,
            }),
            cv: Condvar::new(),
            #[cfg(test)]
            pause: Mutex::new(None),
        })
    }

    /// Hold the store lock across a query-engine reload so it does not observe a flush.
    pub fn with_durable_lock<T>(&self, f: impl FnOnce() -> T) -> std::result::Result<T, String> {
        let _guard = self.durable.lock().map_err(|e| e.to_string())?;
        Ok(f())
    }

    pub fn metrics(&self, bytes: &[u8]) -> Result<usize> {
        self.submit(bytes, true)
    }

    pub fn logs(&self, bytes: &[u8]) -> Result<usize> {
        self.submit(bytes, false)
    }

    fn submit(&self, bytes: &[u8], metrics: bool) -> Result<usize> {
        let batch_id = *blake3::hash(bytes).as_bytes();
        let mut admit = self.admit.lock().map_err(poison)?;
        if metrics && admit.seen.contains(&batch_id) {
            return Ok(0);
        }
        if let Some(existing) = admit.by_id.get(&batch_id) {
            let existing = Arc::clone(existing);
            drop(admit);
            existing.wait_result()?;
            return Ok(0);
        }
        let body = if metrics {
            let mut trial = admit.shadow.clone();
            let prepared = prepare_metrics(bytes, &mut trial)?;
            admit.shadow = trial;
            BatchBody::Metrics(prepared)
        } else {
            BatchBody::Logs(prepare_logs(bytes)?)
        };
        let batch = Arc::new(Batch {
            id: batch_id,
            metrics,
            body: Mutex::new(Some(body)),
            slot: Mutex::new(None),
            slot_cv: Condvar::new(),
        });
        admit.by_id.insert(batch_id, Arc::clone(&batch));
        admit.staged.push_back(Arc::clone(&batch));
        let mine = Arc::clone(&batch);
        loop {
            if mine.ready() {
                drop(admit);
                return mine.wait_result();
            }
            if !admit.running {
                admit.running = true;
                let mut group = Vec::new();
                while group.len() < GROUP_LIMIT {
                    let Some(next) = admit.staged.pop_front() else {
                        break;
                    };
                    group.push(next);
                }
                admit.max_group = admit.max_group.max(group.len());
                drop(admit);
                let flushed = self.flush_group(&group);
                // Recover a poisoned admit lock so this leader still publishes every slot.
                // Leaving `running` set would park every later batch on the condvar.
                admit = match self.admit.lock() {
                    Ok(guard) => guard,
                    Err(poisoned) => poisoned.into_inner(),
                };
                match flushed {
                    Ok(counts) => {
                        for (id, count) in counts {
                            if let Some(done) = admit.by_id.remove(&id) {
                                if done.metrics {
                                    admit.note_seen(id);
                                }
                                done.finish(Ok(count));
                            }
                        }
                        admit.commits = admit.commits.saturating_add(1);
                    }
                    Err(error) => {
                        let reload = self.reload_durable();
                        let mut message = match &reload {
                            Ok(()) => error,
                            Err(reload_error) => format!("{error}; reload: {reload_error}"),
                        };
                        if reload.is_ok() {
                            match self.durable.lock() {
                                Ok(guard) => admit.shadow = guard.totals.clone(),
                                Err(poisoned) => {
                                    let guard = poisoned.into_inner();
                                    admit.shadow = guard.totals.clone();
                                    message.push_str("; durable lock poisoned");
                                }
                            }
                        }
                        let rest: Vec<_> = admit.staged.drain(..).collect();
                        for batch in group.into_iter().chain(rest) {
                            admit.by_id.remove(&batch.id);
                            batch.finish(Err(message.clone()));
                        }
                    }
                }
                admit.running = false;
                self.cv.notify_all();
                continue;
            }
            admit = self.cv.wait(admit).map_err(poison)?;
        }
    }

    fn flush_group(
        &self,
        group: &[Arc<Batch>],
    ) -> std::result::Result<Vec<([u8; 32], usize)>, String> {
        #[cfg(test)]
        self.pause_if_armed();
        let mut durable = self.durable.lock().map_err(|e| e.to_string())?;
        let mut counts = Vec::with_capacity(group.len());
        let mut last_totals = None;
        let mut latest_metric = None;
        let mut log_docs = Vec::new();
        let mut latest_log = None;
        for batch in group {
            let body = batch
                .body
                .lock()
                .map_err(|e| e.to_string())?
                .take()
                .ok_or_else(|| "OTLP batch body missing before flush".to_string())?;
            match body {
                BatchBody::Metrics(prepared) => {
                    let count = prepared.samples.len();
                    let latest = prepared.latest;
                    apply_metrics(&mut durable, &prepared).map_err(|e| e.to_string())?;
                    last_totals = Some(prepared.totals_after);
                    if let Some(ts) = latest {
                        latest_metric = Some(latest_metric.map_or(ts, |prev: i64| prev.max(ts)));
                    }
                    counts.push((batch.id, count));
                }
                BatchBody::Logs(prepared) => {
                    let count = prepared.docs.len();
                    if let Some(ts) = prepared.latest {
                        latest_log = Some(latest_log.map_or(ts, |prev: i64| prev.max(ts)));
                    }
                    log_docs.extend(prepared.docs);
                    counts.push((batch.id, count));
                }
            }
        }
        if !log_docs.is_empty() {
            commit_logs(&durable.docs_root, log_docs, latest_log)?;
        }
        if let Some(totals) = last_totals {
            {
                let Durable {
                    bucketer,
                    config,
                    catalog,
                    store,
                    ..
                } = &mut *durable;
                bucketer
                    .flush_all(config, catalog.as_ref(), store)
                    .map_err(|e| e.to_string())?;
            }
            durable.store.flush().map_err(|e| e.to_string())?;
            if let Some(ts) = latest_metric {
                seal_and_retain(&mut durable, ts)?;
            }
            // Checkpoint is the publish point: both writes above have returned.
            persist_counters(
                &durable.docs_root.join(STORE_DIR).join("otlp-counters.json"),
                &totals,
            )
            .map_err(|e| e.to_string())?;
            durable.totals = totals;
        }
        Ok(counts)
    }

    fn reload_durable(&self) -> Result<()> {
        let root = self.durable.lock().map_err(poison)?.docs_root.clone();
        let fresh = Durable::load(&root)?;
        *self.durable.lock().map_err(poison)? = fresh;
        Ok(())
    }

    #[cfg(test)]
    fn pause_if_armed(&self) {
        let installed = self.pause.lock().unwrap().clone();
        let Some(pause) = installed else {
            return;
        };
        if pause
            .tripped
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return;
        }
        let mut hold = pause.hold.lock().unwrap();
        while *hold {
            hold = pause.cv.wait(hold).unwrap();
        }
    }

    #[cfg(test)]
    fn commit_stats(&self) -> (u64, usize) {
        let admit = self.admit.lock().unwrap();
        (admit.commits, admit.max_group)
    }
}

impl Durable {
    fn load(root: &Path) -> Result<Self> {
        let dir = root.join(STORE_DIR);
        let totals = load_counters(&dir.join("otlp-counters.json"))?;
        let mut config = TiConfig::default();
        config.profiles.opt_in.push("last".into());
        config.path_scales.clear();
        for scale in config.unit_scales.values_mut() {
            *scale = 6;
        }
        if !root.join("ti.toml").exists() {
            std::fs::create_dir_all(root)?;
            std::fs::write(root.join("ti.toml"), "width_seconds = 10\n")?;
        }
        let store = Store::open_or_create(&dir, 10)?;
        let catalog: Arc<dyn Catalog> = store.catalog().clone();
        if !dir.join("ti.toml").exists() {
            std::fs::write(
                dir.join("ti.toml"),
                "width_seconds = 10\n[profiles]\nopt_in = ['last']\n[stores.default]\nwidth = '10s'\nretention = '90d'\n",
            )?;
        }
        let mut bucketer = WatermarkBucketer::new(&config);
        bucketer.retain_numeric_snapshots();
        for ((entity, path), state) in &totals {
            if state.end != 0 {
                let vessel = catalog.register_vessel(&ti_contracts::VesselSpec {
                    urn: entity.clone(),
                    name: None,
                    mmsi: None,
                })?;
                bucketer.restore_counter_observation(
                    vessel,
                    path,
                    (state.end / 1_000_000_000) as i64,
                    state.total,
                )?;
            }
        }
        Ok(Self {
            store,
            catalog,
            bucketer,
            config,
            docs_root: root.into(),
            totals,
        })
    }
}

impl Admit {
    fn note_seen(&mut self, id: [u8; 32]) {
        self.seen.push_back(id);
        while self.seen.len() > 128 {
            self.seen.pop_front();
        }
    }
}

impl Batch {
    fn ready(&self) -> bool {
        self.slot.lock().map(|slot| slot.is_some()).unwrap_or(true)
    }

    fn finish(&self, result: std::result::Result<usize, String>) {
        if let Ok(mut slot) = self.slot.lock() {
            if slot.is_none() {
                *slot = Some(result);
            }
        }
        self.slot_cv.notify_all();
    }

    fn wait_result(&self) -> Result<usize> {
        let mut slot = self.slot.lock().map_err(poison)?;
        while slot.is_none() {
            slot = self.slot_cv.wait(slot).map_err(poison)?;
        }
        match slot.clone().expect("slot is filled") {
            Ok(count) => Ok(count),
            Err(error) => Err(Error::Io(std::io::Error::other(error))),
        }
    }
}

fn poison<T>(err: std::sync::PoisonError<T>) -> Error {
    Error::Io(std::io::Error::other(err.to_string()))
}

fn prepare_metrics(
    bytes: &[u8],
    totals: &mut BTreeMap<(String, String), CounterState>,
) -> Result<PreparedMetrics> {
    let request: MetricsRequest = parse(bytes)?;
    let mut samples = Vec::new();
    for resource in request.resource_metrics {
        let entity = entity(&resource.resource)?;
        for scope in resource.scope_metrics {
            for metric in scope.metrics {
                if metric.name.trim().is_empty() {
                    return Err(invalid("OTLP metric name is empty"));
                }
                let types = usize::from(metric.gauge.is_some())
                    + usize::from(metric.sum.is_some())
                    + usize::from(metric.histogram.is_some());
                if types != 1 {
                    return Err(invalid(
                        "Supported OTLP metrics require exactly one gauge, sum, or histogram",
                    ));
                }
                let monotonic = metric.sum.as_ref().is_some_and(|p| p.is_monotonic);
                if let Some(points) = metric.gauge.or(metric.sum) {
                    for p in points.data_points {
                        let value = match (p.as_double, p.as_int) {
                            (Some(n), None) if n.is_finite() => n,
                            (None, Some(n)) => number(&n)?,
                            _ => {
                                return Err(invalid(
                                    "OTLP point requires one finite asDouble or asInt",
                                ))
                            }
                        };
                        samples.push(Sample {
                            entity: entity.clone(),
                            path: metric_path(&metric.name, &p.attributes),
                            unit: metric.unit.clone(),
                            ts: timestamp(&p.time_unix_nano)?,
                            value,
                            counter: if monotonic {
                                Some(CounterPoint {
                                    temporality: points.aggregation_temporality.unwrap_or(0),
                                    start: p
                                        .start_time_unix_nano
                                        .as_ref()
                                        .map(unsigned)
                                        .transpose()?
                                        .unwrap_or(0),
                                    end: unsigned(&p.time_unix_nano)?,
                                })
                            } else {
                                None
                            },
                        });
                    }
                } else if let Some(histogram) = metric.histogram {
                    for p in histogram.data_points {
                        let ts = timestamp(&p.time_unix_nano)?;
                        let count = unsigned(&p.count)? as f64;
                        if let Some(sum) = p.sum {
                            if !sum.is_finite() {
                                return Err(invalid("OTLP histogram sum must be finite"));
                            }
                            samples.push(Sample {
                                entity: entity.clone(),
                                path: format!("{}.sum", metric_path(&metric.name, &p.attributes)),
                                unit: metric.unit.clone(),
                                ts,
                                value: sum,
                                counter: None,
                            });
                        }
                        samples.push(Sample {
                            entity: entity.clone(),
                            path: format!("{}.count", metric_path(&metric.name, &p.attributes)),
                            unit: "{count}".into(),
                            ts,
                            value: count,
                            counter: None,
                        });
                    }
                }
            }
        }
    }
    samples.sort_by_key(|sample| {
        sample
            .counter
            .as_ref()
            .map_or(sample.ts as u64 * 1_000_000_000, |p| p.end)
    });
    for sample in &mut samples {
        if let Some(point) = &sample.counter {
            let key = (sample.entity.clone(), sample.path.clone());
            if !totals.contains_key(&key) && totals.len() >= MAX_COUNTERS {
                return Err(invalid("OTLP counter series limit (4096) reached"));
            }
            sample.value = counter_value(totals.entry(key).or_default(), point, sample.value)?;
        }
        ti_contracts::to_fixed(sample.value, 6)
            .map_err(|e| invalid(format!("OTLP point out of fixed-point range: {e}")))?;
    }
    let latest = samples.iter().map(|s| s.ts).max();
    Ok(PreparedMetrics {
        samples,
        latest,
        totals_after: totals.clone(),
    })
}

fn apply_metrics(durable: &mut Durable, prepared: &PreparedMetrics) -> Result<()> {
    for sample in &prepared.samples {
        if !sample.unit.is_empty() {
            durable
                .bucketer
                .classifier_mut()
                .register_meta_units(&sample.path, &sample.unit);
        }
        durable.config.units.insert(
            sample.path.clone(),
            ti_contracts::MetricUnit {
                unit: sample.unit.clone(),
                scale: 6,
            },
        );
        durable.bucketer.ingest_point(
            &sample.entity,
            &sample.path,
            "otlp",
            sample.ts,
            NormalizedValue::Double(sample.value),
            &durable.config,
            durable.catalog.as_ref(),
            &mut durable.store,
        )?;
    }
    Ok(())
}

fn seal_and_retain(durable: &mut Durable, ts: i64) -> std::result::Result<(), String> {
    let current = ti_contracts::bucket_of(ts, 10).map_err(|e| e.to_string())? >> 16;
    use ti_contracts::ShardSource;
    let keys = durable.store.shards(None, 0, u32::MAX);
    for key in keys {
        if key.shard < current && durable.store.open_shard(&key).is_some() {
            durable.store.seal(key).map_err(|e| e.to_string())?;
        }
    }
    durable
        .store
        .enforce_retention(ts, RETENTION_SECONDS)
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn prepare_logs(bytes: &[u8]) -> Result<PreparedLogs> {
    let request: LogsRequest = parse(bytes)?;
    let mut docs = Vec::new();
    for resource in request.resource_logs {
        let entity = entity(&resource.resource)?;
        for scope in resource.scope_logs {
            for record in scope.log_records {
                let time = record
                    .time_unix_nano
                    .as_ref()
                    .filter(|v| unsigned(v).ok() != Some(0))
                    .or(record.observed_time_unix_nano.as_ref())
                    .ok_or_else(|| invalid("OTLP log requires event or observed timestamp"))?;
                let ts = timestamp(time)?;
                let mut attrs = attributes(&record.attributes);
                if let Some(body) = record.body {
                    attrs.insert("body".into(), body);
                }
                let body = serde_json::to_string(&attrs).map_err(|e| invalid(e.to_string()))?;
                let title = if record.event_name.is_empty() {
                    attrs
                        .get("event.name")
                        .and_then(Value::as_str)
                        .unwrap_or("OTLP log")
                        .to_string()
                } else {
                    record.event_name
                };
                let identity = format!("{entity}|{time}|{title}|{body}");
                let id = format!("otlp:{}", blake3::hash(identity.as_bytes()).to_hex());
                let doc = Document {
                    id,
                    vessel: entity.clone(),
                    kind: "logbook".into(),
                    ts_start: ts,
                    ts_end: None,
                    title,
                    body,
                };
                doc.validate()?;
                docs.push(doc);
            }
        }
    }
    let latest = docs.iter().map(|d| d.ts_start).max();
    Ok(PreparedLogs { docs, latest })
}

/// `(length, generation)` of `docs/documents.log`. Missing or unreadable is `(0, 0)`.
fn log_tip(path: &Path) -> (u64, u64) {
    let Ok(mut file) = std::fs::File::open(path) else {
        return (0, 0);
    };
    let len = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    let mut header = [0u8; 24];
    if std::io::Read::read(&mut file, &mut header).ok() != Some(header.len()) {
        return (len, 0);
    }
    if &header[..8] != b"LUMEDOC\0" {
        return (len, 0);
    }
    let generation = u64::from_le_bytes(header[12..20].try_into().unwrap_or([0; 8]));
    (len, generation)
}

/// Bytes this group's `upsert_all` put on disk, and whether that write was a compaction.
///
/// An unchanged generation is an append, so the write is the growth of `documents.log`.
/// A new generation replaces the log. The first publication has no previous generation.
/// Compaction syncs the group's frame and then renames a new log over it; the frame is
/// no longer in the file, and the measured write is the rewritten log.
fn docs_bytes_written(
    before_len: u64,
    before_gen: u64,
    after_len: u64,
    after_gen: u64,
) -> (u64, bool) {
    let compacted = before_gen != 0 && after_gen != before_gen;
    let written = if before_gen == 0 || compacted {
        after_len
    } else {
        after_len.saturating_sub(before_len)
    };
    (written, compacted)
}

fn commit_logs(
    root: &Path,
    docs: Vec<Document>,
    latest: Option<i64>,
) -> std::result::Result<(), String> {
    let mut store = DocStore::open(root).map_err(|e| e.to_string())?;
    let log_path = root.join("docs").join("documents.log");
    let (before_len, before_gen) = log_tip(&log_path);
    let started = Instant::now();
    store.upsert_all(docs).map_err(|e| e.to_string())?;
    let rewrite_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
    let (after_len, after_gen) = log_tip(&log_path);
    let (bytes, compacted) = docs_bytes_written(before_len, before_gen, after_len, after_gen);
    eprintln!(
        "otlp-docs-commit bytes={bytes} rewrite_us={rewrite_us} compact={}",
        u8::from(compacted)
    );
    if let Some(ts) = latest {
        let newest = store
            .iter()
            .filter(|d| d.id.starts_with("otlp:"))
            .map(|d| d.ts_start)
            .max()
            .unwrap_or(ts);
        let cutoff = newest.max(ts) - RETENTION_SECONDS as i64;
        let expired: Vec<_> = store
            .iter()
            .filter(|d| d.id.starts_with("otlp:") && d.ts_start < cutoff)
            .map(|d| (d.vessel.clone(), d.id.clone()))
            .collect();
        for (entity, id) in expired {
            store.delete(&entity, &id).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Condvar, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};
    fn root() -> tempfile::TempDir {
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.test-tmp");
        std::fs::create_dir_all(&base).unwrap();
        tempfile::tempdir_in(base).unwrap()
    }
    #[test]
    fn checkpoint_validation_and_failed_publish_preserve_previous_state() {
        let root = root();
        let path = root.path().join("otlp-counters.json");
        assert!(load_counters(&path).unwrap().is_empty());
        let mut totals = BTreeMap::new();
        totals.insert(
            ("agent.urn:test".into(), "tokens".into()),
            CounterState {
                total: 12.0,
                raw: 2.0,
                start: 10,
                offset: 10.0,
                end: 1577836820000000000,
            },
        );
        persist_counters(&path, &totals).unwrap();
        assert_eq!(
            load_counters(&path)
                .unwrap()
                .values()
                .next()
                .unwrap()
                .offset,
            10.0
        );
        let before = std::fs::read(&path).unwrap();
        std::fs::create_dir(path.with_extension("json.tmp")).unwrap();
        assert!(persist_counters(&path, &totals).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        std::fs::remove_dir(path.with_extension("json.tmp")).unwrap();
        let entry = json!({"entity":"agent.urn:test","path":"tokens","state":{"total":12,"raw":2,"start":10,"offset":10,"end":1577836820000000000u64}});
        for bad in [
            json!({"version":2,"counters":[]}),
            json!({"version":1,"counters":[entry.clone(),entry.clone()]}),
            json!({"version":1,"counters":vec![entry; MAX_COUNTERS + 1]}),
            json!({"version":1,"counters":[{"entity":"agent.urn:test","path":"tokens","state":{"total":-1,"raw":2,"start":10,"offset":0,"end":20}}]}),
        ] {
            std::fs::write(&path, bad.to_string()).unwrap();
            assert!(load_counters(&path)
                .err()
                .unwrap()
                .to_string()
                .contains("refusing to reset totals"));
        }
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_CHECKPOINT + 1).unwrap();
        assert!(load_counters(&path)
            .err()
            .unwrap()
            .to_string()
            .contains("exceeds 8 MiB"));
    }
    #[test]
    fn identity_precedence_and_integer_encodings() {
        let resource: Resource = serde_json::from_value(json!({"attributes":[
            {"key":"service.name","value":{"stringValue":"fallback"}},
            {"key":"pane","value":{"stringValue":"pane"}},
            {"key":"service.instance.id","value":{"stringValue":"instance"}}
        ]}))
        .unwrap();
        assert_eq!(entity(&resource).unwrap(), "agent.urn:instance");
        assert_eq!(
            timestamp(&json!("1577836811000000000")).unwrap(),
            ti_contracts::EPOCH + 11
        );
        assert_eq!(
            timestamp(&json!(1577836811000000000u64)).unwrap(),
            ti_contracts::EPOCH + 11
        );
        assert!(timestamp(&json!("-1")).is_err());
        assert!(number(&json!("NaN")).is_err());
        assert!(entity(&Resource::default()).is_err());
    }
    #[test]
    fn malformed_batch_is_validated_before_any_write() {
        let root = root();
        let receiver = AgentStore::open(root.path()).unwrap();
        let payload = json!({"resourceMetrics":[{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":"test"}}]},"scopeMetrics":[{"metrics":[
            {"name":"good","gauge":{"dataPoints":[{"timeUnixNano":"1577836811000000000","asDouble":1}]}},
            {"name":"bad","gauge":{"dataPoints":[{"timeUnixNano":"invalid","asDouble":2}]}}
        ]}]}]});
        assert!(receiver.metrics(payload.to_string().as_bytes()).is_err());
        assert!(receiver
            .durable
            .lock()
            .unwrap()
            .catalog
            .fields()
            .unwrap()
            .is_empty());
        assert!(receiver.metrics(b"{}").is_err());
        assert!(receiver.logs(b"{").is_err());
        assert!(receiver.logs(&vec![b' '; MAX_BODY + 1]).is_err());
    }
    #[test]
    fn log_retention_preserves_other_documents_and_observed_time_fallback() {
        let root = root();
        let receiver = AgentStore::open(root.path()).unwrap();
        let unrelated = Document {
            id: "note:keep".into(),
            vessel: "agent.urn:test".into(),
            kind: "notes".into(),
            ts_start: ti_contracts::EPOCH,
            ts_end: None,
            title: "keep".into(),
            body: "keep".into(),
        };
        DocStore::open(root.path())
            .unwrap()
            .upsert_all([unrelated])
            .unwrap();
        let log = |ts| {
            json!({"resourceLogs":[{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":"test"}}]},
            "scopeLogs":[{"logRecords":[{"timeUnixNano":"0","observedTimeUnixNano":format!("{}000000000", ts),"eventName":"file.edit"}]}]}]}).to_string()
        };
        receiver
            .logs(log(ti_contracts::EPOCH + 1).as_bytes())
            .unwrap();
        receiver
            .logs(log(ti_contracts::EPOCH + RETENTION_SECONDS as i64 + 100).as_bytes())
            .unwrap();
        let docs = DocStore::open(root.path()).unwrap();
        assert_eq!(docs.iter().count(), 2);
        assert!(docs.iter().any(|d| d.id == "note:keep"));
    }
    #[test]
    fn counters_delta_cumulative_reset_and_invalid_order() {
        let p = |temporality, start, end| CounterPoint {
            temporality,
            start,
            end,
        };
        let mut delta = CounterState::default();
        assert_eq!(counter_value(&mut delta, &p(1, 1, 10), 2.0).unwrap(), 2.0);
        assert_eq!(counter_value(&mut delta, &p(1, 1, 20), 3.0).unwrap(), 5.0);
        assert_eq!(counter_value(&mut delta, &p(1, 1, 20), 3.0).unwrap(), 5.0);
        assert!(counter_value(&mut delta, &p(1, 1, 15), 1.0).is_err());
        let mut cumulative = CounterState::default();
        assert_eq!(
            counter_value(&mut cumulative, &p(2, 1, 10), 2.0).unwrap(),
            2.0
        );
        assert_eq!(
            counter_value(&mut cumulative, &p(2, 1, 20), 5.0).unwrap(),
            5.0
        );
        assert_eq!(
            counter_value(&mut cumulative, &p(2, 25, 30), 1.0).unwrap(),
            6.0
        );
        assert_eq!(
            counter_value(&mut cumulative, &p(2, 25, 40), 3.0).unwrap(),
            8.0
        );
        assert!(counter_value(&mut cumulative, &p(2, 25, 50), 2.0).is_err());
        assert!(counter_value(&mut cumulative, &p(2, 25, 50), -1.0).is_err());
    }
    #[test]
    fn dimensional_paths_are_order_independent_and_escape_boundaries() {
        let a: Vec<Attribute> = serde_json::from_value(json!([
            {"key":"model","value":{"stringValue":"model.a"}},
            {"key":"type","value":{"stringValue":"input"}}
        ]))
        .unwrap();
        let b: Vec<Attribute> = serde_json::from_value(json!([
            {"key":"type","value":{"stringValue":"input"}},
            {"key":"model","value":{"stringValue":"model.a"}}
        ]))
        .unwrap();
        assert_eq!(metric_path("tokens", &a), "tokens.input.model.model%2Ea");
        assert_eq!(metric_path("tokens", &a), metric_path("tokens", &b));
        assert_ne!(component("model.a"), component("model%2Ea"));
    }
    #[test]
    fn docs_bytes_written_is_append_delta_or_compaction_rewrite() {
        assert_eq!(super::docs_bytes_written(0, 0, 120, 1), (120, false));
        assert_eq!(super::docs_bytes_written(120, 1, 180, 1), (60, false));
        assert_eq!(super::docs_bytes_written(5_000, 1, 900, 2), (900, true));
        assert_eq!(super::docs_bytes_written(180, 1, 180, 1), (0, false));
    }
    #[test]
    fn second_log_group_appends_documents_log() {
        let root = root();
        let receiver = AgentStore::open(root.path()).unwrap();
        let log = |ts: i64| {
            json!({"resourceLogs":[{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":"test"}}]},
            "scopeLogs":[{"logRecords":[{"timeUnixNano":"0","observedTimeUnixNano":format!("{ts}000000000"),"eventName":"file.edit"}]}]}]}).to_string()
        };
        receiver
            .logs(log(ti_contracts::EPOCH + 1).as_bytes())
            .unwrap();
        let path = root.path().join("docs").join("documents.log");
        let (before_len, before_gen) = super::log_tip(&path);
        assert!(before_len > 24);
        assert_eq!(before_gen, 1);
        assert!(!root.path().join("docs").join("documents.json").exists());
        receiver
            .logs(log(ti_contracts::EPOCH + 2).as_bytes())
            .unwrap();
        let (after_len, after_gen) = super::log_tip(&path);
        let (written, compacted) =
            super::docs_bytes_written(before_len, before_gen, after_len, after_gen);
        assert!(!compacted);
        assert_eq!(written, after_len - before_len);
        assert!(written > 0);
    }
    fn gauge(service: &str, value: f64) -> String {
        json!({"resourceMetrics":[{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":service}}]},"scopeMetrics":[{"metrics":[
            {"name":"load","gauge":{"dataPoints":[{"timeUnixNano":"1577836811000000000","asDouble":value}]}}
        ]}]}]}).to_string()
    }
    #[test]
    fn lone_post_flushes_immediately_as_its_own_group() {
        let root = root();
        let receiver = AgentStore::open(root.path()).unwrap();
        let body = gauge("solo", 1.0);
        let started = Instant::now();
        assert_eq!(receiver.metrics(body.as_bytes()).unwrap(), 1);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "lone POST waited {:?}",
            started.elapsed()
        );
        assert_eq!(receiver.commit_stats(), (1, 1));
        assert_eq!(receiver.metrics(body.as_bytes()).unwrap(), 0);
        assert_eq!(receiver.commit_stats(), (1, 1));
    }
    #[test]
    fn arrivals_during_a_flush_form_the_next_group() {
        let root = root();
        let receiver = Arc::new(AgentStore::open(root.path()).unwrap());
        let pause = Arc::new(super::TestPause {
            hold: Mutex::new(true),
            cv: Condvar::new(),
            tripped: AtomicBool::new(false),
        });
        *receiver.pause.lock().unwrap() = Some(Arc::clone(&pause));
        let leader = {
            let receiver = Arc::clone(&receiver);
            thread::spawn(move || receiver.metrics(gauge("leader", 1.0).as_bytes()).unwrap())
        };
        let started = Instant::now();
        while !pause.tripped.load(Ordering::SeqCst) {
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "leader did not reach the flush pause"
            );
            thread::sleep(Duration::from_millis(10));
        }
        let followers: Vec<_> = ["b", "c"]
            .into_iter()
            .map(|name| {
                let receiver = Arc::clone(&receiver);
                let body = gauge(name, 2.0);
                thread::spawn(move || receiver.metrics(body.as_bytes()).unwrap())
            })
            .collect();
        let started = Instant::now();
        while receiver.admit.lock().unwrap().staged.len() < 2 {
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "followers did not stage during the flush"
            );
            thread::sleep(Duration::from_millis(10));
        }
        {
            let mut hold = pause.hold.lock().unwrap();
            *hold = false;
            pause.cv.notify_all();
        }
        assert_eq!(leader.join().unwrap(), 1);
        for follower in followers {
            assert_eq!(follower.join().unwrap(), 1);
        }
        let (commits, max_group) = receiver.commit_stats();
        assert!(commits >= 2, "commits {commits}");
        assert!(
            (2..=super::GROUP_LIMIT).contains(&max_group),
            "max_group {max_group}"
        );
    }
}
