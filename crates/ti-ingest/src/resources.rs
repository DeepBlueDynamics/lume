//! Read-only Signal K document polling. HTTP runs off the telemetry thread;
//! completed snapshots are reconciled on that thread, serializing all document writes.
use serde_json::Value;
use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};
use ti_contracts::{Document, Error, Result};
use ti_store::DocStore;
const MAX_BYTES: u64 = 8 * 1024 * 1024;
const MAX_ENTRIES: usize = 20_000;
pub const POLL_INTERVAL: Duration = Duration::from_secs(60);
fn invalid(message: impl Into<String>) -> Error {
    Error::InvalidInput(message.into())
}
fn timestamp(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| {
        value
            .as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.timestamp())
    })
}
fn entries(value: Value) -> Result<BTreeMap<String, Value>> {
    match value {
        Value::Object(map) => {
            if map.len() > MAX_ENTRIES || map.values().any(|v| !v.is_object()) {
                return Err(invalid("invalid resource collection"));
            }
            Ok(map.into_iter().collect())
        }
        Value::Array(values) => {
            if values.len() > MAX_ENTRIES {
                return Err(invalid("resource entry cap exceeded"));
            }
            let mut result = BTreeMap::new();
            for value in values {
                let id = value
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("resource without id"))?
                    .to_owned();
                if result.insert(id, value).is_some() {
                    return Err(invalid("duplicate resource id"));
                }
            }
            Ok(result)
        }
        _ => Err(invalid("resource collection is neither map nor array")),
    }
}
/// Complete successful snapshot, never a partial page or failed response.
pub struct ResourceSnapshot {
    pub kind: String,
    pub entries: BTreeMap<String, Value>,
}
/// Fetcher shared by the worker and HTTP integration tests.
pub struct ResourceClient {
    base: String,
    token: Option<String>,
    agent: ureq::Agent,
    cancel: Arc<AtomicBool>,
    deadline: Cell<Option<Instant>>,
}
impl ResourceClient {
    pub fn new(url: &str, token: Option<String>) -> Result<Self> {
        let http = url
            .replacen("ws://", "http://", 1)
            .replacen("wss://", "https://", 1);
        let uri = http
            .parse::<tungstenite::http::Uri>()
            .map_err(|_| invalid("invalid Signal K URL"))?;
        let scheme = uri
            .scheme_str()
            .filter(|s| matches!(*s, "http" | "https"))
            .ok_or_else(|| invalid("Signal K HTTP scheme"))?;
        let authority = uri.authority().ok_or_else(|| invalid("Signal K host"))?;
        Ok(Self {
            base: format!("{scheme}://{authority}"),
            token,
            cancel: Arc::new(AtomicBool::new(false)),
            deadline: Cell::new(None),
            agent: ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(5))
                .redirects(0)
                .build(),
        })
    }
    fn get(&self, path: &str) -> Result<Option<Value>> {
        if self.cancel.load(Ordering::Relaxed)
            || self.deadline.get().is_some_and(|d| Instant::now() >= d)
        {
            return Err(invalid("document polling cancelled or budget exceeded"));
        }
        let url = format!("{}{path}", self.base);
        let mut request = self.agent.get(&url).set("Accept", "application/json");
        if let Some(token) = &self.token {
            request = request.set("Authorization", &format!("Bearer {token}"));
        }
        let response = match request.call() {
            Ok(response) => response,
            Err(ureq::Error::Status(404, _)) => return Ok(None),
            Err(_) => return Err(invalid(format!("Signal K document request failed: {path}"))),
        };
        let mut bytes = Vec::new();
        response
            .into_reader()
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(invalid("resource response exceeds 8 MiB"));
        }
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| invalid("invalid resource JSON"))
    }
    pub fn notes(&self) -> Result<Option<ResourceSnapshot>> {
        self.deadline
            .set(Some(Instant::now() + Duration::from_secs(45)));
        self.get("/signalk/v2/api/resources/notes")?
            .map(|v| {
                entries(v).map(|entries| ResourceSnapshot {
                    kind: "notes".into(),
                    entries,
                })
            })
            .transpose()
    }
    pub fn logbook(&self) -> Result<Option<ResourceSnapshot>> {
        self.deadline
            .set(Some(Instant::now() + Duration::from_secs(45)));
        // Modern plugin requires an explicit time window. No limit: a partial
        // listing must never be treated as a deletion-authoritative snapshot.
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let path =
            format!("/signalk/v2/api/resources/logentries?from=2020-01-01T00:00:00Z&to={now}");
        if let Some(value) = self.get(&path)? {
            return Ok(Some(ResourceSnapshot {
                kind: "logbook".into(),
                entries: entries(value)?,
            }));
        }
        let Some(days) = self.get("/plugins/signalk-logbook/logs")? else {
            return Ok(None);
        };
        let days = days
            .as_array()
            .ok_or_else(|| invalid("invalid logbook days"))?;
        if days.len() > 3660 {
            return Err(invalid("logbook day cap exceeded"));
        }
        let mut all = BTreeMap::new();
        let mut total_bytes = 0usize;
        for day in days {
            let day = day
                .as_str()
                .filter(|s| {
                    s.len() == 10 && chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()
                })
                .ok_or_else(|| invalid("invalid logbook date"))?;
            let Some(values) = self.get(&format!("/plugins/signalk-logbook/logs/{day}"))? else {
                return Err(invalid("missing logbook day"));
            };
            let values = values
                .as_array()
                .ok_or_else(|| invalid("invalid logbook entries"))?;
            for value in values {
                let id = value
                    .get("id")
                    .and_then(Value::as_str)
                    .or_else(|| value.get("datetime").and_then(Value::as_str))
                    .ok_or_else(|| invalid("logbook entry missing identity"))?;
                let id = if value.get("id").and_then(Value::as_str).is_some() {
                    id.to_owned()
                } else {
                    format!("{day}/{id}")
                };
                total_bytes += value.to_string().len();
                if total_bytes > MAX_BYTES as usize {
                    return Err(invalid("logbook snapshot exceeds 8 MiB"));
                }
                if all.insert(id, value.clone()).is_some() {
                    return Err(invalid("duplicate logbook identity"));
                }
            }
            if all.len() > MAX_ENTRIES {
                return Err(invalid("logbook entry cap exceeded"));
            }
        }
        Ok(Some(ResourceSnapshot {
            kind: "logbook".into(),
            entries: all,
        }))
    }
}
/// Durable ownership ledger ensures deletion cannot erase manually imported docs.
/// It is written before document mutations (union of old/new IDs), then narrowed
/// after success, so a crash does not orphan new IDs from subsequent reconciliation.
pub struct ResourceDocuments {
    root: PathBuf,
    owned: BTreeMap<String, BTreeSet<String>>,
    pub rejected_pre_epoch: u64,
}
impl ResourceDocuments {
    pub fn open(root: &Path) -> Result<Self> {
        let path = root.join("docs/resource-owned.json");
        let owned = if path.exists() {
            serde_json::from_slice(&std::fs::read(&path)?)
                .map_err(|_| invalid("invalid resource ownership ledger"))?
        } else {
            BTreeMap::new()
        };
        Ok(Self {
            root: root.into(),
            owned,
            rejected_pre_epoch: 0,
        })
    }
    fn persist(&self) -> Result<()> {
        let path = self.root.join("docs/resource-owned.json");
        std::fs::create_dir_all(path.parent().expect("docs parent"))?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(
            &tmp,
            serde_json::to_vec(&self.owned).map_err(|_| invalid("resource ledger JSON"))?,
        )?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }
    pub fn apply(&mut self, snapshot: ResourceSnapshot, vessel: &str, now: i64) -> Result<()> {
        if !matches!(snapshot.kind.as_str(), "notes" | "logbook") {
            return Err(invalid("resource kind"));
        }
        let rejected = snapshot
            .entries
            .values()
            .filter(|value| {
                let range = value
                    .get("timeRange")
                    .or_else(|| value.get("range"))
                    .or_else(|| value.get("properties").and_then(|p| p.get("x-lume-ti")))
                    .or_else(|| value.get("x-lume-ti"));
                range
                    .and_then(|r| r.get("start"))
                    .or_else(|| value.get("ts_start"))
                    .or_else(|| value.get("start"))
                    .or_else(|| value.get("datetime"))
                    .or_else(|| value.get("timestamp"))
                    .or_else(|| value.get("createdAt"))
                    .or_else(|| value.get("created"))
                    .or_else(|| value.get("creationTime"))
                    .and_then(timestamp)
                    .is_some_and(|ts| ts < ti_contracts::EPOCH)
            })
            .count() as u64;
        self.rejected_pre_epoch = self.rejected_pre_epoch.saturating_add(rejected);
        if rejected > 0 {
            return Err(invalid("resource snapshot contains pre-2020 documents"));
        }
        let owner = format!("{vessel}/{}", snapshot.kind);
        let old = self.owned.get(&owner).cloned().unwrap_or_default();
        let mut store = DocStore::open(&self.root)?;
        let previous_starts: BTreeMap<_, _> = store
            .iter()
            .filter(|d| d.vessel == vessel)
            .map(|d| (d.id.clone(), d.ts_start))
            .collect();
        let mut documents = Vec::new();
        for (id, value) in snapshot.entries {
            let id = if snapshot.kind == "notes" {
                id
            } else {
                format!("logbook/{id}")
            };
            let previous = previous_starts.get(&id);
            let range = value
                .get("timeRange")
                .or_else(|| value.get("range"))
                .or_else(|| value.get("properties").and_then(|p| p.get("x-lume-ti")))
                .or_else(|| value.get("x-lume-ti"));
            let start = range
                .and_then(|r| r.get("start"))
                .or_else(|| value.get("ts_start"))
                .or_else(|| value.get("start"))
                .or_else(|| value.get("datetime"))
                .or_else(|| value.get("timestamp"))
                .or_else(|| value.get("createdAt"))
                .or_else(|| value.get("created"))
                .or_else(|| value.get("creationTime"));
            let ts_start = match start {
                Some(v) => timestamp(v).ok_or_else(|| invalid("invalid note timestamp"))?,
                None => previous.copied().unwrap_or(now),
            };
            let end = range
                .and_then(|r| r.get("end"))
                .or_else(|| value.get("ts_end"))
                .or_else(|| value.get("end").filter(|v| !v.is_boolean()));
            let ts_end = end
                .filter(|v| !v.is_null())
                .map(|v| timestamp(v).ok_or_else(|| invalid("invalid note end")))
                .transpose()?;
            let title = value
                .get("title")
                .or_else(|| value.get("name"))
                .and_then(Value::as_str)
                .unwrap_or(if snapshot.kind == "notes" {
                    "Note"
                } else {
                    "Logbook"
                })
                .to_owned();
            let mut body = ["description", "text"]
                .into_iter()
                .filter_map(|k| value.get(k).and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n");
            if let Some(position) = value.get("position") {
                body.push_str(&format!("\nposition: {position}"));
            }
            if let Some(telemetry) = value.get("telemetry") {
                body.push_str(&format!("\ntelemetry: {telemetry}"));
            }
            let doc = Document {
                id,
                vessel: vessel.into(),
                kind: snapshot.kind.clone(),
                ts_start,
                ts_end,
                title,
                body,
            };
            doc.validate()?;
            documents.push(doc);
        }
        let current: BTreeSet<_> = documents.iter().map(|d| d.id.clone()).collect();
        self.owned
            .insert(owner.clone(), old.union(&current).cloned().collect());
        self.persist()?;
        store.reconcile(vessel, &old, documents)?;
        self.owned.insert(owner, current);
        self.persist()
    }
}
/// One worker; bounded mailbox prevents snapshots accumulating while reconnecting.
pub struct DocumentPoller {
    pub receiver: mpsc::Receiver<ResourceSnapshot>,
    stop: mpsc::Sender<()>,
    cancel: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl DocumentPoller {
    pub fn start(client: ResourceClient) -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(2);
        let (stop, stopped) = mpsc::channel();
        let cancel = Arc::clone(&client.cancel);
        let worker = std::thread::Builder::new()
            .name("signalk-documents".into())
            .spawn(move || loop {
                let started = Instant::now();
                for kind in ["notes", "logbook"] {
                    if client.cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    match if kind == "notes" {
                        client.notes()
                    } else {
                        client.logbook()
                    } {
                        Ok(Some(snapshot)) => {
                            if let Err(mpsc::TrySendError::Disconnected(_)) =
                                sender.try_send(snapshot)
                            {
                                return;
                            }
                        }
                        Ok(None) => {}
                        Err(e) => eprintln!("Signal K document poll: {e}"),
                    }
                }
                match stopped.recv_timeout(POLL_INTERVAL.saturating_sub(started.elapsed())) {
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    _ => break,
                }
            })?;
        Ok(Self {
            receiver,
            stop,
            cancel,
            worker: Some(worker),
        })
    }
}
impl Drop for DocumentPoller {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
