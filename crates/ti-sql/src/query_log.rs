//! Best-effort query events: bounded producer queue, private daily JSONL, live SQL.
use async_trait::async_trait;
use datafusion::{
    arrow::{
        array::{
            ArrayRef, BooleanArray, Float64Array, StringArray, TimestampMillisecondArray,
            UInt64Array,
        },
        datatypes::{DataType, Field, Schema, SchemaRef, TimeUnit},
        record_batch::RecordBatch,
    },
    catalog::{Session, TableProvider},
    common::Result,
    datasource::MemTable,
    logical_expr::{Expr, TableType},
    physical_plan::ExecutionPlan,
};
use serde::{Deserialize, Serialize};
use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, SyncSender},
        Arc, Mutex, OnceLock, Weak,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Debug)]
pub struct Context {
    pub source: String,
    pub identity: Option<String>,
    record: bool,
}
impl Context {
    pub fn new(source: &str, identity: Option<String>) -> Self {
        Self {
            source: source.into(),
            identity,
            record: true,
        }
    }
    pub fn current() -> Self {
        TASK.try_with(Clone::clone)
            .unwrap_or_else(|_| THREAD.with(|c| c.borrow().clone()))
    }
}
tokio::task_local! { static TASK: Context; }
thread_local! { static THREAD: RefCell<Context> = RefCell::new(Context::new("cli", None)); }
pub async fn scoped<F: std::future::Future>(context: Context, future: F) -> F::Output {
    TASK.scope(context, future).await
}
/// Outer result encoders record once after all transport caps have been applied.
pub async fn without_record<F: std::future::Future>(future: F) -> F::Output {
    let mut context = Context::current();
    context.record = false;
    scoped(context, future).await
}
pub struct ThreadContext(Context);
impl ThreadContext {
    pub fn enter(context: Context) -> Self {
        Self(THREAD.with(|c| c.replace(context)))
    }
}
impl Drop for ThreadContext {
    fn drop(&mut self) {
        THREAD.with(|c| c.replace(self.0.clone()));
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    pub ts: i64,
    pub source: String,
    pub identity: Option<String>,
    pub sql: String,
    pub sql_truncated: bool,
    pub elapsed_ms: f64,
    pub rows: u64,
    pub bytes: u64,
    pub error: Option<String>,
}
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}
fn cap(text: &str, limit: usize) -> (String, bool) {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].into(), end < text.len())
}
#[derive(Debug)]
struct State {
    rows: Mutex<VecDeque<Event>>,
    dropped: AtomicU64,
}
#[derive(Debug)]
pub struct QueryLog {
    retention_ms: i64,
    sender: Option<SyncSender<Event>>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
    state: Arc<State>,
    enabled: AtomicBool,
}
type Registry = Mutex<HashMap<PathBuf, Weak<QueryLog>>>;
static REGISTRY: OnceLock<Registry> = OnceLock::new();
impl QueryLog {
    pub fn open(root: &Path, config: &ti_contracts::QueryLogConfig) -> Arc<Self> {
        let key = root.canonicalize().unwrap_or_else(|_| root.into());
        let mut registry = REGISTRY
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        registry.retain(|_, log| log.strong_count() > 0);
        if !root.as_os_str().is_empty() {
            if let Some(log) = registry.get(&key).and_then(Weak::upgrade) {
                return log;
            }
        }
        let state = Arc::new(State {
            rows: Mutex::new(VecDeque::new()),
            dropped: AtomicU64::new(0),
        });
        let (sender, receiver) = mpsc::sync_channel(256);
        let worker = state.clone();
        let root = if root.as_os_str().is_empty() {
            None
        } else {
            Some(root.join("query-log"))
        };
        let config = config.clone();
        let enabled = config.enabled && std::env::var("LUME_TI_QUERY_LOG").as_deref() != Ok("0");
        let retention_ms = i64::from(config.retention_days) * 86_400_000;
        let spawned = std::thread::Builder::new()
            .name("lume-query-log".into())
            .spawn(move || {
                if let Some(dir) = &root {
                    if enabled {
                        load(dir, &worker, &config);
                    }
                }
                loop {
                    let event = match receiver.recv_timeout(Duration::from_secs(60)) {
                        Ok(event) => event,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            if enabled {
                                if let Some(dir) = &root {
                                    prune(dir, &config);
                                }
                            }
                            continue;
                        }
                    };
                    let cutoff = now()
                        .saturating_sub((config.retention_days as i64).saturating_mul(86_400_000));
                    if let Some(dir) = &root {
                        prune(dir, &config);
                        let size =
                            serde_json::to_vec(&event).map_or(u64::MAX, |b| b.len() as u64 + 1);
                        let existing: u64 = files(dir).iter().map(|f| f.2).sum();
                        if existing.saturating_add(size) > config.max_bytes
                            || append(dir, &event).is_err()
                        {
                            worker.dropped.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    let mut rows = worker.rows.lock().unwrap_or_else(|e| e.into_inner());
                    rows.push_back(event);
                    while rows.front().is_some_and(|e| e.ts < cutoff)
                        || rows.len() > config.max_rows
                    {
                        rows.pop_front();
                    }
                }
            });
        let worker = spawned.ok();
        let active = worker.is_some();
        let log = Arc::new(Self {
            retention_ms,
            sender: active.then_some(sender),
            worker: Mutex::new(worker),
            state,
            enabled: AtomicBool::new(enabled),
        });
        registry.insert(key, Arc::downgrade(&log));
        log
    }
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }
    pub fn dropped(&self) -> u64 {
        self.state.dropped.load(Ordering::Relaxed)
    }
    pub fn start(self: &Arc<Self>, sql: &str, context: Context) -> Guard {
        if !context.record || !self.enabled.load(Ordering::Relaxed) {
            return Guard {
                log: self.clone(),
                event: None,
                started: Instant::now(),
            };
        }
        let (sql, sql_truncated) = cap(sql, 4096);
        Guard {
            log: self.clone(),
            event: context.record.then(|| Event {
                ts: now(),
                source: cap(&context.source, 32).0,
                identity: context.identity.map(|s| cap(&s, 256).0),
                sql,
                sql_truncated,
                elapsed_ms: 0.0,
                rows: 0,
                bytes: 0,
                error: Some("query cancelled before completion".into()),
            }),
            started: Instant::now(),
        }
    }
    fn submit(&self, event: Event) {
        if !self.enabled.load(Ordering::Relaxed) {
            return;
        }
        if self
            .sender
            .as_ref()
            .is_none_or(|s| s.try_send(event).is_err())
        {
            self.state.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}
impl Drop for QueryLog {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(worker) = self
            .worker
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            if worker.thread().id() != std::thread::current().id() {
                let _ = worker.join();
            }
        }
    }
}
pub struct Guard {
    log: Arc<QueryLog>,
    event: Option<Event>,
    started: Instant,
}
impl Guard {
    pub fn active(&self) -> bool {
        self.event.is_some()
    }
    /// Successful protocol preparation is not a second execution event.
    pub fn discard(&mut self) {
        self.event = None;
    }
    pub fn progress(&mut self, rows: u64, bytes: u64) {
        if let Some(e) = &mut self.event {
            e.rows = rows;
            e.bytes = bytes;
        }
    }
    pub fn finish(&mut self, error: Option<String>) {
        if let Some(mut e) = self.event.take() {
            e.elapsed_ms = self.started.elapsed().as_secs_f64() * 1000.0;
            e.error = error.map(|s| cap(&s, 1024).0);
            self.log.submit(e);
        }
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(e) = &self.event {
            let error = e.error.clone();
            self.finish(error);
        }
    }
}
fn append(dir: &Path, event: &Event) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        let mut options = OpenOptions::new();
        options.create(true).append(true).mode(0o600);
        let mut f = options.open(dir.join(format!("{}.jsonl", event.ts / 86_400_000)))?;
        serde_json::to_writer(&mut f, event)?;
        f.write_all(b"\n")?;
    }
    #[cfg(not(unix))]
    {
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(format!("{}.jsonl", event.ts / 86_400_000)))?;
        serde_json::to_writer(&mut f, event)?;
        f.write_all(b"\n")?;
    }
    Ok(())
}
fn files(dir: &Path) -> Vec<(i64, PathBuf, u64)> {
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(day) = path
                .file_stem()
                .and_then(|s| s.to_str())
                .and_then(|s| s.parse().ok())
            else {
                continue;
            };
            if let Ok(meta) = entry.metadata() {
                if meta.is_file() {
                    out.push((day, path, meta.len()));
                }
            }
        }
    }
    out.sort_by_key(|e| e.0);
    out
}
fn prune(dir: &Path, config: &ti_contracts::QueryLogConfig) {
    let all = files(dir);
    let mut bytes: u64 = all.iter().map(|e| e.2).sum();
    let cutoff = now() / 86_400_000 - i64::from(config.retention_days);
    for (day, path, size) in all {
        if (day < cutoff || bytes > config.max_bytes) && fs::remove_file(path).is_ok() {
            bytes = bytes.saturating_sub(size);
        }
    }
}
fn load(dir: &Path, state: &State, config: &ti_contracts::QueryLogConfig) {
    prune(dir, config);
    let cutoff = now().saturating_sub(i64::from(config.retention_days) * 86_400_000);
    let mut rows = VecDeque::new();
    for (_, path, _) in files(dir) {
        if let Ok(file) = fs::File::open(path) {
            // Escaped JSON events are bounded to 64 KiB; read bounded records, never unbounded lines.
            let mut reader = BufReader::new(file);
            loop {
                let mut line = Vec::new();
                let mut limited = std::io::Read::take(&mut reader, 8193);
                let n = limited.read_until(b'\n', &mut line).unwrap_or(0);
                if n == 0 {
                    break;
                }
                if n > 65536 || line.last() != Some(&b'\n') {
                    break;
                }
                if let Ok(e) = serde_json::from_slice::<Event>(&line) {
                    if e.ts >= cutoff {
                        rows.push_back(e);
                    }
                    if rows.len() > config.max_rows {
                        rows.pop_front();
                    }
                }
            }
        }
    }
    *state.rows.lock().unwrap_or_else(|e| e.into_inner()) = rows;
}
#[derive(Debug)]
pub struct LogTable(pub Arc<QueryLog>);
impl LogTable {
    fn schema_ref() -> SchemaRef {
        Arc::new(Schema::new(vec![
            Field::new(
                "ts",
                DataType::Timestamp(TimeUnit::Millisecond, Some("UTC".into())),
                false,
            ),
            Field::new("source", DataType::Utf8, false),
            Field::new("identity", DataType::Utf8, true),
            Field::new("sql", DataType::Utf8, false),
            Field::new("sql_truncated", DataType::Boolean, false),
            Field::new("elapsed_ms", DataType::Float64, false),
            Field::new("rows", DataType::UInt64, false),
            Field::new("bytes", DataType::UInt64, false),
            Field::new("error", DataType::Utf8, true),
        ]))
    }
}
#[async_trait]
impl TableProvider for LogTable {
    fn schema(&self) -> SchemaRef {
        Self::schema_ref()
    }
    fn table_type(&self) -> TableType {
        TableType::Base
    }
    async fn scan(
        &self,
        state: &dyn Session,
        projection: Option<&Vec<usize>>,
        filters: &[Expr],
        limit: Option<usize>,
    ) -> Result<Arc<dyn ExecutionPlan>> {
        let rows: Vec<_> = self
            .0
            .state
            .rows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|e| e.ts >= now().saturating_sub(self.0.retention_ms))
            .cloned()
            .collect();
        let columns: Vec<ArrayRef> = vec![
            Arc::new(
                TimestampMillisecondArray::from(rows.iter().map(|e| e.ts).collect::<Vec<_>>())
                    .with_timezone("UTC"),
            ),
            Arc::new(StringArray::from(
                rows.iter().map(|e| e.source.as_str()).collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|e| e.identity.as_deref())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter().map(|e| e.sql.as_str()).collect::<Vec<_>>(),
            )),
            Arc::new(BooleanArray::from(
                rows.iter().map(|e| e.sql_truncated).collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter().map(|e| e.elapsed_ms).collect::<Vec<_>>(),
            )),
            Arc::new(UInt64Array::from(
                rows.iter().map(|e| e.rows).collect::<Vec<_>>(),
            )),
            Arc::new(UInt64Array::from(
                rows.iter().map(|e| e.bytes).collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter().map(|e| e.error.as_deref()).collect::<Vec<_>>(),
            )),
        ];
        let batch = RecordBatch::try_new(Self::schema_ref(), columns)?;
        MemTable::try_new(Self::schema_ref(), vec![vec![batch]])?
            .scan(state, projection, filters, limit)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(ts: i64) -> Event {
        Event {
            ts,
            source: "test".into(),
            identity: None,
            sql: "SELECT 1".into(),
            sql_truncated: false,
            elapsed_ms: 0.1,
            rows: 1,
            bytes: 10,
            error: None,
        }
    }
    #[test]
    fn full_queue_drops_without_waiting_and_off_does_not_enqueue() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let log = QueryLog {
            retention_ms: 1,
            sender: Some(sender),
            worker: Mutex::new(None),
            state: Arc::new(State {
                rows: Mutex::new(VecDeque::new()),
                dropped: AtomicU64::new(0),
            }),
            enabled: AtomicBool::new(true),
        };
        log.submit(event(now()));
        log.submit(event(now()));
        assert_eq!(log.dropped(), 1);
        assert!(receiver.try_recv().is_ok());
        log.set_enabled(false);
        log.submit(event(now()));
        assert!(receiver.try_recv().is_err());
        assert_eq!(log.dropped(), 1);
    }
    #[test]
    fn utf8_caps_and_age_disk_bounds_and_reload() {
        let root = std::env::temp_dir().join(format!("query-log-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let config = ti_contracts::QueryLogConfig {
            retention_days: 1,
            max_rows: 2,
            ..Default::default()
        };
        let old = event(now() - 3 * 86_400_000);
        append(&root, &old).unwrap();
        for _ in 0..3 {
            append(&root, &event(now())).unwrap();
        }
        let state = State {
            rows: Mutex::new(VecDeque::new()),
            dropped: AtomicU64::new(0),
        };
        load(&root, &state, &config);
        assert_eq!(state.rows.lock().unwrap().len(), 2);
        assert_eq!(files(&root).len(), 1);
        let (text, truncated) = cap(&"é".repeat(2500), 4095);
        assert!(truncated);
        assert_eq!(text.len(), 4094);
        prune(
            &root,
            &ti_contracts::QueryLogConfig {
                max_bytes: 1,
                ..config
            },
        );
        assert!(files(&root).is_empty());
        fs::remove_dir_all(root).unwrap();
    }
}
