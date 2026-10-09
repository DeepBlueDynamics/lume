use arrow_array::builder::{Float64Builder, StringBuilder};
use arrow_array::RecordBatch;
use arrow_schema::{DataType, Field, Schema};
use parquet::arrow::ArrowWriter;
use std::collections::HashSet;
use std::sync::Arc;
use tempfile::{tempdir, NamedTempFile};
use ti_contracts::{ShardKey, ShardSink, TiConfig};
use ti_ingest::{
    backfill_parquet_file, BackfillStatus, DeltaRecorder, DeltaReplay, SignalKDelta,
    WatermarkBucketer,
};
use ti_store::Store;

#[test]
fn test_recorder_and_deterministic_replay() {
    let tmp_rec = NamedTempFile::new().unwrap();
    let self_urn = "vessels.urn:mrn:imo:mmsi:230999999";

    // 1. Record deltas to NDJSON
    let mut recorder = DeltaRecorder::create(tmp_rec.path()).unwrap();
    for i in 0..5 {
        let delta: SignalKDelta = serde_json::from_value(serde_json::json!({
            "context": "vessels.self",
            "updates": [{
                "$source": "n2k.115",
                "timestamp": format!("2020-01-01T00:00:0{}.000Z", i * 2),
                "values": [
                    { "path": "navigation.speedOverGround", "value": 5.0 + (i as f64) * 0.5 },
                    { "path": "navigation.headingTrue", "value": 1.25 }
                ]
            }]
        }))
        .unwrap();
        recorder.record_delta(&delta).unwrap();
    }

    // 2. Replay deltas into a Store
    let tmp_store = tempdir().unwrap();
    let config = TiConfig::default();
    let mut store = Store::open_or_create(tmp_store.path(), config.width_seconds).unwrap();
    let catalog = Arc::clone(store.catalog());
    let mut bucketer = WatermarkBucketer::new(&config);

    let replay = DeltaReplay::open(tmp_rec.path()).unwrap();
    let ingested_count = replay
        .replay_all(
            self_urn,
            &mut bucketer,
            &config,
            catalog.as_ref(),
            &mut store,
        )
        .unwrap();

    assert_eq!(ingested_count, 10); // 5 updates * 2 paths = 10 points
    store.flush().unwrap();

    // Verify data in open shard
    let shard_key = ShardKey {
        vessel: 0,
        shard: 0,
    };
    let shard = store.open_shard(&shard_key).unwrap();
    assert!(!shard.data.fields.is_empty());
}

#[test]
fn test_parquet_backfill_and_idempotence() {
    let tmp_pq = NamedTempFile::new().unwrap();
    let schema = Arc::new(Schema::new(vec![
        Field::new("context", DataType::Utf8, false),
        Field::new("path", DataType::Utf8, false),
        Field::new("signalk_timestamp", DataType::Utf8, false),
        Field::new("source_label", DataType::Utf8, false),
        Field::new("value_latitude", DataType::Float64, true),
        Field::new("value_longitude", DataType::Float64, true),
        Field::new("value", DataType::Float64, true),
    ]));

    let mut ctx_b = StringBuilder::new();
    let mut path_b = StringBuilder::new();
    let mut ts_b = StringBuilder::new();
    let mut src_b = StringBuilder::new();
    let mut lat_b = Float64Builder::new();
    let mut lon_b = Float64Builder::new();
    let mut val_b = Float64Builder::new();

    // Row 1: navigation.position object
    ctx_b.append_value("vessels.urn:mrn:imo:mmsi:230999999");
    path_b.append_value("navigation.position");
    ts_b.append_value("2020-01-01T00:00:05.000Z");
    src_b.append_value("gps.0");
    lat_b.append_value(59.91);
    lon_b.append_value(10.75);
    val_b.append_null();

    // Row 2: scalar speedOverGround
    ctx_b.append_value("vessels.urn:mrn:imo:mmsi:230999999");
    path_b.append_value("navigation.speedOverGround");
    ts_b.append_value("2020-01-01T00:00:06.000Z");
    src_b.append_value("gps.0");
    lat_b.append_null();
    lon_b.append_null();
    val_b.append_value(7.4);

    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(ctx_b.finish()),
            Arc::new(path_b.finish()),
            Arc::new(ts_b.finish()),
            Arc::new(src_b.finish()),
            Arc::new(lat_b.finish()),
            Arc::new(lon_b.finish()),
            Arc::new(val_b.finish()),
        ],
    )
    .unwrap();

    let mut writer = ArrowWriter::try_new(tmp_pq.reopen().unwrap(), schema, None).unwrap();
    writer.write(&batch).unwrap();
    writer.close().unwrap();

    let self_urn = "vessels.urn:mrn:imo:mmsi:230999999";
    let tmp_store = tempdir().unwrap();
    let config = TiConfig::default();
    let mut store = Store::open_or_create(tmp_store.path(), config.width_seconds).unwrap();
    let catalog = Arc::clone(store.catalog());

    // 1. Initial backfill ingestion
    let status1 = backfill_parquet_file(
        tmp_pq.path(),
        self_urn,
        None,
        &config,
        catalog.as_ref(),
        &mut store,
    )
    .unwrap();

    let file_hash = match status1 {
        BackfillStatus::Ingested {
            hash,
            rows_read,
            buckets_emitted,
        } => {
            assert_eq!(rows_read, 2);
            assert!(buckets_emitted > 0);
            hash
        }
        BackfillStatus::Skipped { .. } => panic!("first backfill should not be skipped"),
    };

    // 2. Second backfill with manifest hash -> skipped!
    let mut manifest_hashes = HashSet::new();
    manifest_hashes.insert(file_hash);

    let status2 = backfill_parquet_file(
        tmp_pq.path(),
        self_urn,
        Some(&manifest_hashes),
        &config,
        catalog.as_ref(),
        &mut store,
    )
    .unwrap();

    match status2 {
        BackfillStatus::Skipped { hash } => assert_eq!(hash, file_hash),
        BackfillStatus::Ingested { .. } => panic!("second backfill should have been skipped"),
    }

    // 3. Clear-and-rewrite idempotence: backfill without skipping applies cleanly
    let status3 = backfill_parquet_file(
        tmp_pq.path(),
        self_urn,
        None,
        &config,
        catalog.as_ref(),
        &mut store,
    )
    .unwrap();

    assert!(matches!(status3, BackfillStatus::Ingested { .. }));
}
