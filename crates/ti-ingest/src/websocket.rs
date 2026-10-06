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
pub fn process_message(
    text: &str,
    self_urn: &str,
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

    let (raw_points, meta) = decode_delta(&delta, self_urn, bucketer.max_event_time());
    for (p, v) in meta {
        if let Some(units) = v.get("units").and_then(|u| u.as_str()) {
            bucketer.classifier_mut().register_meta_units(&p, units);
        }
    }

    let mut points_ingested = 0;
    for raw in raw_points {
        let norm_points = normalize_point(raw, &config.allow_paths, &config.deny_paths);
        for p in norm_points {
            bucketer.ingest_point(
                &p.context,
                &p.path,
                &p.source,
                p.timestamp,
                p.value,
                config,
                catalog,
                sink,
            )?;
            points_ingested += 1;
        }
    }

    Ok(points_ingested)
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

                while running.load(Ordering::Relaxed) {
                    match socket.read() {
                        Ok(Message::Text(text)) => {
                            documents.ingest_message(
                                &text, self_urn, chrono::Utc::now().timestamp(), config,
                            )?;
                            if let Err(e) = process_message(
                                &text,
                                self_urn,
                                bucketer,
                                config,
                                catalog,
                                sink,
                                &mut recorder,
                            ) {
                                eprintln!("Error processing Signal K message: {e}");
                            }
                        }
                        Ok(Message::Ping(payload)) => {
                            let _ = socket.send(Message::Pong(payload));
                        }
                        Ok(Message::Close(_)) => {
                            break;
                        }
                        Ok(_) => {}
                        Err(e) => {
                            if running.load(Ordering::Relaxed) {
                                eprintln!("Signal K stream read error: {e}");
                            }
                            break;
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

    bucketer.flush_all(config, catalog, sink)?;
    Ok(())
}

/// Process a single text message from the Signal K stream across multiple stores.
pub fn process_message_multi(
    text: &str,
    self_urn: &str,
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

    let (raw_points, meta) = decode_delta(&delta, self_urn, bucketer.max_event_time());
    for (p, v) in meta {
        if let Some(units) = v.get("units").and_then(|u| u.as_str()) {
            bucketer.register_meta_units(&p, units);
        }
    }

    let mut points_ingested = 0;
    for raw in raw_points {
        let norm_points = normalize_point(raw, &config.allow_paths, &config.deny_paths);
        for p in norm_points {
            bucketer.ingest_point(
                &p.context,
                &p.path,
                &p.source,
                p.timestamp,
                &p.value,
                config,
                catalogs,
                sinks,
            )?;
            points_ingested += 1;
        }
    }

    Ok(points_ingested)
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

                while running.load(Ordering::Relaxed) {
                    match socket.read() {
                        Ok(Message::Text(text)) => {
                            documents.ingest_message(
                                &text, self_urn, chrono::Utc::now().timestamp(), config,
                            )?;
                            if let Err(e) = process_message_multi(
                                &text,
                                self_urn,
                                bucketer,
                                config,
                                catalogs,
                                sinks,
                                &mut recorder,
                            ) {
                                eprintln!("Error processing Signal K message: {e}");
                            }
                        }
                        Ok(Message::Ping(payload)) => {
                            let _ = socket.send(Message::Pong(payload));
                        }
                        Ok(Message::Close(_)) => {
                            break;
                        }
                        Ok(_) => {}
                        Err(e) => {
                            if running.load(Ordering::Relaxed) {
                                eprintln!("Signal K stream read error: {e}");
                            }
                            break;
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

    bucketer.flush_all(config, catalogs, sinks)?;
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
}
