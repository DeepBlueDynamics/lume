//! Signal K WebSocket client and subscription management.
//!
//! Enforces:
//! - Plain `ws://` client over blocking tungstenite (D28)
//! - Subscription to `*` @ 1000ms instant and `notifications.*` instant
//! - Token-based authentication header (`Authorization: Bearer <token>`)
//! - Reconnect loop with exponential backoff
//! - Live session recording to NDJSON via `DeltaRecorder`

use std::collections::BTreeMap;
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ti_contracts::{Catalog, Error, Result, ShardSink, TiConfig};
use tungstenite::client::IntoClientRequest;
use tungstenite::http::HeaderValue;
use tungstenite::{connect, Message, WebSocket};

use crate::decode::decode_delta;
use crate::normalize::normalize_point;
use crate::notifications::NotificationDocuments;
use crate::recorder::DeltaRecorder;
use crate::watermark::{MultiStoreBucketer, WatermarkBucketer};

/// Build the two Signal K subscription messages required by spec 06:
/// 1. All paths at 1000ms period with instant policy.
/// 2. Notification paths with instant policy.
pub fn build_subscription_messages() -> (String, String) {
    let sub_all = serde_json::json!({
        "context": "vessels.self",
        "subscribe": [{
            "path": "*",
            "period": 1000,
            "policy": "instant"
        }]
    })
    .to_string();

    let sub_notif = serde_json::json!({
        "context": "vessels.self",
        "subscribe": [{
            "path": "notifications.*",
            "policy": "instant"
        }]
    })
    .to_string();

    (sub_all, sub_notif)
}

/// Establish a blocking WebSocket connection to Signal K.
pub fn connect_signalk(
    url: &str,
    token: Option<&str>,
) -> Result<WebSocket<tungstenite::stream::MaybeTlsStream<TcpStream>>> {
    let mut request = url
        .into_client_request()
        .map_err(|e| Error::InvalidInput(format!("invalid websocket URL '{url}': {e}")))?;

    if let Some(tok) = token {
        let val = HeaderValue::from_str(&format!("Bearer {tok}"))
            .map_err(|e| Error::InvalidInput(format!("invalid token header: {e}")))?;
        request.headers_mut().insert("Authorization", val);
    }

    let (socket, _) = connect(request).map_err(|e| {
        Error::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            e,
        ))
    })?;

    Ok(socket)
}

/// Send the standard subscription messages over an open WebSocket.
pub fn subscribe_signalk<S: std::io::Read + std::io::Write>(
    socket: &mut WebSocket<S>,
) -> Result<()> {
    let (sub_all, sub_notif) = build_subscription_messages();
    socket
        .send(Message::Text(sub_all))
        .map_err(|e| Error::Io(std::io::Error::new(std::io::ErrorKind::BrokenPipe, e)))?;
    socket
        .send(Message::Text(sub_notif))
        .map_err(|e| Error::Io(std::io::Error::new(std::io::ErrorKind::BrokenPipe, e)))?;
    Ok(())
}

/// Process a single text message from the Signal K stream.
#[allow(clippy::too_many_arguments)]
pub fn process_message(
    text: &str,
    self_urn: &str,
    receive_time: std::time::SystemTime,
    bucketer: &mut WatermarkBucketer,
    config: &TiConfig,
    catalog: &dyn Catalog,
    sink: &mut dyn ShardSink,
    recorder: &mut Option<DeltaRecorder>,
) -> Result<usize> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(0);
    }

    // Try parsing as a delta
    let delta: crate::decode::SignalKDelta = match serde_json::from_str(trimmed) {
        Ok(d) => d,
        Err(_) => return Ok(0), // Ignore non-delta messages (e.g. server hello / responses)
    };

    if let Some(rec) = recorder {
        let _ = rec.record_delta(&delta);
    }

    let recv_secs = receive_time
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (raw_points, meta) = decode_delta(&delta, self_urn, recv_secs);
    for (p, v) in meta {
        if let Some(units) = v.get("units").and_then(|u| u.as_str()) {
            bucketer.classifier_mut().register_meta_units(&p, units);
        }
    }

    let mut points_ingested = 0;
    let mut first_error = None;
    for raw in raw_points {
        let norm_points = normalize_point(raw, &config.allow_paths, &config.deny_paths);
        for p in norm_points {
            let result = bucketer.ingest_point(
                &p.context,
                &p.path,
                &p.source,
                p.timestamp,
                p.value,
                config,
                catalog,
                sink,
            );
            if let Err(error) = result {
                if first_error.is_none() { first_error = Some(error); }
            } else {
                points_ingested += 1;
            }
        }
    }

    first_error.map_or(Ok(points_ingested), Err)
}

