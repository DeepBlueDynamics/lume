use datafusion::arrow::array::TimestampSecondArray;
use std::{collections::BTreeMap, sync::Arc};
use ti_contracts::{Agg, BucketRecord, FieldKind, FieldSpec, FieldValue, ShardKey, EPOCH};
use ti_core::{MemoryShard, MemorySource};
use ti_sql::{FixtureSource, SqlCatalog, SqlSession, VesselInfo};

async fn fixture() -> SqlSession {
    let fields = vec![
        FieldSpec {
            id: 1,
            path: "navigation.position.latitude".into(),
            agg: Some(Agg::Last),
            kind: FieldKind::Bsi { scale: 7 },
            units: None,
        },
        FieldSpec {
            id: 2,
            path: "navigation.position.longitude".into(),
            agg: Some(Agg::Last),
            kind: FieldKind::Bsi { scale: 7 },
            units: None,
        },
        FieldSpec {
            id: 3,
            path: "navigation.position".into(),
            agg: None,
            kind: FieldKind::Geo { res: 7 },
            units: None,
        },
    ];
    let catalog = SqlCatalog::new(
        10,
        fields.clone(),
        vec![VesselInfo {
            ord: 1,
            urn: "vessels.urn:geo:1".into(),
            name: None,
            mmsi: None,
            first_seen: EPOCH,
            last_seen: EPOCH + 100,
        }],
        BTreeMap::new(),
    )
    .unwrap();
    let mut shard = MemoryShard::new(ShardKey {
        vessel: 1,
        shard: 0,
    })
    .unwrap();
    for field in &fields {
        shard.register_field(field.clone()).unwrap();
    }
    let mut records = vec![];
    for (bucket, lat, lon) in [
        (0, 36.0, -122.0),
        (1, 36.01, -122.01),
        (2, 37.0, -120.0),
        (3, 0.0, 179.9),
        (4, 0.0, -179.9),
        (5, 89.99, 100.0),
    ] {
        for (field, value) in [(1, lat), (2, lon)] {
            records.push(BucketRecord {
                vessel: 1,
                bucket,
                field,
                value: FieldValue::Int(ti_contracts::to_fixed(value, 7).unwrap()),
                rewrite: false,
            });
        } // Include a legacy bucket without Geo rows: BSI safety envelope must retain it.
        if bucket != 1 {
            records.push(BucketRecord {
                vessel: 1,
                bucket,
                field: 3,
                value: FieldValue::Cells(ti_geo::cells_for(lat, lon).unwrap().to_vec()),
                rewrite: false,
            });
        }
    } // Coordinate NULL: a geo-only bucket must not match positive or negative refinement.
    records.push(BucketRecord {
        vessel: 1,
        bucket: 6,
        field: 3,
        value: FieldValue::Cells(ti_geo::cells_for(36.0, -122.0).unwrap().to_vec()),
        rewrite: false,
    });
    shard.apply(&records).unwrap();
    let mut memory = MemorySource::new();
    memory.insert(shard);
    SqlSession::new(
        Arc::new(FixtureSource {
            memory,
            catalog: catalog.clone(),
        }),
        catalog,
    )
    .await
    .unwrap()
}
async fn buckets(s: &SqlSession, predicate: &str) -> Vec<i64> {
    s.query(&format!(
        "SELECT ts FROM telemetry WHERE {predicate} ORDER BY ts"
    ))
    .await
    .unwrap()
    .iter()
    .flat_map(|b| {
        b.column(0)
            .as_any()
            .downcast_ref::<TimestampSecondArray>()
            .unwrap()
            .values()
            .iter()
            .map(|t| (t - EPOCH) / 10)
            .collect::<Vec<_>>()
    })
    .collect()
}
#[tokio::test]
async fn geo_refines_nulls_wrapping_and_negation() {
    let s = fixture().await;
    assert_eq!(
        buckets(&s, "in_bbox(35.5,-123.0,36.5,-121.0)").await,
        vec![0, 1]
    );
    assert_eq!(buckets(&s, "within_nm(36.0,-122.0,1.0)").await, vec![0, 1]);
    assert_eq!(
        buckets(&s, "NOT in_bbox(35.5,-123.0,36.5,-121.0)").await,
        vec![2, 3, 4, 5]
    );
    assert_eq!(
        buckets(&s, "NOT within_nm(36.0,-122.0,1.0)").await,
        vec![2, 3, 4, 5]
    );
    assert_eq!(
        buckets(&s, "in_bbox(-1.0,179.0,1.0,-179.0)").await,
        vec![3, 4]
    );
    assert_eq!(buckets(&s, "within_nm(0.0,180.0,7.0)").await, vec![3, 4]);
    assert_eq!(buckets(&s, "within_nm(90.0,0.0,1.0)").await, vec![5]);
    assert_eq!(buckets(&s, "within_nm(36.0,-122.0,0.0)").await, vec![0]);
    assert_eq!(
        buckets(&s, "in_bbox(NULL,-123.0,36.5,-121.0)").await,
        Vec::<i64>::new()
    );
    assert_eq!(
        buckets(
            &s,
            "in_bbox(35.5,-123.0,36.5,-121.0) OR in_bbox(-1.0,179.0,1.0,-179.0)"
        )
        .await,
        vec![0, 1, 3, 4]
    );
    let plan = s
        .explain("SELECT count(*) FROM telemetry WHERE in_bbox(35.5,-123.0,36.5,-121.0)")
        .await
        .unwrap();
    assert!(plan.contains("Inexact"), "{plan}");
    assert!(plan.contains("FilterExec"), "{plan}");
    let plan = s
        .explain("SELECT ts FROM telemetry WHERE NOT within_nm(36.0,-122.0,1.0)")
        .await
        .unwrap();
    assert!(plan.contains("Unsupported"), "{plan}");
    assert!(s
        .query("SELECT ts FROM telemetry WHERE within_nm(36.0,-122.0,-1.0)")
        .await
        .is_err());
}

