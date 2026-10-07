//! Read-only paired diagnosis of q5-002; no golden SQL or classifier changes.
use datafusion::physical_plan::{collect, displayable};
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Instant};
use ti_contracts::{Agg, BucketRecord, FieldKind, FieldSpec, FieldValue, ShardKey, EPOCH};
use ti_core::{MemoryShard, MemorySource};
use ti_sql::{FixtureSource, SqlCatalog, SqlSession, TiEngine, VesselInfo};

const DISTINCT: &str = r#"SELECT * FROM intervals(
  '"propulsion.port.motorPower" > 500 AND "propulsion.main.state" IS DISTINCT FROM ''started''',
  min_len => '40s')"#;
const BITMAP_EQUIVALENT: &str = r#"SELECT * FROM intervals(
  '"propulsion.port.motorPower" > 500 AND ("propulsion.main.state" IS NULL OR "propulsion.main.state" <> ''started'')',
  min_len => '40s')"#;

fn canonical(batches: &[datafusion::arrow::record_batch::RecordBatch]) -> Vec<String> {
    let mut rows: Vec<_> = ti_sql::rows_json(batches)
        .unwrap()
        .into_iter()
        .map(|row| serde_json::to_string(&row).unwrap())
        .collect();
    rows.sort();
    rows
}
async fn measure(session: &SqlSession, sql: &str) -> (Value, Vec<String>) {
    session.reset_diagnostics().unwrap();
    let total = Instant::now();
    let t = Instant::now();
    let frame = session.prepare(sql).await.unwrap();
    let logical_ms = t.elapsed().as_secs_f64() * 1000.0;
    let t = Instant::now();
    let plan = frame.create_physical_plan().await.unwrap();
    let physical_ms = t.elapsed().as_secs_f64() * 1000.0;
    let t = Instant::now();
    let batches = collect(
        plan.clone(),
        Arc::new(datafusion::execution::TaskContext::default()),
    )
    .await
    .unwrap();
    let execute_ms = t.elapsed().as_secs_f64() * 1000.0;
    let total_ms = total.elapsed().as_secs_f64() * 1000.0;
    let scans = session.reports().unwrap();
    let report = json!({
        "logical_ms":logical_ms, "physical_ms":physical_ms,
        "execute_ms":execute_ms,"total_ms":total_ms,
        "materialized_rows":scans.iter().map(|s| s.materialized_rows).sum::<u64>(),
        "scanned_shards":scans.iter().map(|s| s.scanned_shards).sum::<usize>(),
        "filters":scans.iter().flat_map(|s| s.filters.clone()).collect::<Vec<_>>(),
        "plan":displayable(plan.as_ref()).indent(true).to_string(),
        "rows":batches.iter().map(|b| b.num_rows()).sum::<usize>()
    });
    (report, canonical(&batches))
}
fn percentile(values: &mut [f64], fraction: f64) -> f64 {
    values.sort_by(f64::total_cmp);
    values[((values.len() as f64 * fraction).ceil() as usize).saturating_sub(1)]
}