/// Run the blocking Signal K stream loop with reconnect and exponential backoff.
pub fn run_stream_loop(
    self_urn: &str,
    config: &TiConfig,
    catalog: &dyn Catalog,
    sink: &mut dyn ShardSink,
    bucketer: &mut WatermarkBucketer,
    running: Arc<AtomicBool>,
    mut recorder: Option<DeltaRecorder>,
) -> Result<()> {
    let root = config.resolved_stores()["default"].resolved_root(&config.store_root, "default");
    let mut documents = NotificationDocuments::open(std::path::Path::new(&root))?;
    let url = &config.signal_k.url;
    let token = config.signal_k.token.as_deref();

    let mut backoff = Duration::from_secs(1);
    let max_backoff = Duration::from_secs(30);

    while running.load(Ordering::Relaxed) {
        match connect_signalk(url, token) {
            Ok(mut socket) => {
                backoff = Duration::from_secs(1);
                if let Err(e) = subscribe_signalk(&mut socket) {
                    eprintln!("Failed to send subscribe messages: {e}");
                    std::thread::sleep(backoff);
                    continue;
                }

                if let tungstenite::stream::MaybeTlsStream::Plain(stream) = socket.get_ref() {
                    stream.set_read_timeout(Some(Duration::from_millis(200)))?;
                }
                while running.load(Ordering::Relaxed) {
                    match socket.read() {
                        Ok(Message::Text(text)) => {
                            let receive_time = std::time::SystemTime::now();
                            let recv_secs = receive_time
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_secs() as i64)
                                .unwrap_or(0);
                            if let Err(e) =
                                documents.ingest_message(&text, self_urn, recv_secs, config)
                            {
                                eprintln!("Error ingesting notification document: {e}");
                            }
                            let prior_failures = bucketer.counters().apply_failures;
                            if let Err(e) = process_message(
                                &text,
                                self_urn,
                                receive_time,
                                bucketer,
                                config,
                                catalog,
                                sink,
                                &mut recorder,
                            ) {
                                if bucketer.counters().apply_failures == prior_failures && !bucketer.counters().ingest_blocked {
                                    eprintln!("Error processing Signal K message: {e}");
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
                        Err(tungstenite::Error::Io(ref error))
                            if error.kind() == std::io::ErrorKind::WouldBlock
                                || error.kind() == std::io::ErrorKind::TimedOut => {}
                        Err(e) => {
                            if running.load(Ordering::Relaxed) {
                                eprintln!("Signal K stream read error: {e}");
                            }
                            break;
                        }
                    }
                    let prior_failures = bucketer.counters().apply_failures;
                    if let Err(error) = bucketer.retry_pending(std::time::Instant::now(), config, catalog, sink) {
                        if bucketer.counters().apply_failures == prior_failures && !bucketer.counters().ingest_blocked {
                            eprintln!("Ingest retry publication failed: {error}");
                        }
                    }
                }
            }
            Err(e) => {
                if running.load(Ordering::Relaxed) {
                    eprintln!("Signal K connection error ({e}), retrying in {backoff:?}...");
                    std::thread::sleep(backoff);
                    backoff = (backoff * 2).min(max_backoff);
                }
            }
        }
    }

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let result = bucketer.flush_all(config, catalog, sink);
        if bucketer.pending_retry_count() == 0 { result?; break; }
        if std::time::Instant::now() >= deadline {
            return Err(Error::InvalidInput("shutdown has unpublished ingest windows".into()));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

/// Process a single text message from the Signal K stream across multiple stores.
#[allow(clippy::too_many_arguments)]
pub fn process_message_multi(
    text: &str,
    self_urn: &str,
    receive_time: std::time::SystemTime,
    bucketer: &mut MultiStoreBucketer,
    config: &TiConfig,
    catalogs: &BTreeMap<String, &dyn Catalog>,
    sinks: &mut BTreeMap<String, &mut dyn ShardSink>,
    recorder: &mut Option<DeltaRecorder>,
) -> Result<usize> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(0);
    }

    // Try parsing as a delta
    let delta: crate::decode::SignalKDelta = match serde_json::from_str(trimmed) {
        Ok(d) => d,
        Err(_) => return Ok(0), // Ignore non-delta messages (e.g. server hello / responses)
    };

    if let Some(rec) = recorder {
        let _ = rec.record_delta(&delta);
    }

    let recv_secs = receive_time
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (raw_points, meta) = decode_delta(&delta, self_urn, recv_secs);
    for (p, v) in meta {
        if let Some(units) = v.get("units").and_then(|u| u.as_str()) {
            bucketer.register_meta_units(&p, units);
        }
    }

    let mut points_ingested = 0;
    let mut first_error = None;
    for raw in raw_points {
        let norm_points = normalize_point(raw, &config.allow_paths, &config.deny_paths);
        for p in norm_points {
            let result = bucketer.ingest_point(
                &p.context,
                &p.path,
                &p.source,
                p.timestamp,
                &p.value,
                config,
                catalogs,
                sinks,
            );
            if let Err(error) = result {
                if first_error.is_none() { first_error = Some(error); }
            } else {
                points_ingested += 1;
            }
        }
    }

    first_error.map_or(Ok(points_ingested), Err)
}

/// Run the blocking Signal K stream loop with reconnect and exponential backoff across multiple stores.
pub fn run_stream_loop_multi(
    self_urn: &str,
    config: &TiConfig,
    catalogs: &BTreeMap<String, &dyn Catalog>,
    sinks: &mut BTreeMap<String, &mut dyn ShardSink>,
    bucketer: &mut MultiStoreBucketer,
    running: Arc<AtomicBool>,
    mut recorder: Option<DeltaRecorder>,
) -> Result<()> {
    let root = config.resolved_stores()["default"].resolved_root(&config.store_root, "default");
    let mut documents = NotificationDocuments::open(std::path::Path::new(&root))?;
    let url = &config.signal_k.url;
    let token = config.signal_k.token.as_deref();

    let mut backoff = Duration::from_secs(1);
    let max_backoff = Duration::from_secs(30);

    while running.load(Ordering::Relaxed) {
        match connect_signalk(url, token) {
            Ok(mut socket) => {
                backoff = Duration::from_secs(1);
                if let Err(e) = subscribe_signalk(&mut socket) {
                    eprintln!("Failed to send subscribe messages: {e}");
                    std::thread::sleep(backoff);
                    continue;
                }

                if let tungstenite::stream::MaybeTlsStream::Plain(stream) = socket.get_ref() {
                    stream.set_read_timeout(Some(Duration::from_millis(200)))?;
                }
                while running.load(Ordering::Relaxed) {
                    match socket.read() {
                        Ok(Message::Text(text)) => {
                            let receive_time = std::time::SystemTime::now();
                            let recv_secs = receive_time
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_secs() as i64)
                                .unwrap_or(0);
                            if let Err(e) =
                                documents.ingest_message(&text, self_urn, recv_secs, config)
                            {
                                eprintln!("Error ingesting notification document: {e}");
                            }
                            let prior_failures = bucketer.counters().apply_failures;
                            if let Err(e) = process_message_multi(
                                &text,
                                self_urn,
                                receive_time,
                                bucketer,
                                config,
                                catalogs,
                                sinks,
                                &mut recorder,
                            ) {
                                if bucketer.counters().apply_failures == prior_failures && !bucketer.counters().ingest_blocked {
                                    eprintln!("Error processing Signal K message: {e}");
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
                        Err(tungstenite::Error::Io(ref error))
                            if error.kind() == std::io::ErrorKind::WouldBlock
                                || error.kind() == std::io::ErrorKind::TimedOut => {}
                        Err(e) => {
                            if running.load(Ordering::Relaxed) {
                                eprintln!("Signal K stream read error: {e}");
                            }
                            break;
                        }
                    }
                    let prior_failures = bucketer.counters().apply_failures;
                    if let Err(error) = bucketer.retry_pending(std::time::Instant::now(), config, catalogs, sinks) {
                        if bucketer.counters().apply_failures == prior_failures && !bucketer.counters().ingest_blocked {
                            eprintln!("Ingest retry publication failed: {error}");
                        }
                    }
                }
            }
            Err(e) => {
                if running.load(Ordering::Relaxed) {
                    eprintln!("Signal K connection error ({e}), retrying in {backoff:?}...");
                    std::thread::sleep(backoff);
                    backoff = (backoff * 2).min(max_backoff);
                }
            }
        }
    }

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let result = bucketer.flush_all(config, catalogs, sinks);
        if bucketer.pending_retry_count() == 0 { result?; break; }
        if std::time::Instant::now() >= deadline {
            return Err(Error::InvalidInput("shutdown has unpublished ingest windows".into()));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn test_subscription_messages_json() {
        let (sub_all, sub_notif) = build_subscription_messages();
        let val_all: serde_json::Value = serde_json::from_str(&sub_all).unwrap();
        assert_eq!(val_all["context"], "vessels.self");
        assert_eq!(val_all["subscribe"][0]["path"], "*");
        assert_eq!(val_all["subscribe"][0]["period"], 1000);
        assert_eq!(val_all["subscribe"][0]["policy"], "instant");

        let val_notif: serde_json::Value = serde_json::from_str(&sub_notif).unwrap();
        assert_eq!(val_notif["subscribe"][0]["path"], "notifications.*");
        assert_eq!(val_notif["subscribe"][0]["policy"], "instant");
    }

    #[test]
    fn test_websocket_mock_server_connect_and_stream() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let local_addr = listener.local_addr().unwrap();

        let server_thread = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();

            // Receive subscription message 1
            let msg1 = ws.read().unwrap();
            assert!(msg1.is_text());

            // Receive subscription message 2
            let msg2 = ws.read().unwrap();
            assert!(msg2.is_text());

            // Send a delta
            let delta = serde_json::json!({
                "context": "vessels.self",
                "updates": [{
                    "$source": "n2k.115",
                    "timestamp": "2026-03-03T04:00:00.000Z",
                    "values": [{
                        "path": "navigation.speedOverGround",
                        "value": 6.8
                    }]
                }]
            });
            ws.send(Message::Text(delta.to_string())).unwrap();

            // Close connection
            ws.close(None).unwrap();
        });

        let url = format!("ws://{local_addr}/signalk/v1/stream?subscribe=none");
        let mut socket = connect_signalk(&url, Some("test_token_123")).unwrap();
        subscribe_signalk(&mut socket).unwrap();

        let msg = socket.read().unwrap();
        assert!(msg.is_text());
        let text = msg.into_text().unwrap();

        // Process message through bucketer
        let tmp = tempfile::tempdir().unwrap();
        let config = TiConfig::default();
        let mut store = ti_store::Store::open_or_create(tmp.path(), config.width_seconds).unwrap();
        let catalog = std::sync::Arc::clone(store.catalog());
        let mut bucketer = WatermarkBucketer::new(&config);

        let count = process_message(
            &text,
            "vessels.urn:mrn:imo:mmsi:230999999",
            std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1772510400),
            &mut bucketer,
            &config,
            catalog.as_ref(),
            &mut store,
            &mut None,
        )
        .unwrap();

        assert_eq!(count, 1);
        bucketer
            .flush_all(&config, catalog.as_ref(), &mut store)
            .unwrap();

        server_thread.join().unwrap();
    }

    #[test]
    fn test_websocket_process_message_multi() {
        use ti_contracts::StoreConfig;

        let tmp_default = tempfile::tempdir().unwrap();
        let tmp_hr = tempfile::tempdir().unwrap();

        let mut config = TiConfig::default();
        let mut stores = BTreeMap::new();
        stores.insert(
            "default".to_string(),
            StoreConfig {
                width: "10s".to_string(),
                retention: "30d".to_string(),
                ..Default::default()
            },
        );
        stores.insert(
            "hr".to_string(),
            StoreConfig {
                width: "1s".to_string(),
                retention: "7d".to_string(),
                paths: Some(vec!["navigation.*".to_string()]),
                ..Default::default()
            },
        );
        config.stores = stores;

        let mut default_store = ti_store::Store::open_or_create(tmp_default.path(), 10).unwrap();
        let default_catalog = std::sync::Arc::clone(default_store.catalog());

        let mut hr_store = ti_store::Store::open_or_create(tmp_hr.path(), 1).unwrap();
        let hr_catalog = std::sync::Arc::clone(hr_store.catalog());

        let mut catalogs: BTreeMap<String, &dyn Catalog> = BTreeMap::new();
        catalogs.insert("default".to_string(), default_catalog.as_ref());
        catalogs.insert("hr".to_string(), hr_catalog.as_ref());

        let mut sinks: BTreeMap<String, &mut dyn ShardSink> = BTreeMap::new();
        sinks.insert("default".to_string(), &mut default_store);
        sinks.insert("hr".to_string(), &mut hr_store);

        let mut bucketer = MultiStoreBucketer::new(&config).unwrap();

        let delta = serde_json::json!({
            "context": "vessels.self",
            "updates": [{
                "$source": "n2k.115",
                "timestamp": "2026-03-03T04:00:00.000Z",
                "values": [{
                    "path": "navigation.speedOverGround",
                    "value": 6.8
                }]
            }]
        });

        let count = process_message_multi(
            &delta.to_string(),
            "vessels.urn:mrn:imo:mmsi:230999999",
            std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1772510400),
            &mut bucketer,
            &config,
            &catalogs,
            &mut sinks,
            &mut None,
        )
        .unwrap();

        assert_eq!(count, 1);
        bucketer.flush_all(&config, &catalogs, &mut sinks).unwrap();
    }

    #[test]
    fn test_websocket_receive_time_skew_and_epoch_prevention() {
        use ti_contracts::{ShardSink, ShardSource};

        let tmp = tempfile::tempdir().unwrap();
        let config = TiConfig::default();
        let mut store = ti_store::Store::open_or_create(tmp.path(), config.width_seconds).unwrap();
        let catalog = std::sync::Arc::clone(store.catalog());
        let mut bucketer = WatermarkBucketer::new(&config);

        // Injected receive time: 2026-06-01T12:00:00Z (1_780_315_200)
        let recv_secs = 1_780_315_200i64;
        let receive_time =
            std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(recv_secs as u64);

        // 1. Delta within 5 min (120 s ahead): 2026-06-01T12:02:00Z (1_780_315_320)
        let delta_close = serde_json::json!({
            "context": "vessels.self",
            "updates": [{
                "$source": "n2k.115",
                "timestamp": "2026-06-01T12:02:00.000Z",
                "values": [{
                    "path": "navigation.speedOverGround",
                    "value": 5.5
                }]
            }]
        });

        // 2. Delta > 5 min off (2 hours behind): 2026-06-01T10:00:00Z (1_780_308_000)
        let delta_skewed = serde_json::json!({
            "context": "vessels.self",
            "updates": [{
                "$source": "n2k.115",
                "timestamp": "2026-06-01T10:00:00.000Z",
                "values": [{
                    "path": "navigation.speedOverGround",
                    "value": 6.0
                }]
            }]
        });

        // 3. Delta at 2020-01-01 EPOCH (1_577_836_800)
        let delta_epoch = serde_json::json!({
            "context": "vessels.self",
            "updates": [{
                "$source": "n2k.115",
                "timestamp": "2020-01-01T00:00:00.000Z",
                "values": [{
                    "path": "navigation.speedOverGround",
                    "value": 7.0
                }]
            }]
        });

        let urn = "vessels.urn:mrn:imo:mmsi:230999999";
        let c1 = process_message(
            &delta_close.to_string(),
            urn,
            receive_time,
            &mut bucketer,
            &config,
            catalog.as_ref(),
            &mut store,
            &mut None,
        )
        .unwrap();
        assert_eq!(c1, 1);

        let c2 = process_message(
            &delta_skewed.to_string(),
            urn,
            receive_time,
            &mut bucketer,
            &config,
            catalog.as_ref(),
            &mut store,
            &mut None,
        )
        .unwrap();
        assert_eq!(c2, 1);

        let c3 = process_message(
            &delta_epoch.to_string(),
            urn,
            receive_time,
            &mut bucketer,
            &config,
            catalog.as_ref(),
            &mut store,
            &mut None,
        )
        .unwrap();
        assert_eq!(c3, 1);

        bucketer
            .flush_all(&config, catalog.as_ref(), &mut store)
            .unwrap();

        // Verify that bucketer event times and all shards are in 2026, never 2020 (EPOCH).
        assert!(bucketer.max_event_time() >= recv_secs);

        let shard_keys = store.shards(None, 0, u32::MAX);
        assert!(!shard_keys.is_empty(), "Store should have open shards");

        for key in shard_keys {
            // Shard index 0 corresponds to 2020-01-01 (EPOCH). 2026 shards are >= 300.
            assert!(
                key.shard >= 300,
                "Shard index {} was near EPOCH instead of 2026",
                key.shard
            );
            let entry = store.seal(key).unwrap();
            let ts_from = ti_contracts::EPOCH + (entry.from as i64 * config.width_seconds as i64);
            let ts_to = ti_contracts::EPOCH + ((entry.to as i64 + 1) * config.width_seconds as i64);
            assert!(
                ts_from >= 1_700_000_000,
                "Sealed shard ts_from {} was near EPOCH instead of 2026",
                ts_from
            );
            assert!(
                ts_to >= 1_700_000_000,
                "Sealed shard ts_to {} was near EPOCH instead of 2026",
                ts_to
            );
        }
    }

    #[test]
    fn test_websocket_bad_notification_frame_does_not_stop_loop() {
        use ti_contracts::ShardSource;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let local_addr = listener.local_addr().unwrap();

        let running = Arc::new(AtomicBool::new(true));
        let running_server = Arc::clone(&running);

        let now = chrono::Utc::now();
        let ts1 = (now - chrono::Duration::seconds(100)).to_rfc3339();
        let ts_bad = (now - chrono::Duration::seconds(80)).to_rfc3339();
        let ts2 = (now - chrono::Duration::seconds(50)).to_rfc3339();
        let ts2_clone = ts2.clone();

        let server_thread = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();

            // Receive subscription messages
            let _ = ws.read().unwrap();
            let _ = ws.read().unwrap();

            // 1. Good delta 1
            let delta1 = serde_json::json!({
                "context": "vessels.self",
                "updates": [{
                    "$source": "n2k.115",
                    "timestamp": ts1,
                    "values": [{
                        "path": "navigation.speedOverGround",
                        "value": 5.1
                    }]
                }]
            });
            ws.send(Message::Text(delta1.to_string())).unwrap();

            // 2. Bad notification frame: invalid notification state triggers an error in NotificationDocuments::ingest
            let bad_notif = serde_json::json!({
                "context": "vessels.self",
                "updates": [{
                    "$source": "n2k.115",
                    "timestamp": ts_bad,
                    "values": [{
                        "path": "notifications.security",
                        "value": {
                            "state": "invalid_bogus_state",
                            "message": "test error"
                        }
                    }]
                }]
            });
            ws.send(Message::Text(bad_notif.to_string())).unwrap();

            // 3. Good delta 2 (different 10 s bucket so universe has 2 buckets)
            let delta2 = serde_json::json!({
                "context": "vessels.self",
                "updates": [{
                    "$source": "n2k.115",
                    "timestamp": ts2_clone,
                    "values": [{
                        "path": "navigation.speedOverGround",
                        "value": 5.8
                    }]
                }]
            });
            ws.send(Message::Text(delta2.to_string())).unwrap();

            // Ping to verify client has processed delta2
            ws.send(Message::Ping(vec![])).unwrap();
            assert!(matches!(ws.read().unwrap(), Message::Pong(_)));

            // Signal shutdown and close connection
            running_server.store(false, Ordering::Relaxed);
            ws.close(None).unwrap();
        });

        let tmp = tempfile::tempdir().unwrap();
        let config = TiConfig {
            store_root: tmp.path().to_string_lossy().into_owned(),
            signal_k: ti_contracts::SignalKConfig {
                url: format!("ws://{local_addr}/signalk/v1/stream?subscribe=none"),
                ..Default::default()
            },
            ..Default::default()
        };

        let mut store = ti_store::Store::open_or_create(tmp.path(), config.width_seconds).unwrap();
        let catalog = std::sync::Arc::clone(store.catalog());
        let mut bucketer = WatermarkBucketer::new(&config);

        let running_client = Arc::clone(&running);
        let stream_res = run_stream_loop(
            "vessels.urn:mrn:imo:mmsi:230999999",
            &config,
            catalog.as_ref(),
            &mut store,
            &mut bucketer,
            running_client,
            None,
        );

        assert!(
            stream_res.is_ok(),
            "run_stream_loop should not fail on bad notification: {:?}",
            stream_res
        );
        server_thread.join().unwrap();

        // Verify that both good deltas were processed into store open shards
        let ts2_secs = chrono::DateTime::parse_from_rfc3339(&ts2)
            .unwrap()
            .timestamp();
        let ts2_bucket = ((ts2_secs - ti_contracts::EPOCH) / (config.width_seconds as i64)) as u32;

        let ts2_col = ts2_bucket & 0xffff;

        let shard_keys = store.shards(None, 0, u32::MAX);
        assert!(!shard_keys.is_empty(), "Store should contain open shards");
        let mut delta2_found = false;
        let mut total_records = 0;
        for key in &shard_keys {
            let shard = store.open_shard(key).unwrap();
            total_records += shard.data.universe().len();
            if shard.data.universe().contains(ts2_col) {
                delta2_found = true;
            }
        }
        assert!(
            delta2_found,
            "Delta 2 bucket must be present in store after bad notification frame"
        );
        assert_eq!(
            total_records, 3,
            "Expected 3 buckets across stream including delta 2"
        );
    }
}
