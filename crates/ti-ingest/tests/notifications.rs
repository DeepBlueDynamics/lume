//! W10 notification parquet backfill and deterministic replay.
use arrow_array::{RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use parquet::arrow::ArrowWriter;
use std::sync::Arc;
use ti_contracts::TiConfig;
use ti_ingest::backfill_directory;
use ti_store::{DocStore, Store};

#[test]
fn parquet_episodes_span_files_and_replays_preserve_ids() {
    let dir = tempfile::Builder::new()
        .prefix("notifications-")
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap();
    let raw = dir.path().join("raw");
    std::fs::create_dir_all(&raw).unwrap();
    let root = dir.path().join("store");
    let config = TiConfig {
        store_root: root.to_string_lossy().into_owned(),
        ..Default::default()
    };
    let urn = "vessels.urn:mrn:signalk:uuid:test";
    let schema = Arc::new(Schema::new(vec![
        Field::new("context", DataType::Utf8, false),
        Field::new("path", DataType::Utf8, false),
        Field::new("signalk_timestamp", DataType::Utf8, false),
        Field::new("value_json", DataType::Utf8, false),
    ]));
    let events = [
        ("2026-06-01T00:00:00Z", "warn", "battery low"),
        ("2026-06-01T00:00:10Z", "alarm", "battery critical"),
        ("2026-06-01T00:00:30Z", "normal", ""),
        ("2026-06-01T00:01:00Z", "warn", "battery low again"),
    ];
    for (i, &(ts, state, message)) in events.iter().enumerate() {
        let value = serde_json::json!({"state": state, "message": message, "method": ["sound"]})
            .to_string();
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(StringArray::from(vec![urn])),
                Arc::new(StringArray::from(vec!["notifications.electrical.battery"])),
                Arc::new(StringArray::from(vec![ts])),
                Arc::new(StringArray::from(vec![value.as_str()])),
            ],
        )
        .unwrap();
        let file = std::fs::File::create(raw.join(format!("{i}.parquet"))).unwrap();
        let mut writer = ArrowWriter::try_new(file, schema.clone(), None).unwrap();
        writer.write(&batch).unwrap();
        writer.close().unwrap();
    }
    let mut store = Store::open_or_create(&root, 10).unwrap();
    let catalog = Arc::clone(store.catalog());
    backfill_directory(&raw, urn, None, &config, catalog.as_ref(), &mut store).unwrap();
    let docs: Vec<_> = DocStore::open(&root).unwrap().iter().cloned().collect();
    assert_eq!(docs.len(), 2);
    assert_eq!(docs[0].ts_end, Some(docs[0].ts_start + 30));
    assert!(docs[0].body.contains("battery critical"));
    assert!(docs[0].body.contains("sound"));
    assert_eq!(docs[1].ts_end, None);
    backfill_directory(&raw, urn, None, &config, catalog.as_ref(), &mut store).unwrap();
    assert_eq!(
        DocStore::open(&root)
            .unwrap()
            .iter()
            .cloned()
            .collect::<Vec<_>>(),
        docs
    );
}

#[test]
fn both_live_loops_persist_notification_lifecycles() {
    use std::collections::BTreeMap;
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use ti_contracts::{Catalog, ShardSink};
    use ti_ingest::{MultiStoreBucketer, WatermarkBucketer};
    use tungstenite::Message;

    for multi in [false, true] {
        let dir = tempfile::Builder::new()
            .prefix("live-notifications-")
            .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
            .unwrap();
        let root = dir.path().join("store");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let running = Arc::new(AtomicBool::new(true));
        let server_running = Arc::clone(&running);
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();
            ws.read().unwrap();
            ws.read().unwrap();
            let start = chrono::Utc::now().timestamp() - 30;
            for (offset, state) in [(0, "warn"), (10, "alarm"), (30, "normal")] {
                let ts = chrono::DateTime::from_timestamp(start + offset, 0)
                    .unwrap()
                    .to_rfc3339();
                let delta = serde_json::json!({
                    "context": "vessels.self",
                    "updates": [{"timestamp": ts, "values": [{
                        "path": "notifications.battery",
                        "value": {"state": state, "message": "battery low", "method": ["sound"]}
                    }]}]
                });
                ws.send(Message::Text(delta.to_string())).unwrap();
            }
            // The pong confirms the client processed the preceding clear frame.
            ws.send(Message::Ping(vec![1])).unwrap();
            assert!(matches!(ws.read().unwrap(), Message::Pong(_)));
            server_running.store(false, Ordering::Relaxed);
            ws.close(None).unwrap();
        });
        let config = TiConfig {
            store_root: root.to_string_lossy().into_owned(),
            signal_k: ti_contracts::SignalKConfig {
                url: format!("ws://{address}/signalk/v1/stream"),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut store = Store::open_or_create(&root, 10).unwrap();
        let catalog = Arc::clone(store.catalog());
        if multi {
            let catalogs =
                BTreeMap::from([("default".to_owned(), catalog.as_ref() as &dyn Catalog)]);
            let mut sinks =
                BTreeMap::from([("default".to_owned(), &mut store as &mut dyn ShardSink)]);
            ti_ingest::run_stream_loop_multi(
                "vessels.urn:mrn:signalk:uuid:test",
                &config,
                &catalogs,
                &mut sinks,
                &mut MultiStoreBucketer::new(&config).unwrap(),
                running,
                None,
            )
            .unwrap();
        } else {
            ti_ingest::run_stream_loop(
                "vessels.urn:mrn:signalk:uuid:test",
                &config,
                catalog.as_ref(),
                &mut store,
                &mut WatermarkBucketer::new(&config),
                running,
                None,
            )
            .unwrap();
        }
        server.join().unwrap();
        let docs = DocStore::open(&root).unwrap();
        assert_eq!(docs.len(), 1);
        let doc = docs.iter().next().unwrap();
        assert_eq!(doc.ts_end, Some(doc.ts_start + 30));
        assert!(doc.title.ends_with("(alarm)"));
        assert!(doc.body.contains("battery low"));
    }
}
