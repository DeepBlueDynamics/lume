use datafusion::arrow::array::{Array, StringArray, TimestampSecondArray, UInt64Array};
use std::{collections::BTreeMap, sync::Arc};
use ti_contracts::{BucketRecord, FieldKind, FieldSpec, FieldValue, ShardKey, EPOCH};
use ti_core::{MemoryShard, MemorySource};
use ti_sql::{FixtureSource, SqlCatalog, SqlSession, VesselInfo};

async fn fixture() -> SqlSession {
    let fields = vec![FieldSpec {
        id: 1,
        path: "wind".into(),
        agg: None,
        kind: FieldKind::Bsi { scale: 0 },
        units: None,
    }];
    let vessels = (1..=2)
        .map(|ord| VesselInfo {
            ord,
            urn: format!("vessels.urn:test:{ord}"),
            name: None,
            mmsi: None,
            first_seen: EPOCH,
            last_seen: EPOCH + 700_000,
        })
        .collect();
    let catalog = SqlCatalog::new(10, fields.clone(), vessels, BTreeMap::new()).unwrap();
    let mut memory = MemorySource::new();
    for (vessel, shard_ix, cols) in [
        (1, 0, vec![65534, 65535]),
        (1, 1, vec![0, 2, 5]),
        (2, 1, vec![0, 1]),
    ] {
        let mut shard = MemoryShard::new(ShardKey {
            vessel,
            shard: shard_ix,
        })
        .unwrap();
        shard.register_field(fields[0].clone()).unwrap();
        shard
            .apply(
                &cols
                    .into_iter()
                    .map(|col| BucketRecord {
                        vessel,
                        bucket: (shard_ix << 16) | col,
                        field: 1,
                        value: FieldValue::Int(10),
                        rewrite: false,
                    })
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        memory.insert(shard);
    }
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
#[tokio::test]
async fn intervals_cross_shards_named_args_and_boundaries() {
    let s = fixture().await;
    let sql = "SELECT * FROM intervals('wind > 0', max_gap => '10s', vessel => 'vessels.urn:test:1', min_len => '50s')";
    let batches = s.query(sql).await.unwrap();
    assert_eq!(batches.iter().map(|b| b.num_rows()).sum::<usize>(), 1);
    let batch = batches.iter().find(|b| b.num_rows() != 0).unwrap();
    assert_eq!(
        batch
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap()
            .value(0),
        "vessels.urn:test:1"
    );
    assert_eq!(
        batch
            .column(1)
            .as_any()
            .downcast_ref::<TimestampSecondArray>()
            .unwrap()
            .value(0),
        EPOCH + 65534 * 10
    );
    assert_eq!(
        batch
            .column(2)
            .as_any()
            .downcast_ref::<TimestampSecondArray>()
            .unwrap()
            .value(0),
        EPOCH + 65539 * 10
    );
    assert_eq!(
        batch
            .column(3)
            .as_any()
            .downcast_ref::<UInt64Array>()
            .unwrap()
            .value(0),
        4
    );
    let plan = s.explain(sql).await.unwrap();
    assert!(plan.contains("bitmap runs"), "{plan}");
    assert!(s
        .reports()
        .unwrap()
        .iter()
        .all(|r| r.materialized_rows == 0));
    let batches = s.query("SELECT * FROM intervals('wind > 0', min_len => '50.000000001s', max_gap => '10s', vessel => 'vessels.urn:test:1')").await.unwrap();
    assert_eq!(batches.iter().map(|b| b.num_rows()).sum::<usize>(), 0);
    let batches = s.query("SELECT * FROM intervals('wind > 0', max_gap => '9.999999999s', vessel => 'vessels.urn:test:1')").await.unwrap();
    assert_eq!(batches.iter().map(|b| b.num_rows()).sum::<usize>(), 3);
    assert!(s
        .query("SELECT * FROM intervals('TRUE; DELETE FROM telemetry')")
        .await
        .is_err());
    assert!(s
        .query("SELECT * FROM intervals('TRUE', min_len => '-1s')")
        .await
        .is_err());
    assert!(s
        .query("SELECT * FROM intervals('TRUE', max_gap => '0s', max_gap => '1s')")
        .await
        .is_err());
}
#[tokio::test]
async fn intervals_residual_matches_bitmap_and_separates_vessels() {
    let s = fixture().await;
    let bitmap = s
        .query("SELECT * FROM intervals('wind > 0') ORDER BY vessel, start")
        .await
        .unwrap();
    let residual = s
        .query("SELECT * FROM intervals('wind + 1 > 1') ORDER BY vessel, start")
        .await
        .unwrap();
    assert_eq!(bitmap.iter().map(|b| b.num_rows()).sum::<usize>(), 4);
    assert_eq!(
        datafusion::arrow::util::pretty::pretty_format_batches(&bitmap)
            .unwrap()
            .to_string(),
        datafusion::arrow::util::pretty::pretty_format_batches(&residual)
            .unwrap()
            .to_string()
    );
    let plan = s
        .explain("SELECT * FROM intervals('wind + 1 > 1')")
        .await
        .unwrap();
    assert!(plan.contains("materialization fallback"), "{plan}");
    for batch in bitmap {
        assert!(batch.columns().iter().all(|a| a.null_count() == 0));
    }
}

fn formatted(batches: &[datafusion::arrow::record_batch::RecordBatch]) -> String {
    datafusion::arrow::util::pretty::pretty_format_batches(batches)
        .unwrap()
        .to_string()
}
async fn compare(s: &SqlSession, baseline: &SqlSession, sql: &str) {
    let optimized = s.query(sql).await.unwrap();
    let materialized = baseline.query(sql).await.unwrap();
    assert_eq!(formatted(&optimized), formatted(&materialized), "{sql}");
}
#[tokio::test]
async fn aggregate_selection_grouping_and_fallbacks() {
    let s = fixture().await;
    let baseline =
        SqlSession::new_with_bitmap_aggregates(s.source.clone(), s.catalog.clone(), false)
            .await
            .unwrap();
    for sql in [
        "SELECT count(*), count(wind), sum(wind), min(wind), max(wind) FROM telemetry",
        "SELECT vessel, count(*), sum(wind), min(wind), max(wind) FROM telemetry GROUP BY vessel ORDER BY vessel",
        "SELECT date_bin(INTERVAL '1 day',ts) AS day, count(*), max(wind) FROM telemetry GROUP BY 1 ORDER BY 1",
        "SELECT vessel, date_bin(INTERVAL '70 seconds',ts) AS day, count(wind), max(wind) FROM telemetry GROUP BY 1,2 ORDER BY 1,2",
        "SELECT vessel, date_bin(INTERVAL '70 seconds',ts,TIMESTAMP '2020-01-01 00:00:03') AS day, count(wind), max(wind) FROM telemetry GROUP BY 1,2 ORDER BY 1,2",
        "SELECT count(*), sum(wind), min(wind), max(wind) FROM telemetry WHERE wind > 100",
        "SELECT vessel, max(wind) FROM telemetry WHERE wind > 100 GROUP BY vessel ORDER BY vessel",
    ] {
        compare(&s, &baseline, sql).await;
        let plan = s.explain(sql).await.unwrap();
        assert!(plan.contains("BitmapAggregateExec chosen"), "{sql}\n{plan}");
        assert!(plan.contains("Arrow telemetry rows=0"), "{plan}");
    }
    for (sql, reason) in [
        (
            "SELECT avg(wind) FROM telemetry",
            "unsupported aggregate avg",
        ),
        (
            "SELECT sum(wind) FROM telemetry WHERE wind + 1 > 1",
            "FilterExec",
        ),
        ("SELECT count(DISTINCT wind) FROM telemetry", "DISTINCT"),
        ("SELECT sum(wind + 1) FROM telemetry", "computed expression"),
        (
            "SELECT date_bin(INTERVAL '1 month',ts),max(wind) FROM telemetry GROUP BY 1",
            "calendar months",
        ),
        (
            "SELECT wind,count(*) FROM telemetry GROUP BY wind",
            "GROUP BY",
        ),
    ] {
        compare(&s, &baseline, sql).await;
        let plan = s.explain(sql).await.unwrap();
        assert!(
            plan.contains("fallback") && plan.contains(reason),
            "{sql}\n{plan}"
        );
    }
    let sql = "SELECT date_bin(INTERVAL '1 day',ts,CAST(NULL AS TIMESTAMP)),max(wind) FROM telemetry GROUP BY 1";
    let optimized_error = s.query(sql).await.unwrap_err().to_string();
    let baseline_error = baseline.query(sql).await.unwrap_err().to_string();
    assert_eq!(optimized_error, baseline_error);
    let plan = s
        .prepare(sql)
        .await
        .unwrap()
        .create_physical_plan()
        .await
        .unwrap();
    assert!(!datafusion::physical_plan::displayable(plan.as_ref())
        .indent(true)
        .to_string()
        .contains("BitmapAggregateExec"));
}
async fn generated(width: u64, vessels: u32, buckets: u32, sparse: bool) -> SqlSession {
    let fields = vec![
        FieldSpec {
            id: 1,
            path: "wind".into(),
            agg: None,
            kind: FieldKind::Bsi { scale: 0 },
            units: None,
        },
        FieldSpec {
            id: 2,
            path: "present".into(),
            agg: None,
            kind: FieldKind::Presence,
            units: None,
        },
    ];
    let infos = (1..=vessels)
        .map(|ord| VesselInfo {
            ord,
            urn: format!("vessels.urn:generated:{ord}"),
            name: None,
            mmsi: None,
            first_seen: EPOCH,
            last_seen: EPOCH + i64::from(buckets) * width as i64,
        })
        .collect();
    let catalog = SqlCatalog::new(width, fields.clone(), infos, BTreeMap::new()).unwrap();
    let mut memory = MemorySource::new();
    let mut rng = 0x12345678u32;
    for vessel in 1..=vessels {
        for shard_ix in 0..=((buckets - 1) >> 16) {
            let mut shard = MemoryShard::new(ShardKey {
                vessel,
                shard: shard_ix,
            })
            .unwrap();
            for f in &fields {
                shard.register_field(f.clone()).unwrap();
            }
            let from = shard_ix << 16;
            let to = (u64::from(from) + 65536).min(u64::from(buckets)) as u32;
            let mut records = vec![];
            for bucket in from..to {
                rng = rng.wrapping_mul(1664525).wrapping_add(1013904223);
                if sparse && rng.is_multiple_of(5) {
                    continue;
                }
                records.push(BucketRecord {
                    vessel,
                    bucket,
                    field: 2,
                    value: FieldValue::Present,
                    rewrite: false,
                });
                if !sparse || !rng.is_multiple_of(7) {
                    records.push(BucketRecord {
                        vessel,
                        bucket,
                        field: 1,
                        value: FieldValue::Int(i64::from((rng >> 8) % 1024) - 512),
                        rewrite: false,
                    });
                }
            }
            shard.apply(&records).unwrap();
            memory.insert(shard);
        }
    }
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
#[tokio::test]
async fn deterministic_random_aggregates_match_materialization() {
    let s = generated(13, 3, 65700, true).await;
    let baseline =
        SqlSession::new_with_bitmap_aggregates(s.source.clone(), s.catalog.clone(), false)
            .await
            .unwrap();
    for threshold in [-512, -201, 0, 313, 512] {
        for groups in [
            "",
            "vessel",
            "date_bin(INTERVAL '1 hour',ts)",
            "vessel,date_bin(INTERVAL '1001 seconds',ts)",
        ] {
            let prefix = if groups.is_empty() {
                String::new()
            } else {
                format!("{groups},")
            };
            let suffix = if groups.is_empty() {
                String::new()
            } else {
                format!(" GROUP BY {groups} ORDER BY {groups}")
            };
            let sql = format!("SELECT {prefix}count(*),count(wind),count(present),sum(wind),min(wind),max(wind) FROM telemetry WHERE wind >= {threshold}{suffix}");
            compare(&s, &baseline, &sql).await;
            let plan = s.explain(&sql).await.unwrap();
            assert!(plan.contains("BitmapAggregateExec chosen"), "{plan}");
        }
    }
    compare(&s, &baseline, "SELECT count(*),count(wind),sum(wind),min(wind),max(wind) FROM telemetry WHERE wind IS NULL").await;
}
#[tokio::test]
#[ignore = "synthetic one-year Q4 performance measurement"]
async fn synthetic_year_benchmark() {
    use std::time::Instant;
    let build = Instant::now();
    let s = generated(60, 50, 365 * 24 * 60, false).await;
    println!(
        "fixture: 365 days, 50 vessels, width=60s, {} buckets, build={:?}",
        50u64 * 365 * 24 * 60,
        build.elapsed()
    );
    let baseline =
        SqlSession::new_with_bitmap_aggregates(s.source.clone(), s.catalog.clone(), false)
            .await
            .unwrap();
    let sql = "SELECT vessel,date_bin(INTERVAL '1 day',ts) AS day,max(wind) FROM telemetry GROUP BY 1,2 ORDER BY 1,2";
    compare(&s, &baseline, sql).await;
    let plan = s.explain(sql).await.unwrap();
    assert!(plan.contains("BitmapAggregateExec chosen"), "{plan}");
    let mut bitmap = vec![];
    let mut materialized = vec![];
    for _ in 0..3 {
        let t = Instant::now();
        let a = s.query(sql).await.unwrap();
        bitmap.push(t.elapsed().as_secs_f64());
        let t = Instant::now();
        let b = baseline.query(sql).await.unwrap();
        materialized.push(t.elapsed().as_secs_f64());
        assert_eq!(formatted(&a), formatted(&b));
    }
    bitmap.sort_by(f64::total_cmp);
    materialized.sort_by(f64::total_cmp);
    println!("Q4 median: bitmap={:.6}s, materialized={:.6}s, ratio={:.2}x; bitmap runs={bitmap:?}; materialized runs={materialized:?}; debug profile",
        bitmap[1], materialized[1], materialized[1]/bitmap[1]);
}
