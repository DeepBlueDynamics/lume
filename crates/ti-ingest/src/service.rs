//! Live Signal K ingestion service for single and multi-store configurations.
//!
//! Enforces:
//! - Streaming WebSocket ingest with exponential reconnect and backoff (spec/06)
//! - Group-commit WAL sync at least every 1 s (D16)
//! - Periodic flush of dirty rows / open buckets every 60 s or 50k records
//! - Automatic sealing of open shards whose time span ended > 1 h ago
//! - Retention sweep across configured stores (StoreSet)
//! - Durable status reporting (lag, last delta, reconnects) to <store>/ingest_status.json
//! - Clean shutdown on SIGTERM / Ctrl-C, flushing all open buckets and WALs (spec/03)
//! - ClosedBucketObserver hook dispatch for W10 rules

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ti_contracts::{Catalog, Result, ShardKey, ShardSink, ShardSource, TiConfig};
use ti_store::StoreSet;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::Message;

use crate::notifications::NotificationDocuments;
use crate::recorder::DeltaRecorder;
use crate::resources::{DocumentPoller, ResourceClient, ResourceDocuments};
use crate::watermark::{ClosedBucketObserver, MultiStoreBucketer};
use crate::websocket::{connect_signalk, process_message_multi, subscribe_signalk};

pub type ClockFn = Arc<dyn Fn() -> i64 + Send + Sync>;

static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn handle_shutdown_signal(_: std::os::raw::c_int) {
    SHUTDOWN_REQUESTED.store(true, Ordering::Relaxed);
}

/// Register SIGINT (Ctrl-C) and SIGTERM signal handlers for clean service shutdown.
pub fn register_shutdown_signals() {
    #[cfg(unix)]
    {
        extern "C" {
            fn signal(
                sig: std::os::raw::c_int,
                handler: extern "C" fn(std::os::raw::c_int),
            ) -> usize;
        }
        unsafe {
            signal(2, handle_shutdown_signal); // SIGINT
            signal(15, handle_shutdown_signal); // SIGTERM
        }
    }
}

/// Normalize a Signal K WebSocket URL by ensuring the stream path and query are attached.
pub fn normalize_signalk_url(url: &str) -> String {
    let trimmed = url.trim();
    if !trimmed.contains("/signalk/v1/stream") {
        let base = trimmed.trim_end_matches('/');
        format!("{base}/signalk/v1/stream?subscribe=none")
    } else {
        trimmed.to_string()
    }
}

/// Resolve a token argument either as a file path or a raw token literal.
pub fn resolve_token(token_arg: Option<&str>, config_token: Option<&str>) -> Option<String> {
    if let Some(arg) = token_arg {
        let path = Path::new(arg);
        if path.is_file() {
            if let Ok(content) = std::fs::read_to_string(path) {
                return Some(content.trim().to_string());
            }
        }
        return Some(arg.trim().to_string());
    }
    config_token.map(|s| s.trim().to_string())
}

/// Options for configuring the live ingest service.
#[derive(Debug, Clone)]
pub struct IngestServiceOptions {
    pub signalk_url: String,
    pub store_root: PathBuf,
    pub config_path: Option<PathBuf>,
    pub token_path: Option<PathBuf>,
    pub token: Option<String>,
    pub self_urn: Option<String>,
}

/// Live Signal K ingestion service.
pub struct IngestService {
    pub config: TiConfig,
    pub self_urn: String,
    pub running: Arc<AtomicBool>,
    pub clock: Option<ClockFn>,
    pub observer: Option<Box<dyn ClosedBucketObserver>>,
    pub recorder: Option<DeltaRecorder>,
    pub reconnects: u64,
    pub records_ingested: u64,
    pub documents_rejected_pre_epoch: u64,
    pub sample_counters: crate::counters::SampleCounters,
    pub last_delta_ts: Option<i64>,
    pub last_delta_received: Option<SystemTime>,
    pub last_flush_ts: i64,
    pub last_seal_ts: i64,
    pub last_retention_ts: i64,
    pub records_since_flush: u64,
    pub flush_hook: Option<Arc<dyn Fn() + Send + Sync>>,
    pub last_flush_notify: Option<Instant>,
}