/// Stored oracle gate; no DuckDB executable is needed in CI.
#[tokio::test]
#[ignore = "requires the generated correctness Parquet and stored Q7 expected outputs"]
async fn q7_stored_corpus() {
    let data =
        std::path::PathBuf::from(std::env::var("TI_DATA_DIR").expect("TI_DATA_DIR required"));
    let expected = std::path::PathBuf::from(
        std::env::var("TI_EXPECTED_DIR").expect("TI_EXPECTED_DIR required"),
    );
    let corpus_path =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/golden/corpus.json");
    let corpus: ti_sql::Corpus =
        serde_json::from_slice(&std::fs::read(corpus_path).unwrap()).unwrap();
    let tmp = tempfile_compatible_store_root();
    let mut store = ti_store::Store::open_or_create(&tmp, corpus.bucket_width_seconds).unwrap();
    let catalog = Arc::clone(store.catalog());
    let mut config = ti_contracts::TiConfig {
        width_seconds: corpus.bucket_width_seconds,
        ..Default::default()
    };
    config.profiles.default = vec!["mean".into(), "min".into(), "max".into(), "last".into()];
    config.profiles.slow = config.profiles.default.clone();
    let urn = "vessels.urn:mrn:imo:mmsi:367000000";
    let raw = data.join("tier=raw/context=vessels__urn-mrn-imo-mmsi-367000000");
    let paths = [
        "navigation.position",
        "navigation.speedOverGround",
        "environment.depth.belowTransducer",
        "environment.wind.speedTrue",
        "electrical.batteries.house.stateOfCharge",
    ];
    config.allow_paths = paths.iter().map(|p| format!("{p}*")).collect();
    for path in paths {
        let directory = raw.join(format!("path={}", path.replace('.', "__")));
        assert!(
            directory.is_dir(),
            "missing raw partition {}",
            directory.display()
        );
        let statuses = ti_ingest::backfill_directory(
            &directory,
            urn,
            None,
            &config,
            catalog.as_ref(),
            &mut store,
        )
        .unwrap();
        assert!(!statuses.is_empty(), "no Parquet input for {path}");
    }
    let session = ti_sql::session_from_store(Arc::new(store), corpus.bucket_width_seconds)
        .await
        .unwrap();
    let mut passed = 0;
    for entry in corpus
        .entries
        .into_iter()
        .filter(|e| e.id.starts_with("q7-"))
    {
        if entry.id == "q7-005" {
            println!("q7-005 pending W5: mixed geo/text predicate");
            continue;
        }
        let oracle: Vec<serde_json::Map<String, serde_json::Value>> = serde_json::from_slice(
            &std::fs::read(expected.join(format!("{}.json", entry.id))).unwrap(),
        )
        .unwrap();
        let batches = session.query(&entry.ti_sql).await.unwrap();
        let actual = ti_sql::rows_json(&batches).unwrap();
        println!(
            "{}: {} TI rows / {} stored oracle rows",
            entry.id,
            actual.len(),
            oracle.len()
        );
        ti_sql::diff_rows(actual, oracle, &entry.tolerance).unwrap();
        passed += 1;
    }
    assert_eq!(passed, 5);
    std::fs::remove_dir_all(tmp).unwrap();
}
fn tempfile_compatible_store_root() -> std::path::PathBuf {
    let root = std::path::PathBuf::from(
        std::env::var("TMPDIR").expect("TMPDIR must be inside /workspace"),
    );
    let path = root.join(format!("ti-geo-corpus-{}", std::process::id()));
    assert!(
        !path.exists(),
        "test store already exists: {}",
        path.display()
    );
    path
}
