//! Integration tests for Signal K synthetic feed:
//! 1. Hello and subscribe message handling.
//! 2. Rate accuracy (±5% over 10 s) on loopback.
//!
//! Wrapped in hard 45 s timeout to guarantee fast failure without hangs.

use std::{
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
use ti_bench::sk_feed::FeedConfig;
use tungstenite::{connect, Message};

fn run_with_timeout<F>(timeout: Duration, f: F)
where
    F: FnOnce() + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    let thread_handle = thread::spawn(move || {
        f();
        let _ = tx.send(());
    });
    match rx.recv_timeout(timeout) {
        Ok(()) => {
            thread_handle.join().expect("Test thread panicked");
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("Test timed out after {:?}", timeout);
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("Test thread disconnected without completing");
        }
    }
}

#[test]
fn test_hello_and_subscribe_handling() {
    run_with_timeout(Duration::from_secs(40), || {
        let config = FeedConfig {
            bind: "127.0.0.1".into(),
            port: 0,
            values_per_sec: 2000,
            batch_size: 20,
            vessels: 5,
            seed: 12345,
            duration_sec: Some(30),
            max_values: Some(10_000),
            ramp: None,
            self_urn: "urn:mrn:imo:mmsi:367000000".into(),
        };

        let feed =
            ti_bench::sk_feed::start_test_feed(config).expect("Failed to start synthetic feed");

        let url = format!("ws://{}/signalk/v1/stream?subscribe=none", feed.addr);
        let (mut ws, _) = connect(&url).expect("Failed to connect websocket");

        // Give the client socket a 5s read timeout
        if let tungstenite::stream::MaybeTlsStream::Plain(ref s) = *ws.get_ref() {
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        }

        // 1. First message must be the Hello frame
        let msg1 = ws.read().expect("Failed to read hello frame");
        let hello_text = match msg1 {
            Message::Text(t) => t,
            other => panic!("Expected text message for hello, got {other:?}"),
        };
        let hello: serde_json::Value = serde_json::from_str(&hello_text).unwrap();
        assert_eq!(hello["name"], "signalk-synthetic-feed");
        assert_eq!(hello["self"], "urn:mrn:imo:mmsi:367000000");

        // 2. Since subscribe=none was requested, no deltas must arrive before subscribe
        if let tungstenite::stream::MaybeTlsStream::Plain(ref s) = *ws.get_ref() {
            s.set_read_timeout(Some(Duration::from_millis(150)))
                .unwrap();
        }
        let poll_res = ws.read();
        assert!(
            poll_res.is_err(),
            "Expected timeout/no message before subscribe, but got {poll_res:?}"
        );

        // 3. Send subscription for a single specific path
        if let tungstenite::stream::MaybeTlsStream::Plain(ref s) = *ws.get_ref() {
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        }
        let sub_single = serde_json::json!({
            "context": "vessels.self",
            "subscribe": [{
                "path": "navigation.speedOverGround",
                "period": 1000,
                "policy": "instant"
            }]
        });
        ws.send(Message::Text(sub_single.to_string())).unwrap();

        // Verify received deltas only contain navigation.speedOverGround
        for _ in 0..5 {
            let msg = ws.read().expect("Expected delta after subscribe");
            if let Message::Text(txt) = msg {
                let val: serde_json::Value = serde_json::from_str(&txt).unwrap();
                let updates = val["updates"].as_array().expect("updates array");
                for update in updates {
                    let values = update["values"].as_array().expect("values array");
                    for v in values {
                        assert_eq!(
                            v["path"], "navigation.speedOverGround",
                            "Expected filtered path navigation.speedOverGround, got {}",
                            v["path"]
                        );
                    }
                }
            }
        }

        // 4. Send subscription for all paths wildcard
        let sub_all = serde_json::json!({
            "context": "vessels.self",
            "subscribe": [{
                "path": "*",
                "period": 1000,
                "policy": "instant"
            }]
        });
        ws.send(Message::Text(sub_all.to_string())).unwrap();

        let mut saw_paths = std::collections::HashSet::new();
        let mut saw_contexts = std::collections::HashSet::new();
        for _ in 0..15 {
            let msg = ws.read().expect("Expected delta after wildcard subscribe");
            if let Message::Text(txt) = msg {
                let val: serde_json::Value = serde_json::from_str(&txt).unwrap();
                if let Some(ctx) = val["context"].as_str() {
                    saw_contexts.insert(ctx.to_string());
                }
                if let Some(updates) = val["updates"].as_array() {
                    for u in updates {
                        if let Some(values) = u["values"].as_array() {
                            for v in values {
                                if let Some(p) = v["path"].as_str() {
                                    saw_paths.insert(p.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }

        // Verify multiple paths and multiple contexts are being streamed
        assert!(
            saw_paths.len() > 1,
            "Expected multiple paths from wildcard subscription, got {}",
            saw_paths.len()
        );
        assert!(
            saw_contexts.len() > 1,
            "Expected multiple vessel contexts (self + AIS), got {:?}",
            saw_contexts
        );

        // Graceful close
        let _ = ws.close(None);
        let _ = feed.join();
    });
}

#[test]
fn test_rate_accuracy() {
    run_with_timeout(Duration::from_secs(40), || {
        let target_rate: u64 = 10_000;
        let batch_size: usize = 100;

        let config = FeedConfig {
            bind: "127.0.0.1".into(),
            port: 0,
            values_per_sec: target_rate,
            batch_size,
            vessels: 10,
            seed: 999,
            duration_sec: Some(30),
            max_values: Some(250_000),
            ramp: None,
            self_urn: "urn:mrn:imo:mmsi:367000000".into(),
        };

        let feed =
            ti_bench::sk_feed::start_test_feed(config).expect("Failed to start synthetic feed");

        let url = format!("ws://{}/signalk/v1/stream?subscribe=none", feed.addr);
        let (mut ws, _) = connect(&url).expect("Failed to connect websocket");

        // Give the client socket a 5s read timeout
        if let tungstenite::stream::MaybeTlsStream::Plain(ref s) = *ws.get_ref() {
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        }

        // Consume hello
        let _ = ws.read().expect("Failed to read hello");

        // Send subscribe for all paths
        let sub = serde_json::json!({
            "context": "vessels.self",
            "subscribe": [{
                "path": "*",
                "period": 1000,
                "policy": "instant"
            }]
        });
        ws.send(Message::Text(sub.to_string())).unwrap();

        // Measure received values over 10.0 seconds
        let measurement_duration = Duration::from_secs(10);
        let start = Instant::now();
        let mut total_values: u64 = 0;

        while start.elapsed() < measurement_duration {
            let msg = ws.read().expect("Failed to read delta during rate test");
            if let Message::Text(txt) = msg {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&txt) {
                    if let Some(updates) = val.get("updates").and_then(|u| u.as_array()) {
                        for update in updates {
                            if let Some(values) = update.get("values").and_then(|v| v.as_array()) {
                                total_values += values.len() as u64;
                            }
                        }
                    }
                }
            }
        }

        let elapsed = start.elapsed();
        let measured_rate = total_values as f64 / elapsed.as_secs_f64();
        println!(
            "Rate test: received {total_values} values in {:.3}s -> {:.1} values/s (target: {target_rate})",
            elapsed.as_secs_f64(),
            measured_rate
        );

        // Graceful close and join
        let _ = ws.close(None);
        let _ = feed.join();

        // Spec requires ±5% accuracy over 10s: [9,500, 10,500] for 10,000 values/s
        let min_allowed = (target_rate as f64) * 0.95;
        let max_allowed = (target_rate as f64) * 1.05;

        assert!(
            measured_rate >= min_allowed && measured_rate <= max_allowed,
            "Rate accuracy out of ±5% tolerance: measured {:.1} values/s, expected [{:.1}, {:.1}]",
            measured_rate,
            min_allowed,
            max_allowed
        );
    });
}

/// Lume ingest subscribes to `*` and then `notifications.*` on the same socket.
/// Signal K subscriptions are additive, so the second must not narrow the first.
#[test]
fn test_additive_subscriptions_like_lume_ingest() {
    run_with_timeout(Duration::from_secs(40), || {
        let config = FeedConfig {
            bind: "127.0.0.1".into(),
            port: 0,
            values_per_sec: 2000,
            batch_size: 20,
            vessels: 3,
            seed: 7,
            duration_sec: Some(20),
            max_values: Some(10_000),
            ramp: None,
            self_urn: "urn:mrn:imo:mmsi:367000000".into(),
        };
        let feed =
            ti_bench::sk_feed::start_test_feed(config).expect("Failed to start synthetic feed");
        let url = format!("ws://{}/signalk/v1/stream?subscribe=none", feed.addr);
        let (mut ws, _) = connect(&url).expect("Failed to connect websocket");
        if let tungstenite::stream::MaybeTlsStream::Plain(ref s) = *ws.get_ref() {
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        }
        let _hello = ws.read().expect("hello");
        for path in ["*", "notifications.*"] {
            let sub = serde_json::json!({
                "context": "vessels.*",
                "subscribe": [{"path": path, "policy": "instant"}]
            });
            ws.send(Message::Text(sub.to_string())).unwrap();
        }
        let mut saw_paths = std::collections::HashSet::new();
        let started = Instant::now();
        while saw_paths.len() < 5 && started.elapsed() < Duration::from_secs(10) {
            let msg = ws
                .read()
                .expect("deltas must keep flowing after the second subscribe");
            if let Message::Text(txt) = msg {
                let val: serde_json::Value = serde_json::from_str(&txt).unwrap();
                for u in val["updates"].as_array().into_iter().flatten() {
                    for v in u["values"].as_array().into_iter().flatten() {
                        if let Some(p) = v["path"].as_str() {
                            saw_paths.insert(p.to_string());
                        }
                    }
                }
            }
        }
        assert!(
            saw_paths.len() >= 5,
            "expected the `*` subscription to survive `notifications.*`, saw {saw_paths:?}"
        );
        let _ = ws.close(None);
        let _ = feed.join();
    });
}