impl IngestService {
    /// Create a new IngestService from configuration and a running flag.
    pub fn new(config: TiConfig, running: Arc<AtomicBool>) -> Self {
        let self_urn = "vessels.urn:mrn:imo:mmsi:000000000".to_string();
        let now = chrono::Utc::now().timestamp();
        Self {
            config,
            self_urn,
            running,
            clock: None,
            observer: None,
            recorder: None,
            reconnects: 0,
            records_ingested: 0,
            documents_rejected_pre_epoch: 0,
            sample_counters: Default::default(),
            last_delta_ts: None,
            last_delta_received: None,
            last_flush_ts: now,
            last_seal_ts: now,
            last_retention_ts: now,
            records_since_flush: 0,
            flush_hook: None,
            last_flush_notify: None,
        }
    }

    /// Set a hook to be called on store flush/seal events.
    pub fn set_flush_hook(&mut self, hook: Arc<dyn Fn() + Send + Sync>) {
        self.flush_hook = Some(hook);
    }

    /// Notify flush hook, debounced to at most once every 5 seconds.
    pub fn notify_flush(&mut self) {
        let now = Instant::now();
        if let Some(last) = self.last_flush_notify {
            if now.duration_since(last) < Duration::from_secs(5) {
                return;
            }
        }
        self.last_flush_notify = Some(now);
        if let Some(hook) = &self.flush_hook {
            hook();
        }
    }

    /// Set vessel self URN.
    pub fn with_self_urn(mut self, urn: impl Into<String>) -> Self {
        self.self_urn = urn.into();
        self
    }

    /// Set an optional delta recorder.
    pub fn set_recorder(&mut self, recorder: Option<DeltaRecorder>) {
        self.recorder = recorder;
    }

    /// Set an injected clock function (for testing).
    pub fn set_clock(&mut self, clock: ClockFn) {
        self.clock = Some(clock);
    }

    /// Set a closed bucket observer for the default store.
    pub fn set_closed_bucket_observer(&mut self, observer: Option<Box<dyn ClosedBucketObserver>>) {
        self.observer = observer;
    }

    /// Current SystemTime (using injected clock if set, else SystemTime::now()).
    pub fn current_system_time(&self) -> SystemTime {
        if let Some(ref c) = self.clock {
            let secs = c();
            if secs >= 0 {
                UNIX_EPOCH + Duration::from_secs(secs as u64)
            } else {
                SystemTime::now()
            }
        } else {
            SystemTime::now()
        }
    }

    /// Current timestamp in seconds (using injected clock if set, else UTC now).
    pub fn now_timestamp(&self) -> i64 {
        if let Some(ref c) = self.clock {
            c()
        } else {
            chrono::Utc::now().timestamp()
        }
    }

    /// Check whether the service is currently running and has not received a shutdown signal.
    #[inline]
    pub fn is_active(&self) -> bool {
        self.running.load(Ordering::Relaxed) && !SHUTDOWN_REQUESTED.load(Ordering::Relaxed)
    }

    /// Check all open shards across all stores, sealing any that ended > 1 h ago.
    pub fn check_seal_shards(&mut self, store_set: &mut StoreSet, now_sec: i64) -> Result<usize> {
        let mut sealed_count = 0;
        for store in store_set.stores_mut().values_mut() {
            let width = store.width_seconds();
            let shard_keys = store.shards(None, 0, u32::MAX);
            let to_seal: Vec<ShardKey> = shard_keys
                .into_iter()
                .filter(|key| {
                    if store.open_shard(key).is_some() {
                        let shard_end =
                            ti_contracts::EPOCH + (((key.shard as i64 + 1) << 16) * (width as i64));
                        now_sec - shard_end >= 3600
                    } else {
                        false
                    }
                })
                .collect();

            for key in to_seal {
                if store.seal(key).is_ok() {
                    sealed_count += 1;
                }
            }
        }
        Ok(sealed_count)
    }

