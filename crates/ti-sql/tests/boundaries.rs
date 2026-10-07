use datafusion::arrow::{
    array::{Float64Array, Int64Array},
    record_batch::RecordBatch,
};
use std::{collections::BTreeMap, sync::Arc};
use ti_contracts::{Agg, BucketRecord, FieldKind, FieldSpec, FieldValue, ShardKey, EPOCH};
use ti_core::{MemoryShard, MemorySource};
use ti_sql::{FixtureSource, SqlCatalog, SqlSession, VesselInfo};
async fn fixture(values: &[i64]) -> SqlSession {
    let field = FieldSpec {
        id: 1,
        path: "x".into(),
        agg: Some(Agg::Mean),
        kind: FieldKind::Bsi { scale: 0 },
        units: None,
    };
    let catalog = SqlCatalog::new(
        1,
        vec![field.clone()],
        vec![VesselInfo {
            ord: 0,
            urn: "vessels.urn:test:0".into(),
            name: None,
            mmsi: None,
            first_seen: EPOCH,
            last_seen: EPOCH + values.len() as i64,
        }],
        BTreeMap::new(),
    )
    .unwrap();
    let mut shard = MemoryShard::new(ShardKey {
        vessel: 0,
        shard: 0,
    })
    .unwrap();
    shard.register_field(field).unwrap();
    let records = values
        .iter()
        .enumerate()
        .map(|(bucket, value)| BucketRecord {
            vessel: 0,
            bucket: bucket as u32,
            field: 1,
            value: FieldValue::Int(*value),
            rewrite: false,
        })
        .collect::<Vec<_>>();
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
#[tokio::test]
async fn exact_numeric_filters_agree_with_projected_float_collisions() {
    let s = fixture(&[
        9_007_199_254_740_991,
        9_007_199_254_740_992,
        9_007_199_254_740_993,
        9_007_199_254_740_994,
    ])
    .await;
    for (sql, n) in [
        ("x=9007199254740992", 2),
        ("x<9007199254740992", 1),
        ("x>9007199254740992", 1),
        ("x!=9007199254740992", 2),
    ] {
        let batches = s
            .query(&format!("SELECT count(*) FROM telemetry WHERE {sql}"))
            .await
            .unwrap();
        assert_eq!(
            batches[0]
                .column(0)
                .as_any()
                .downcast_ref::<Int64Array>()
                .unwrap()
                .value(0),
            n,
            "{sql}"
        );
    }
    let batches = s
        .query("SELECT x FROM telemetry WHERE x=9007199254740992")
        .await
        .unwrap();
    for b in batches {
        for v in b
            .column(0)
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap()
            .values()
        {
            assert_eq!(*v, 9_007_199_254_740_992.0);
        }
    }
}
#[tokio::test]
async fn streamed_batches_and_zero_column_projection() {
    let s = fixture(&vec![1; 8193]).await;
    let batches = s.query("SELECT x FROM telemetry").await.unwrap();
    assert_eq!(
        batches.iter().map(RecordBatch::num_rows).sum::<usize>(),
        8193
    );
    assert!(batches.iter().all(|b| b.num_rows() <= 8192));
    let batches = s.query("SELECT count(*) FROM telemetry").await.unwrap();
    assert_eq!(
        batches[0]
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap()
            .value(0),
        8193
    );
    let explain = s
        .explain("SELECT x FROM telemetry WHERE x+1>1")
        .await
        .unwrap();
    assert!(explain.contains("Unsupported"), "{explain}");
    let explain = s
        .query("EXPLAIN SELECT x FROM telemetry WHERE x=1")
        .await
        .unwrap();
    assert_eq!(explain.iter().map(RecordBatch::num_rows).sum::<usize>(), 1);
}

/// String functions an analyst or an LLM reaches for. Without DataFusion's
/// `unicode_expressions` and `regex_expressions` features these failed to plan
/// ("Substring could not be planned by registered expr planner").
#[tokio::test]
async fn unicode_and_regex_string_functions_plan_and_run() {
    let s = fixture(&[1, 2]).await;
    let batches = s
        .query(
            "SELECT substring(vessel, 1, 8) AS head, substr(vessel, 9) AS tail, \
             left(vessel, 7) AS l, right(vessel, 6) AS r, strpos(vessel, 'urn') AS at, \
             lpad('7', 3, '0') AS padded, regexp_like(vessel, 'test:[0-9]') AS matches, \
             regexp_replace(vessel, '^vessels[.]', '') AS bare \
             FROM telemetry LIMIT 1",
        )
        .await
        .unwrap();
    let text = datafusion::arrow::util::pretty::pretty_format_batches(&batches)
        .unwrap()
        .to_string();
    for expected in [
        "| vessels. ",
        "urn:test:0",
        "| vessels ",
        "test:0",
        "| 9 ",
        "| 007 ",
        "| true ",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in\n{text}");
    }
}
