use datafusion::arrow::array::builder::{Float64Builder, StringBuilder};
use datafusion::arrow::array::RecordBatch;
use datafusion::arrow::datatypes::{DataType, Field, Schema};
use parquet::arrow::ArrowWriter;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use ti_contracts::{Catalog, ShardSink, TiConfig};
use ti_ingest::backfill_directory_stores;
use ti_sql::{rows_json, TiEngine};
use ti_store::Store;

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);
impl Scratch {
    fn new(prefix: &str) -> Self {
        let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn create_sample_parquet(path: &Path) {
    let schema = Arc::new(Schema::new(vec![
        Field::new("context", DataType::Utf8, false),
        Field::new("path", DataType::Utf8, false),
        Field::new("signalk_timestamp", DataType::Utf8, false),
        Field::new("source_label", DataType::Utf8, false),
        Field::new("value", DataType::Float64, true),
    ]));

    let mut ctx_b = StringBuilder::new();
    let mut path_b = StringBuilder::new();
    let mut ts_b = StringBuilder::new();
    let mut src_b = StringBuilder::new();
    let mut val_b = Float64Builder::new();

    // 5 data points across two 10-second windows:
    // Window 1: 04:00:00 - 04:00:10 (3 points: seconds 01, 02, 03)
    // Window 2: 04:00:10 - 04:00:20 (2 points: seconds 11, 12)
    let timestamps = [
        "2026-03-03T04:00:01.000Z",
        "2026-03-03T04:00:02.000Z",
        "2026-03-03T04:00:03.000Z",
        "2026-03-03T04:00:11.000Z",
        "2026-03-03T04:00:12.000Z",
    ];
    let speeds = [5.1, 5.2, 5.3, 6.1, 6.2];

    for (ts, spd) in timestamps.iter().zip(speeds.iter()) {
        ctx_b.append_value("vessels.urn:mrn:imo:mmsi:230999999");
        path_b.append_value("navigation.speedOverGround");
        ts_b.append_value(ts);
        src_b.append_value("gps.0");
        val_b.append_value(*spd);
    }

    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(ctx_b.finish()),
            Arc::new(path_b.finish()),
            Arc::new(ts_b.finish()),
            Arc::new(src_b.finish()),
            Arc::new(val_b.finish()),
        ],
    )
    .unwrap();

    let file = std::fs::File::create(path).unwrap();
    let mut writer = ArrowWriter::try_new(file, schema, None).unwrap();
    writer.write(&batch).unwrap();
    writer.close().unwrap();
}

#[tokio::test]
async fn test_multi_store_backfill_and_query_fanout() {
    let store_scratch = Scratch::new("test-ms-store");
    let store_root = &store_scratch.0;

    let pq_scratch = Scratch::new("test-ms-pq");
    let pq_file = pq_scratch.0.join("sample.parquet");
    create_sample_parquet(&pq_file);

    let hr_root = store_root.join("stores").join("telemetry_hr");
    std::fs::create_dir_all(&hr_root).unwrap();

    // Configure TiConfig with 10s default and 1s telemetry_hr store
    let toml_content = format!(
        r#"
width_seconds = 10
store_root = '{}'

[stores.default]
width = "10s"
retention = "30d"

[stores.telemetry_hr]
width = "1s"
retention = "7d"
paths = ["navigation.*"]
"#,
        store_root.display()
    );
    std::fs::write(store_root.join("ti.toml"), &toml_content).unwrap();

    let config = TiConfig::from_toml(&toml_content).unwrap();
    let self_urn = "vessels.urn:mrn:imo:mmsi:230999999";

    let mut default_store = Store::open_or_create(store_root, 10).unwrap();
    let default_cat = default_store.catalog().clone();

    let mut hr_store = Store::open_or_create(&hr_root, 1).unwrap();
    let hr_cat = hr_store.catalog().clone();

    let mut catalogs: BTreeMap<String, &dyn Catalog> = BTreeMap::new();
    catalogs.insert("default".to_string(), default_cat.as_ref());
    catalogs.insert("telemetry_hr".to_string(), hr_cat.as_ref());

    let mut sinks: BTreeMap<String, &mut dyn ShardSink> = BTreeMap::new();
    sinks.insert("default".to_string(), &mut default_store);
    sinks.insert("telemetry_hr".to_string(), &mut hr_store);

    let statuses = backfill_directory_stores(
        &pq_scratch.0,
        self_urn,
        None,
        &config,
        &catalogs,
        &mut sinks,
    )
    .unwrap();
    assert_eq!(statuses.len(), 1);

    default_store.shutdown().unwrap();
    hr_store.shutdown().unwrap();

    // Open TiEngine over the root containing ti.toml and both stores
    let engine = TiEngine::open(store_root, None, None).await.unwrap();

    // 1. Verify 1 s high-res table: telemetry_hr answers with 5 buckets
    let hr_batches = engine
        .session
        .query("SELECT count(*) FROM telemetry_hr")
        .await
        .unwrap();
    let hr_rows = rows_json(&hr_batches).unwrap();
    assert_eq!(hr_rows.len(), 1);
    let hr_count = hr_rows[0].values().next().unwrap().as_i64().unwrap();
    assert_eq!(
        hr_count, 5,
        "1s store should have 5 buckets (one per second)"
    );

    // 2. Verify 10 s default table: telemetry answers with 2 buckets
    let default_batches = engine
        .session
        .query("SELECT count(*) FROM telemetry")
        .await
        .unwrap();
    let default_rows = rows_json(&default_batches).unwrap();
    assert_eq!(default_rows.len(), 1);
    let default_count = default_rows[0].values().next().unwrap().as_i64().unwrap();
    assert_eq!(
        default_count, 2,
        "10s store should have 2 buckets (two 10s windows)"
    );

    // 3. Verify querying specific columns from telemetry_hr
    let hr_data = engine
        .session
        .query("SELECT ts, \"navigation.speedOverGround\" FROM telemetry_hr ORDER BY ts")
        .await
        .unwrap();
    let hr_data_rows = rows_json(&hr_data).unwrap();
    assert_eq!(hr_data_rows.len(), 5);

    // 4. Verify querying specific columns from telemetry (10s table unchanged)
    let def_data = engine
        .session
        .query("SELECT ts, \"navigation.speedOverGround\" FROM telemetry ORDER BY ts")
        .await
        .unwrap();
    let def_data_rows = rows_json(&def_data).unwrap();
    assert_eq!(def_data_rows.len(), 2);
}
