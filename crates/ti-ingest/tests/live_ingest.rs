use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use ti_contracts::{ShardKey, ShardSource, TiConfig};
use ti_ingest::service::IngestService;
use ti_store::Store;
use tungstenite::Message;

fn get_scratch_dir() -> PathBuf {
    let base = std::env::var_os("CARGO_TARGET_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let scratch = base.join(format!("ti-live-ingest-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).unwrap();
    scratch
}

#[test]
fn test_live_ingest_service_stream_seal_and_replay() {
    let scratch = get_scratch_dir();
    let store_root = scratch.join("store");
    std::fs::create_dir_all(&store_root).unwrap();

    // 1. Start mock Signal K WebSocket server
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let local_port = listener.local_addr().unwrap().port();

    let server_handle = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut ws = tungstenite::accept(stream).unwrap();

        // Read subscriptions
        let msg1 = ws.read().unwrap();
        assert!(msg1.is_text());
        let msg2 = ws.read().unwrap();
        assert!(msg2.is_text());

        // Send hello frame with self URN
        let hello = serde_json::json!({
            "name": "signalk-mock",
            "version": "1.0.0",
            "self": "urn:mrn:imo:mmsi:367000000"
        });
        ws.send(Message::Text(hello.to_string())).unwrap();

        // Delta 1: in shard 0 (timestamp 1578492000, shard 0 ends at 1578492160)
        let d1 = serde_json::json!({
            "context": "vessels.self",
            "updates": [{
                "$source": "n2k.115",
                "timestamp": "2020-01-08T14:00:00.000Z", // 1578492000
                "values": [
                    { "path": "navigation.speedOverGround", "value": 5.5 },
                    { "path": "environment.wind.speedTrue", "value": 12.0 }
                ]
            }]
        });
        ws.send(Message::Text(d1.to_string())).unwrap();

        // Small sleep so delta 1 is consumed
        thread::sleep(Duration::from_millis(50));

        // Delta 2: in shard 0 (timestamp 1578492100, closes delta 1 bucket via watermark)
        let d2 = serde_json::json!({
            "context": "vessels.self",
            "updates": [{
                "$source": "n2k.115",
                "timestamp": "2020-01-08T14:01:40.000Z", // 1578492100
                "values": [
                    { "path": "navigation.speedOverGround", "value": 6.0 }
                ]
            }]
        });
        ws.send(Message::Text(d2.to_string())).unwrap();

        // Wait a bit before sending delta 3
        thread::sleep(Duration::from_millis(150));

        // Delta 3: in shard 1 (timestamp 1578492170)
        let d3 = serde_json::json!({
            "context": "vessels.self",
            "updates": [{
                "$source": "n2k.115",
                "timestamp": "2020-01-08T14:02:50.000Z", // 1578492170
                "values": [
                    { "path": "navigation.speedOverGround", "value": 7.2 }
                ]
            }]
        });
        ws.send(Message::Text(d3.to_string())).unwrap();

        // Keep connection open until client disconnects or closes
        while let Ok(msg) = ws.read() {
            if msg.is_close() {
                break;
            }
        }
    });

    // 2. Configure IngestService with injected clock
    let config = TiConfig {
        store_root: store_root.to_str().unwrap().to_string(),
        signal_k: ti_contracts::SignalKConfig {
            url: format!("ws://127.0.0.1:{local_port}/signalk/v1/stream?subscribe=none"),
            ..Default::default()
        },
        ..Default::default()
    };

    let clock_time = Arc::new(AtomicI64::new(1578492000));
    let clock_clone = clock_time.clone();
    let clock_fn = Arc::new(move || clock_clone.load(Ordering::Relaxed));

    let running = Arc::new(AtomicBool::new(true));
    let mut service = IngestService::new(config.clone(), running.clone());
    service.set_clock(clock_fn);

    // 3. Run IngestService in a thread
    let service_handle = thread::spawn(move || service.run());

    // Let service process Delta 1 and Delta 2
    thread::sleep(Duration::from_millis(200));

    // Advance injected clock past shard 0 end + 1 hour (shard 0 ends at 1578492160)
    // 1578492160 + 3601 = 1578495761
    clock_time.store(1578495761, Ordering::Relaxed);

    // Wait for maintenance tick in service to seal shard 0
    thread::sleep(Duration::from_millis(400));

    // 4. Request clean shutdown
    running.store(false, Ordering::Relaxed);
    let res = service_handle.join().unwrap();
    assert!(res.is_ok(), "IngestService run failed: {:?}", res);

    let _ = server_handle.join();

    // 5. Verify status file
    let status_path = store_root.join("ingest_status.json");
    assert!(status_path.is_file(), "ingest_status.json must be created");
    let status_content = std::fs::read_to_string(&status_path).unwrap();
    let status_json: serde_json::Value = serde_json::from_str(&status_content).unwrap();
    assert_eq!(status_json["running"], false);
    assert!(status_json["records_ingested"].as_u64().unwrap() >= 3);

    // 6. Verify Store state: Shard 0 must be sealed in manifest
    {
        let store = Store::open_or_create(&store_root, 10).unwrap();
        let sealed_keys = store.shards(None, 0, u32::MAX);
        let key0 = ShardKey {
            vessel: 0,
            shard: 0,
        };
        assert!(
            sealed_keys.contains(&key0),
            "Shard 0 must be sealed in manifest; found {:?}",
            sealed_keys
        );

        // 7. Verify Shard 1 replayed from WAL and has records with no duplicates
        let key1 = ShardKey {
            vessel: 0,
            shard: 1,
        };
        let open_shard1 = store.open_shard(&key1);
        assert!(
            open_shard1.is_some(),
            "Shard 1 must be open and recovered via WAL replay"
        );

        let shard1 = open_shard1.unwrap();
        assert_eq!(
            shard1.data.universe().len(),
            1,
            "Shard 1 must contain exactly 1 row from Delta 3"
        );
    }

    // 8. Re-open again to prove idempotence of WAL replay
    {
        let store = Store::open_or_create(&store_root, 10).unwrap();
        let key1 = ShardKey {
            vessel: 0,
            shard: 1,
        };
        let shard1 = store.open_shard(&key1).unwrap();
        assert_eq!(
            shard1.data.universe().len(),
            1,
            "Re-opening store must not duplicate records replayed from WAL"
        );
    }

    // Clean up scratch directory
    let _ = std::fs::remove_dir_all(&scratch);
}