#[tokio::test]
async fn nullable_distinct_from_intervals_equal_bitmap_expression_across_shards() {
    let fields = vec![
        FieldSpec {
            id: 1,
            path: "propulsion.port.motorPower".into(),
            agg: Some(Agg::Mean),
            kind: FieldKind::Bsi { scale: 0 },
            units: Some("W".into()),
        },
        FieldSpec {
            id: 2,
            path: "propulsion.main.state".into(),
            agg: None,
            kind: FieldKind::Set,
            units: None,
        },
    ];
    let dictionaries = BTreeMap::from([(
        2,
        BTreeMap::from([(0, "started".into()), (1, "stopped".into())]),
    )]);
    let catalog = SqlCatalog::new(
        10,
        fields.clone(),
        vec![VesselInfo {
            ord: 0,
            urn: "vessels.urn:test:q5".into(),
            name: None,
            mmsi: None,
            first_seen: EPOCH,
            last_seen: EPOCH + 700000,
        }],
        dictionaries,
    )
    .unwrap();
    let mut memory = MemorySource::new();
    for shard_ix in [0, 1] {
        let mut shard = MemoryShard::new(ShardKey {
            vessel: 0,
            shard: shard_ix,
        })
        .unwrap();
        for f in &fields {
            shard.register_field(f.clone()).unwrap();
        }
        shard.register_set_value(2, 0, "started").unwrap();
        shard.register_set_value(2, 1, "stopped").unwrap();
        let mut records = vec![];
        for offset in 0..24 {
            let bucket = 65530 + offset;
            if bucket >> 16 != shard_ix {
                continue;
            }
            // Each 8-bucket period: started, stopped+NULLs, low motor power.
            let slot = offset % 8;
            records.push(BucketRecord {
                vessel: 0,
                bucket,
                field: 1,
                value: FieldValue::Int(if slot == 7 { 200 } else { 1000 }),
                rewrite: false,
            });
            if slot == 0 || slot == 7 || slot == 1 {
                records.push(BucketRecord {
                    vessel: 0,
                    bucket,
                    field: 2,
                    value: FieldValue::SetValue(if slot == 1 { 1 } else { 0 }),
                    rewrite: false,
                });
            }
        }
        shard.apply(&records).unwrap();
        memory.insert(shard);
    }
    let session = SqlSession::new(
        Arc::new(FixtureSource {
            memory,
            catalog: catalog.clone(),
        }),
        catalog,
    )
    .await
    .unwrap();
    let (original, a) = measure(&session, DISTINCT).await;
    let (rewritten, b) = measure(&session, BITMAP_EQUIVALENT).await;
    assert_eq!(a, b);
    assert_eq!(a.len(), 3);
    assert!(
        original["plan"]
            .as_str()
            .unwrap()
            .contains("materialization fallback"),
        "{original}"
    );
    assert!(original["materialized_rows"].as_u64().unwrap() > 0);
    assert!(
        rewritten["plan"].as_str().unwrap().contains("bitmap runs"),
        "{rewritten}"
    );
    assert_eq!(rewritten["materialized_rows"], 0);
    // Dropping the NULL arm would lose each stopped+NULL interval entirely.
    let without_null = session
        .query(
            r#"SELECT * FROM intervals(
      '"propulsion.port.motorPower" > 500 AND "propulsion.main.state" <> ''started''',
      min_len => '40s')"#,
        )
        .await
        .unwrap();
    assert!(canonical(&without_null).is_empty());
}

#[tokio::test]
#[ignore = "requires a read-only boat store; native host timing preferred"]
async fn boat_store_q5_paired_profile() {
    let root = PathBuf::from(std::env::var_os("TI_Q5_STORE").expect("TI_Q5_STORE"));
    let output = PathBuf::from(std::env::var_os("TI_Q5_OUTPUT").expect("TI_Q5_OUTPUT"));
    assert!(!output.exists(), "use a fresh output filename");
    let runs = std::env::var("TI_Q5_RUNS")
        .unwrap_or_else(|_| "7".into())
        .parse::<usize>()
        .unwrap();
    assert!(runs > 0 && runs <= 100);
    let engine = TiEngine::open(&root, None, None).await.unwrap();
    let cache = engine.query_cache_control().unwrap();
    cache.set_budget(256 * 1024 * 1024).unwrap();
    let mut variants = vec![];
    let mut expected = None;
    for (label, sql) in [
        ("distinct_from", DISTINCT),
        ("bitmap_equivalent", BITMAP_EQUIVALENT),
    ] {
        cache.clear().unwrap();
        let (cold, answer) = measure(&engine.session, sql).await;
        if let Some(ref expected) = expected {
            assert_eq!(&answer, expected, "A/B answer mismatch");
        } else {
            expected = Some(answer.clone());
        }
        let mut warm = vec![];
        for _ in 0..runs {
            let (timing, reply) = measure(&engine.session, sql).await;
            assert_eq!(reply, answer, "warm answer changed");
            warm.push(timing);
        }
        let mut totals: Vec<_> = warm
            .iter()
            .map(|r| r["total_ms"].as_f64().unwrap())
            .collect();
        let p50 = percentile(&mut totals, 0.5);
        let p95 = percentile(&mut totals, 0.95);
        println!("{label}: cold {:.3} ms, warm p50 {p50:.3} ms, p95 {p95:.3} ms; {} rows; materialized {}",
            cold["total_ms"].as_f64().unwrap(),answer.len(),warm[0]["materialized_rows"]);
        variants.push(json!({"label":label,"sql":sql,"cold":cold,"warm":warm,
            "p50_ms":p50,"p95_ms":p95,"values_match":true,"answer":answer,
            "cache_stats":cache.stats().unwrap()}));
    }
    let report = json!({"profile":if cfg!(debug_assertions) {"debug"} else {"release"},
        "runs":runs,"store":root,"cache_budget_bytes":256*1024*1024,
        "cold_definition":"decoded cache cleared; OS cache uncontrolled",
        "variants":variants});
    std::fs::create_dir_all(output.parent().unwrap()).unwrap();
    std::fs::write(output, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
}
