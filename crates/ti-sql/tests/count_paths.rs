use std::{path::PathBuf, sync::Arc};
use ti_contracts::{Agg, Catalog, FieldKind, ShardKey, ShardSink, TiConfig, EPOCH};
use ti_ingest::{NormalizedValue, WatermarkBucketer};
use ti_store::Store;

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[tokio::test]
async fn bucket_snapshot_and_old_sealed_values_are_not_reinterpreted() {
    let scratch = Scratch(
        PathBuf::from(
            std::env::var_os("CARGO_TARGET_TMPDIR").unwrap_or_else(|| std::env::temp_dir().into()),
        )
        .join(format!("count-paths-{}", std::process::id())),
    );
    let mut store = Store::open_or_create(&scratch.0, 10).unwrap();
    let catalog = store.catalog().clone();
    let mut config = TiConfig {
        store_root: scratch.0.to_string_lossy().into_owned(),
        ..TiConfig::default()
    };
    config.profiles.opt_in = vec!["count".into(), "last".into()];
    let mut bucketer = WatermarkBucketer::new(&config);
    let urn = "vessels.urn:mrn:signalk:uuid:count-test";
    // This bucket begins under the old config, even though it closes under the new one.
    bucketer
        .ingest_point(
            urn,
            "cycles",
            "a",
            EPOCH + 1,
            NormalizedValue::Double(9.0),
            &config,
            catalog.as_ref(),
            &mut store,
        )
        .unwrap();
    config.ingest.count_paths = vec!["cycles".into()];
    bucketer
        .ingest_point(
            urn,
            "cycles",
            "a",
            EPOCH + 2,
            NormalizedValue::Double(11.0),
            &config,
            catalog.as_ref(),
            &mut store,
        )
        .unwrap();
    bucketer
        .flush_all(&config, catalog.as_ref(), &mut store)
        .unwrap();
    let old_key = ShardKey {
        vessel: 0,
        shard: 0,
    };
    let sealed_before = store.seal(old_key).unwrap();
    // A new shard starts under the new policy, with values unrelated to the event count.
    let next = EPOCH + (1 << 16) * 10;
    for ts in [next + 1, next + 1, next + 3] {
        bucketer
            .ingest_point(
                urn,
                "cycles",
                "a",
                ts,
                NormalizedValue::Double(87.0),
                &config,
                catalog.as_ref(),
                &mut store,
            )
            .unwrap();
    }
    // Another entity has no count-path samples at all.
    bucketer
        .ingest_point(
            "robots.urn:fleet:missing",
            "other",
            "a",
            next + 2,
            NormalizedValue::Double(1.0),
            &config,
            catalog.as_ref(),
            &mut store,
        )
        .unwrap();
    bucketer
        .flush_all(&config, catalog.as_ref(), &mut store)
        .unwrap();
    // A late event reuses the closed bucket's accumulator rather than replacing it with 1.
    bucketer
        .ingest_point(
            urn,
            "cycles",
            "a",
            next + 4,
            NormalizedValue::Double(f64::MAX),
            &config,
            catalog.as_ref(),
            &mut store,
        )
        .unwrap();
    bucketer
        .ingest_point(
            urn,
            "cycles",
            "a",
            next + 5,
            NormalizedValue::Double(f64::NAN),
            &config,
            catalog.as_ref(),
            &mut store,
        )
        .unwrap();
    assert_eq!(
        store.manifest().get(old_key).unwrap().hash,
        sealed_before.hash
    );
    let fields = catalog.fields().unwrap();
    let bare = fields
        .iter()
        .find(|f| f.path == "cycles" && f.agg.is_none())
        .unwrap();
    assert_eq!(bare.kind, FieldKind::Count);
    assert!(fields
        .iter()
        .any(|f| f.path == "cycles" && f.agg == Some(Agg::Mean)));
    let session = ti_sql::session_from_store(Arc::new(store), 10)
        .await
        .unwrap();
    let rows = session.query("SELECT vessel, cycles, \"cycles@mean\" AS mean, \"cycles@count\" AS n, \"cycles@max\" AS max, \"cycles@min\" AS min, \"cycles@last\" AS last FROM telemetry ORDER BY ts, vessel").await.unwrap();
    let rows = ti_sql::rows_json(&rows).unwrap();
    assert_eq!(rows.len(), 3);
    assert!(rows[0]["cycles"].is_null());
    assert_eq!(rows[0]["mean"], 10.0);
    assert_eq!(rows[0]["n"], 2);
    assert!(rows[1]["cycles"].is_null());
    assert_eq!(rows[2]["cycles"], 4);
    assert_eq!(rows[2]["mean"], 87.0);
    assert_eq!(rows[2]["max"], 87.0);
    assert_eq!(rows[2]["min"], 87.0);
    assert_eq!(rows[2]["last"], 87.0);
    assert_eq!(rows[2]["n"], 4);
    // Counts are integer Arrow columns and are exposed to PostgreSQL as bigint/OID20.
    assert_eq!(
        session
            .catalog
            .schema
            .field_with_name("cycles")
            .unwrap()
            .data_type(),
        &ti_contracts::arrow_schema::DataType::UInt64
    );
    assert_eq!(ti_sql::postgres::type_name("UInt64"), ("bigint", 20));
    assert!(session
        .explain("SELECT cycles FROM telemetry WHERE cycles >= 3")
        .await
        .unwrap()
        .contains("Exact"));
    let engine = ti_sql::TiEngine::from_session(session, scratch.0.clone());
    let status = engine.status().await.unwrap();
    assert_eq!(status["skipped_magnitudes"]["total"], 1);
    assert_eq!(status["skipped_magnitudes"]["paths"]["cycles"], 1);
    let schema = engine.schema(None, None).await.unwrap();
    let telemetry = schema["tables"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "telemetry")
        .unwrap();
    let column = telemetry["columns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "cycles")
        .unwrap();
    assert_eq!(column["type"], "UInt64");
    ti_sql::postgres::register(&engine).await.unwrap();
    let metadata = engine.session.query("SELECT data_type FROM information_schema.columns WHERE table_name = 'telemetry' AND column_name = 'cycles'").await.unwrap();
    assert_eq!(
        ti_sql::rows_json(&metadata).unwrap()[0]["data_type"],
        "bigint"
    );
}
