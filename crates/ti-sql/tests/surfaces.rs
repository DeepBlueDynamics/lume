use std::{
    io::Write,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
use ti_sql::store_width;
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        // Cargo's per-target scratch dir stays inside the workspace on host and container.
        let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
            "ti-surfaces-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
fn header(path: &std::path::Path, width: u64) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut file = std::fs::File::create(path).unwrap();
    file.write_all(&ti_contracts::SHARD_MAGIC).unwrap();
    file.write_all(&ti_contracts::FORMAT_VERSION.to_le_bytes())
        .unwrap();
    let urn = b"vessels.urn:test";
    file.write_all(&(urn.len() as u32).to_le_bytes()).unwrap();
    file.write_all(urn).unwrap();
    file.write_all(&[0; 8]).unwrap();
    file.write_all(&width.to_le_bytes()).unwrap();
}
#[test]
fn widths_are_derived_and_conflicts_or_corruption_fail() {
    let dir = Scratch::new();
    assert!(store_width(&dir.0, None).is_err());
    assert_eq!(store_width(&dir.0, Some(60)).unwrap(), 60);
    header(&dir.0.join("shards/0/1/v1/0.rbm"), 60);
    assert_eq!(store_width(&dir.0, None).unwrap(), 60);
    let error = store_width(&dir.0, Some(10)).unwrap_err().to_string();
    assert!(
        error.contains("this store\'s width is 60 s; omit width_seconds"),
        "{error}"
    );
    header(&dir.0.join("shards/0/2/open/0.rbm"), 10);
    assert!(store_width(&dir.0, None).is_err());
}
#[test]
fn stored_config_supports_empty_stores_and_rejects_mismatches() {
    let dir = Scratch::new();
    std::fs::write(dir.0.join("ti.toml"), "width_seconds = 60\n").unwrap();
    assert_eq!(store_width(&dir.0, None).unwrap(), 60);
    let error = store_width(&dir.0, Some(10)).unwrap_err().to_string();
    assert!(
        error.contains("this store\'s width is 60 s; omit width_seconds"),
        "{error}"
    );
}

#[tokio::test]
async fn query_echoes_canonical_aggregates_units_and_open_shards() {
    use std::sync::Arc;
    use ti_contracts::{Agg, BucketRecord, FieldKind, FieldSpec, FieldValue, ShardKey, EPOCH};
    let field = FieldSpec {
        id: 7,
        path: "navigation.speedOverGround".into(),
        agg: Some(Agg::Mean),
        kind: FieldKind::Bsi { scale: 3 },
        units: Some("m/s".into()),
    };
    let catalog = ti_sql::SqlCatalog::new(
        10,
        vec![field.clone()],
        vec![ti_sql::VesselInfo {
            ord: 0,
            urn: "vessels.urn:test".into(),
            name: None,
            mmsi: None,
            first_seen: EPOCH,
            last_seen: EPOCH + 10,
        }],
        Default::default(),
    )
    .unwrap();
    let mut shard = ti_core::MemoryShard::new(ShardKey {
        vessel: 0,
        shard: 0,
    })
    .unwrap();
    shard.register_field(field).unwrap();
    shard
        .apply(&[BucketRecord {
            vessel: 0,
            bucket: 1,
            field: 7,
            value: FieldValue::Int(5000),
            rewrite: false,
        }])
        .unwrap();
    let mut source = ti_core::MemorySource::new();
    source.insert(shard);
    let source = ti_sql::FixtureSource {
        memory: source,
        catalog: catalog.clone(),
    };
    let session = ti_sql::SqlSession::new(Arc::new(source), catalog)
        .await
        .unwrap();
    let engine = ti_sql::TiEngine::from_session(session, PathBuf::new());
    let result = engine
        .query("SELECT \"navigation.speedOverGround\" FROM telemetry", 500)
        .await
        .unwrap();
    assert_eq!(
        result["columns"][0]["name"],
        "navigation.speedOverGround@mean"
    );
    assert_eq!(result["columns"][0]["units"], "m/s");
    assert_eq!(result["rows"][0]["navigation.speedOverGround@mean"], 5.0);
    assert_eq!(engine.status().await.unwrap()["shards"]["open"], 1);
    assert!(engine.query("DELETE FROM telemetry", 500).await.is_err());
}