    /// Flush all open shards and truncate WALs across all stores.
    pub fn flush_stores(&mut self, store_set: &mut StoreSet) -> Result<()> {
        for store in store_set.stores_mut().values_mut() {
            store.flush_shards()?;
            store.truncate_wals()?;
        }
        Ok(())
    }

    /// Run group-commit WAL tick across all stores.
    pub fn tick_stores(&mut self, store_set: &mut StoreSet) -> Result<()> {
        for store in store_set.stores_mut().values_mut() {
            store.tick()?;
        }
        Ok(())
    }

    /// Write operational status to `<store_root>/ingest_status.json`.
    pub fn write_status(&self, store_root: &Path, is_running: bool) {
        let lag_seconds = self.last_delta_received.and_then(|rec| {
            let rec_sec = rec.duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
            self.last_delta_ts
                .map(|d_ts| (rec_sec - d_ts).max(0) as f64)
        });
        let last_delta_rfc = self
            .last_delta_ts
            .and_then(|ts| chrono::DateTime::from_timestamp(ts, 0).map(|dt| dt.to_rfc3339()));
        let status = serde_json::json!({
            "running": is_running,
            "pid": std::process::id(),
            "reconnects": self.reconnects,
            "records_ingested": self.records_ingested,
            "documents_rejected_pre_epoch": self.documents_rejected_pre_epoch,
            "samples_dropped_late": self.sample_counters.samples_dropped_late,
            "samples_dropped_nonfinite": self.sample_counters.samples_dropped_nonfinite,
            "samples_skipped_magnitude": self.sample_counters.samples_skipped_magnitude,
            "samples_rejected_source": self.sample_counters.samples_rejected_source,
            "apply_failures": self.sample_counters.apply_failures,
            "apply_retries": self.sample_counters.apply_retries,
            "samples_rejected_blocked": self.sample_counters.samples_rejected_blocked,
            "ingest_blocked": self.sample_counters.ingest_blocked,
            "ingest_lag_seconds": lag_seconds,
            "last_delta": last_delta_rfc,
            "updated_at": chrono::Utc::now().to_rfc3339(),
        });
        let status_file = store_root.join("ingest_status.json");
        let tmp_file = store_root.join("ingest_status.json.tmp");
        if let Ok(s) = serde_json::to_string_pretty(&status) {
            if std::fs::write(&tmp_file, s).is_ok() {
                let _ = std::fs::rename(&tmp_file, &status_file);
            }
        }
    }

    fn apply_resource_snapshots(
        &mut self,
        polling: &mut Option<(DocumentPoller, ResourceDocuments)>,
    ) {
        if let Some((poller, documents)) = polling {
            for snapshot in poller.receiver.try_iter() {
                let previous_rejected = documents.rejected_pre_epoch;
                if let Err(error) = documents.apply(snapshot, &self.self_urn, self.now_timestamp())
                {
                    eprintln!("Signal K document reconciliation: {error}");
                }
                self.documents_rejected_pre_epoch =
                    self.documents_rejected_pre_epoch.saturating_add(
                        documents
                            .rejected_pre_epoch
                            .saturating_sub(previous_rejected),
                    );
            }
        }
    }

