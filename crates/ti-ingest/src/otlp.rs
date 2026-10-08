//! Hand-written OTLP/HTTP JSON subset. No protobuf runtime or exporter dependency.
use crate::{normalize::NormalizedValue, WatermarkBucketer};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
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
#[derive(Clone, Default)]
struct CounterState {
    total: f64,
    raw: f64,
    start: u64,
    offset: f64,
    end: u64,
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

/// One receiver owns its bucketer and serializes batches. A flush publishes each batch.
pub struct AgentStore {
    store: Store,
    catalog: Arc<dyn Catalog>,
    bucketer: WatermarkBucketer,
    config: TiConfig,
    docs_root: PathBuf,
    totals: BTreeMap<(String, String), CounterState>,
    seen_batches: std::collections::VecDeque<[u8; 32]>,
}
impl AgentStore {
    pub fn open(root: &Path) -> Result<Self> {
        let dir = root.join(STORE_DIR);
        let mut config = TiConfig::default();
        config.profiles.opt_in.push("last".into());
        config.path_scales.clear();
        for scale in config.unit_scales.values_mut() {
            *scale = 6;
        }
        // Log-only stores have no shard header from which the CLI can infer a width.
        // Never replace an existing vessel ingest configuration.
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
        Ok(Self {
            store,
            catalog,
            bucketer,
            config,
            docs_root: root.into(),
            totals: BTreeMap::new(),
            seen_batches: std::collections::VecDeque::new(),
        })
    }
    pub fn metrics(&mut self, bytes: &[u8]) -> Result<usize> {
        let request: MetricsRequest = parse(bytes)?;
        let batch_id = *blake3::hash(bytes).as_bytes();
        if self.seen_batches.contains(&batch_id) {
            return Ok(0);
        }
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
                                    path: format!(
                                        "{}.sum",
                                        metric_path(&metric.name, &p.attributes)
                                    ),
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
        let mut totals = self.totals.clone();
        for sample in &mut samples {
            if let Some(point) = &sample.counter {
                let key = (sample.entity.clone(), sample.path.clone());
                if !totals.contains_key(&key) && totals.len() >= 4096 {
                    return Err(invalid("OTLP counter series limit (4096) reached"));
                }
                sample.value = counter_value(totals.entry(key).or_default(), point, sample.value)?;
            }
            // Generic OTLP fractional seconds/costs must not inherit Signal K's integer-second scale.
            self.config.units.insert(
                sample.path.clone(),
                ti_contracts::MetricUnit {
                    unit: sample.unit.clone(),
                    scale: 6,
                },
            );
            ti_contracts::to_fixed(sample.value, 6)
                .map_err(|e| invalid(format!("OTLP point out of fixed-point range: {e}")))?;
        }
        let count = samples.len();
        let latest = samples.iter().map(|s| s.ts).max();
        for s in samples {
            if !s.unit.is_empty() {
                self.bucketer
                    .classifier_mut()
                    .register_meta_units(&s.path, &s.unit);
            }
            self.bucketer.ingest_point(
                &s.entity,
                &s.path,
                "otlp",
                s.ts,
                NormalizedValue::Double(s.value),
                &self.config,
                self.catalog.as_ref(),
                &mut self.store,
            )?;
        }
        self.bucketer
            .flush_all(&self.config, self.catalog.as_ref(), &mut self.store)?;
        self.store.flush()?;
        self.totals = totals;
        self.seen_batches.push_back(batch_id);
        if self.seen_batches.len() > 128 {
            self.seen_batches.pop_front();
        }
        if let Some(ts) = latest {
            // Completed shards must be sealed before the store's immutable-shard retention can remove them.
            let current = ti_contracts::bucket_of(ts, 10)? >> 16;
            use ti_contracts::ShardSource;
            let keys = self.store.shards(None, 0, u32::MAX);
            for key in keys {
                if key.shard < current && self.store.open_shard(&key).is_some() {
                    self.store.seal(key)?;
                }
            }
            self.store.enforce_retention(ts, RETENTION_SECONDS)?;
        }
        Ok(count)
    }
    pub fn logs(&mut self, bytes: &[u8]) -> Result<usize> {
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
        let count = docs.len();
        let latest = docs.iter().map(|d| d.ts_start).max();
        let mut store = DocStore::open(&self.docs_root)?;
        store.upsert_all(docs)?;
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
                store.delete(&entity, &id)?;
            }
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn root() -> tempfile::TempDir {
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.test-tmp");
        std::fs::create_dir_all(&base).unwrap();
        tempfile::tempdir_in(base).unwrap()
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
        let mut receiver = AgentStore::open(root.path()).unwrap();
        let payload = json!({"resourceMetrics":[{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":"test"}}]},"scopeMetrics":[{"metrics":[
            {"name":"good","gauge":{"dataPoints":[{"timeUnixNano":"1577836811000000000","asDouble":1}]}},
            {"name":"bad","gauge":{"dataPoints":[{"timeUnixNano":"invalid","asDouble":2}]}}
        ]}]}]});
        assert!(receiver.metrics(payload.to_string().as_bytes()).is_err());
        assert!(receiver.catalog.fields().unwrap().is_empty());
        assert!(receiver.metrics(b"{}").is_err());
        assert!(receiver.logs(b"{").is_err());
        assert!(receiver.logs(&vec![b' '; MAX_BODY + 1]).is_err());
    }
    #[test]
    fn log_retention_preserves_other_documents_and_observed_time_fallback() {
        let root = root();
        let mut receiver = AgentStore::open(root.path()).unwrap();
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
}