#[test]
fn import_docs_reads_parquet_and_persists_idempotently() {
    use datafusion::arrow::{
        array::{ArrayRef, StringArray},
        datatypes::{DataType, Field, Schema},
        record_batch::RecordBatch,
    };
    use std::sync::Arc;
    let dir = Scratch::new();
    let root = dir.0.join("store");
    drop(ti_store::Store::open_or_create(&root, 60).unwrap());
    let docs = dir.0.join("incoming");
    std::fs::create_dir(&docs).unwrap();
    let schema = Arc::new(Schema::new(
        ["context", "kind", "ts_start", "ts_end", "title", "body"]
            .iter()
            .map(|name| Field::new(*name, DataType::Utf8, true))
            .collect::<Vec<_>>(),
    ));
    let values = [
        Some("vessels.urn:mrn:imo:mmsi:367000000"),
        Some("notes"),
        Some("2026-03-01T00:00:00Z"),
        None,
        Some("Bilge inspection"),
        Some("Water near the pump"),
    ];
    let columns: Vec<ArrayRef> = values
        .into_iter()
        .map(|v| Arc::new(StringArray::from(vec![v])) as ArrayRef)
        .collect();
    let batch = RecordBatch::try_new(schema.clone(), columns).unwrap();
    let file = std::fs::File::create(docs.join("notes.parquet")).unwrap();
    let mut writer = parquet::arrow::ArrowWriter::try_new(file, schema, None).unwrap();
    writer.write(&batch).unwrap();
    writer.close().unwrap();
    let args = vec![
        "import-docs".into(),
        docs.to_str().unwrap().into(),
        "--store".into(),
        root.to_str().unwrap().into(),
        "--width".into(),
        "60".into(),
    ];
    ti_sql::run_cli(&args).unwrap();
    ti_sql::run_cli(&args).unwrap();
    let stored = ti_store::DocStore::open(&root).unwrap();
    assert_eq!(stored.len(), 1);
    let doc = stored.iter().next().unwrap();
    assert_eq!(doc.body, "Water near the pump");
    assert_eq!(doc.kind, "notes");
    assert!(doc.ts_end.is_none());
}

#[test]
fn width_discovery_reads_one_representative_per_field_directory() {
    let dir = Scratch::new();
    let fields = dir.0.join("shards/0/1/v1");
    header(&fields.join("0.rbm"), 60);
    std::fs::write(fields.join("1.rbm"), b"not another header to probe").unwrap();
    assert_eq!(store_width(&dir.0, None).unwrap(), 60);
    assert!(store_width(&dir.0, Some(10)).is_err());
    header(&dir.0.join("shards/0/2/v1/0.rbm"), 10);
    assert!(store_width(&dir.0, None).is_err());
}

#[tokio::test]
async fn status_reads_ingest_status_json() {
    let dir = Scratch::new();
    let root = dir.0.join("store");
    drop(ti_store::Store::open_or_create(&root, 60).unwrap());
    let status_file = root.join("ingest_status.json");
    let ingest_data = serde_json::json!({
        "running": true,
        "pid": 12345,
        "reconnects": 2,
        "records_ingested": 100,
        "ingest_lag_seconds": 1.25,
        "last_delta": "2026-06-01T12:00:00Z",
        "updated_at": "2026-06-01T12:00:01Z"
    });
    std::fs::write(&status_file, serde_json::to_string(&ingest_data).unwrap()).unwrap();

    let engine = ti_sql::TiEngine::open(&root, Some(60), None).await.unwrap();
    let status = engine.status().await.unwrap();
    assert_eq!(status["ingest_lag_seconds"], 1.25);
    assert_eq!(status["reconnects"], 2);
    assert_eq!(status["last_delta"], "2026-06-01T12:00:00Z");
    let unavailable = status["unavailable"].as_array().unwrap();
    assert!(
        !unavailable
            .iter()
            .any(|u| u.as_str().unwrap().starts_with("ingest_lag_seconds")),
        "ingest_lag_seconds must not be listed as unavailable when ingest_status.json is present"
    );
}
