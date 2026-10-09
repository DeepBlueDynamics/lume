//! Lume's own metrics live in `<store>/stores/lume` and are served as `telemetry_lume`,
//! so `telemetry` holds only real vessels (no rows of nulls for the Lume entity).

use std::time::{Duration, Instant};

use ti_ingest::self_telemetry::{SelfStats, SelfStore, SelfTelemetry};
use ti_sql::TiEngine;

#[tokio::test]
async fn self_telemetry_is_its_own_table() {
    // Cargo's per-target scratch dir stays inside the workspace on host and container.
    let root = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("ti-self-telemetry-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    drop(ti_store::Store::open_or_create(&root, 10).unwrap());

    let mut telemetry = SelfTelemetry::new("test-box", Duration::from_secs(10));
    let mut store = SelfStore::open(&root).unwrap();
    let start = Instant::now();
    let base = ti_contracts::EPOCH + 1_000;
    for (step, records) in [(0u64, 1_000u64), (1, 201_000)] {
        let samples = telemetry.sample(
            start + Duration::from_secs(step * 10),
            &SelfStats {
                records_ingested: records,
                ..Default::default()
            },
        );
        store
            .record(telemetry.context(), base + step as i64 * 10, &samples)
            .unwrap();
    }
    drop(store);

    let engine = TiEngine::open(&root, Some(10), None).await.unwrap();
    let result = engine
        .query(
            "SELECT vessel, max(\"lume.ingest.valuesPerSecond@mean\") AS vps, count(*) AS n \
             FROM telemetry_lume GROUP BY vessel",
            500,
        )
        .await
        .unwrap();
    let rows = result["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{result}");
    assert_eq!(rows[0]["vessel"], "lume.urn:host:test-box");
    assert_eq!(rows[0]["vps"], 20_000.0);
    assert_eq!(rows[0]["n"], 2);

    let vessels = engine
        .query("SELECT count(*) AS n FROM telemetry", 500)
        .await
        .unwrap();
    assert_eq!(
        vessels["rows"][0]["n"], 0,
        "telemetry must not hold Lume's rows"
    );
    drop(engine);
    std::fs::remove_dir_all(&root).unwrap();
}