    /// Run the full ingest service loop until `running` is set to false.
    pub fn run(&mut self) -> Result<()> {
        let mut store_set = StoreSet::open_or_create(&self.config)?;
        let catalogs_arc: BTreeMap<String, Arc<ti_store::DiskCatalog>> = store_set
            .stores()
            .iter()
            .map(|(k, v)| (k.clone(), Arc::clone(v.catalog())))
            .collect();
        let mut bucketer = MultiStoreBucketer::new(&self.config)?;
        if let Some(obs) = self.observer.take() {
            let _ = bucketer.set_closed_bucket_observer("default", Some(obs));
        }

        let default_root = self.config.resolved_stores()["default"]
            .resolved_root(&self.config.store_root, "default");
        let store_root_path = PathBuf::from(&self.config.store_root);
        let config_file = store_root_path.join("ti.toml");
        if !config_file.exists() {
            let toml_str = format!("width_seconds = {}\n", self.config.width_seconds);
            let _ = std::fs::write(&config_file, toml_str);
        }
        let mut documents = NotificationDocuments::open(Path::new(&default_root))?;

        let mut recorder = self.recorder.take();

        let url = self.config.signal_k.url.clone();
        let token = self.config.signal_k.token.clone();

        // Fetch remotely on a worker, but apply on this thread alongside
        // notifications/rules so document read-modify-write operations cannot race.
        let mut resource_setup = match ResourceClient::new(&url, token.clone()).and_then(|client| {
            let docs = ResourceDocuments::open(Path::new(&default_root))?;
            Ok((client, docs))
        }) {
            Ok(polling) => Some(polling),
            Err(error) => {
                eprintln!("Signal K document polling unavailable: {error}");
                None
            }
        };

        let mut resource_polling = None;
        // Resources belong to this server's self vessel. Wait for its hello
        // rather than persisting them against the placeholder before connecting.
        let mut resource_vessel_ready = self.self_urn != "vessels.urn:mrn:imo:mmsi:000000000";
        let mut backoff = Duration::from_secs(2);
        let max_backoff = Duration::from_secs(30);

        let mut last_wal_tick = self.now_timestamp();
        let mut last_flush = self.now_timestamp();
        let mut last_seal_check = self.now_timestamp();
        let mut last_retention_check = self.now_timestamp();
        let mut last_status_write = 0i64;

        SHUTDOWN_REQUESTED.store(false, Ordering::Relaxed);
        register_shutdown_signals();
        while self.is_active() {
            if resource_vessel_ready {
                self.apply_resource_snapshots(&mut resource_polling);
            }
            let connection = connect_signalk(&url, token.as_deref());
            // Start HTTP after the initial websocket handshake attempt. This
            // keeps the initial self-vessel connection ahead of resource reads.
            if let Some((client, documents)) = resource_setup.take() {
                match DocumentPoller::start(client) {
                    Ok(poller) => resource_polling = Some((poller, documents)),
                    Err(error) => eprintln!("Signal K document polling unavailable: {error}"),
                }
            }
            match connection {
                Ok(mut socket) => {
                    backoff = Duration::from_secs(2);
                    if let Err(e) = subscribe_signalk(&mut socket) {
                        eprintln!("Failed to send subscribe messages: {e}");
                        std::thread::sleep(backoff);
                        continue;
                    }

                    // Set read timeout so maintenance timers run even during traffic lulls
                    if let MaybeTlsStream::Plain(ref s) = socket.get_ref() {
                        let _ = s.set_read_timeout(Some(Duration::from_millis(200)));
                    }

                    while self.is_active() {
                        match socket.read() {
                            Ok(Message::Text(text)) => {
                                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&text) {
                                    if let Some(s) = val.get("self").and_then(|s| s.as_str()) {
                                        let canonical = if s.starts_with("vessels.urn:") {
                                            s.to_string()
                                        } else if s.starts_with("urn:") {
                                            format!("vessels.{s}")
                                        } else {
                                            format!("vessels.urn:mrn:signalk:{s}")
                                        };
                                        self.self_urn = canonical;
                                        resource_vessel_ready = true;
                                    }
                                }
                                let receive_time = self.current_system_time();
                                let recv_secs = receive_time
                                    .duration_since(UNIX_EPOCH)
                                    .map(|d| d.as_secs() as i64)
                                    .unwrap_or(0);

                                if let Err(e) = documents.ingest_message(
                                    &text,
                                    &self.self_urn,
                                    recv_secs,
                                    &self.config,
                                ) {
                                    eprintln!("Error ingesting notification document: {e}");
                                }

                                let catalogs: BTreeMap<String, &dyn Catalog> = catalogs_arc
                                    .iter()
                                    .map(|(k, v)| (k.clone(), v.as_ref() as &dyn Catalog))
                                    .collect();
                                let mut sinks: BTreeMap<String, &mut dyn ShardSink> = store_set
                                    .stores_mut()
                                    .iter_mut()
                                    .map(|(k, v)| (k.clone(), v as &mut dyn ShardSink))
                                    .collect();

                                let prior_failures = bucketer.counters().apply_failures;
                                match process_message_multi(
                                    &text,
                                    &self.self_urn,
                                    receive_time,
                                    &mut bucketer,
                                    &self.config,
                                    &catalogs,
                                    &mut sinks,
                                    &mut recorder,
                                ) {
                                    Ok(count) => {
                                        if count > 0 {
                                            self.records_since_flush += count as u64;
                                            self.records_ingested += count as u64;
                                            self.last_delta_received = Some(receive_time);
                                            let max_event = bucketer.max_event_time();
                                            if max_event > ti_contracts::EPOCH {
                                                self.last_delta_ts = Some(max_event);
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        if bucketer.counters().apply_failures == prior_failures && !bucketer.counters().ingest_blocked {
                                            eprintln!("Error processing Signal K message: {e}");
                                        }
                                    }
                                }
                            }
                            Ok(Message::Ping(payload)) => {
                                let _ = socket.send(Message::Pong(payload));
                            }
                            Ok(Message::Close(_)) => {
                                break;
                            }
                            Ok(_) => {}
                            Err(tungstenite::Error::Io(ref e))
                                if e.kind() == std::io::ErrorKind::WouldBlock
                                    || e.kind() == std::io::ErrorKind::TimedOut =>
                            {
                                // Timeout tick: proceed directly to periodic timer checks
                            }
                            Err(e) => {
                                if self.is_active() {
                                    eprintln!("Signal K stream read error: {e}");
                                    self.reconnects += 1;
                                }
                                break;
                            }
                        }

                        if resource_vessel_ready {
                            self.apply_resource_snapshots(&mut resource_polling);
                        }

                        // Periodic timer maintenance
                        let now_sec = self.now_timestamp();

                        // 1. Group commit WAL fsync (every 1 s)
                        if now_sec.saturating_sub(last_wal_tick) >= 1 {
                            if let Err(error) = self.tick_stores(&mut store_set) {
                                eprintln!("Ingest WAL tick failed: {error}");
                            }
                            last_wal_tick = now_sec;
                        }

                        // 2. Periodic flush of dirty rows / buckets (every 5 s when dirty, or every 60 s, or 50k records)
                        let flush_due = if self.records_since_flush > 0 {
                            now_sec.saturating_sub(last_flush) >= 5
                                || self.records_since_flush >= 50_000
                        } else {
                            now_sec.saturating_sub(last_flush) >= 60
                        };
                        if flush_due {
                            let watermark = bucketer.max_event_time().saturating_sub(30);
                            let catalogs: BTreeMap<String, &dyn Catalog> = catalogs_arc
                                .iter()
                                .map(|(k, v)| (k.clone(), v.as_ref() as &dyn Catalog))
                                .collect();
                            let mut sinks: BTreeMap<String, &mut dyn ShardSink> = store_set
                                .stores_mut()
                                .iter_mut()
                                .map(|(k, v)| (k.clone(), v as &mut dyn ShardSink))
                                .collect();
                            let prior_failures = bucketer.counters().apply_failures;
                            if let Err(error) = bucketer.advance_watermark(watermark, &self.config, &catalogs, &mut sinks) {
                                if bucketer.counters().apply_failures == prior_failures && !bucketer.counters().ingest_blocked {
                                    eprintln!("Ingest bucket publication failed: {error}");
                                }
                            }
                            if let Err(error) = self.flush_stores(&mut store_set) {
                                eprintln!("Ingest store flush failed: {error}");
                            }
                            self.notify_flush();
                            self.records_since_flush = 0;
                            last_flush = now_sec;
                        }

                        // 3. Seal open shards whose time span ended > 1 h ago
                        if now_sec.saturating_sub(last_seal_check) >= 60 {
                            if let Ok(sealed) = self.check_seal_shards(&mut store_set, now_sec) {
                                if sealed > 0 {
                                    self.notify_flush();
                                }
                            }
                            last_seal_check = now_sec;
                        }

                        // 4. Retention sweep
                        if now_sec.saturating_sub(last_retention_check) >= 60 {
                            let _ = store_set.enforce_retention(now_sec, &self.config);
                            last_retention_check = now_sec;
                        }

                        // Retry retained windows independently of incoming traffic or dirty-row flush.
                        {
                            let catalogs = catalogs_arc.iter().map(|(name, catalog)|
                                (name.clone(), catalog.as_ref() as &dyn Catalog)).collect();
                            let mut sinks = store_set.stores_mut().iter_mut().map(|(name, store)|
                                (name.clone(), store as &mut dyn ShardSink)).collect();
                            let prior_failures = bucketer.counters().apply_failures;
                            if let Err(error) = bucketer.retry_pending(Instant::now(), &self.config, &catalogs, &mut sinks) {
                                if bucketer.counters().apply_failures == prior_failures && !bucketer.counters().ingest_blocked {
                                    eprintln!("Ingest retry publication failed: {error}");
                                }
                            }
                        }
                        self.sample_counters = bucketer.counters();

                        // 5. Update status file
                        if now_sec.saturating_sub(last_status_write) >= 1 {
                            self.write_status(&store_root_path, true);
                            last_status_write = now_sec;
                        }
                    }
                }
                Err(e) => {
                    if self.is_active() {
                        eprintln!("Signal K connection error ({e}), retrying in {backoff:?}...");
                        self.reconnects += 1;
                        self.write_status(&store_root_path, true);

                        // Sleep in small increments to respond promptly to shutdown signals
                        let sleep_end = SystemTime::now() + backoff;
                        while SystemTime::now() < sleep_end && self.running.load(Ordering::Relaxed)
                        {
                            std::thread::sleep(Duration::from_millis(100));
                        }
                        backoff = (backoff * 2).min(max_backoff);
                    }
                }
            }
        }

        // Clean shutdown: flush all open buckets to sinks, flush shards, and sync WALs
        let catalogs: BTreeMap<String, &dyn Catalog> = catalogs_arc
            .iter()
            .map(|(k, v)| (k.clone(), v.as_ref() as &dyn Catalog))
            .collect();
        let mut sinks: BTreeMap<String, &mut dyn ShardSink> = store_set
            .stores_mut()
            .iter_mut()
            .map(|(k, v)| (k.clone(), v as &mut dyn ShardSink))
            .collect();

        // Give acknowledged retries a bounded shutdown grace period; never report a
        // clean shutdown with retained, unpublished samples.
        let shutdown_deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let result = bucketer.flush_all(&self.config, &catalogs, &mut sinks);
            self.sample_counters = bucketer.counters();
            if bucketer.pending_retry_count() == 0 {
                result?;
                break;
            }
            if Instant::now() >= shutdown_deadline {
                self.write_status(&store_root_path, false);
                return result.and_then(|_| Err(ti_contracts::Error::InvalidInput(
                    "shutdown has unpublished ingest windows after retry grace period".into())));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        self.flush_stores(&mut store_set)?;
        self.notify_flush();

        for store in store_set.stores_mut().values_mut() {
            store.shutdown()?;
        }

        self.write_status(&store_root_path, false);
        Ok(())
    }
}
