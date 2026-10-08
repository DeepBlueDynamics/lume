use datafusion::{
    arrow::{
        array::{ArrayRef, Float64Array, StringArray, TimestampSecondArray},
        datatypes::{DataType, Field, Schema},
        record_batch::RecordBatch,
    },
    datasource::MemTable,
};
use std::{collections::BTreeMap, sync::Arc};
use ti_contracts::{Agg, BucketRecord, FieldKind, FieldSpec, FieldValue, ShardKey, EPOCH};
use ti_core::{MemoryShard, MemorySource};
use ti_sql::{FixtureSource, SqlCatalog, SqlSession, VesselInfo};

async fn fixture(enabled: bool, nullable: bool, large: bool) -> SqlSession {
    let field = FieldSpec {
        id: 1,
        path: "speed".into(),
        agg: Some(Agg::Mean),
        kind: FieldKind::Bsi { scale: 0 },
        units: None,
    };
    let catalog = SqlCatalog::new(
        10,
        vec![field.clone()],
        (0..2)
            .map(|ord| VesselInfo {
                ord,
                urn: format!("vessels.urn:test:{ord}"),
                name: None,
                mmsi: None,
                first_seen: EPOCH,
                last_seen: EPOCH + 3_000_000,
            })
            .collect(),
        BTreeMap::new(),
    )
    .unwrap();
    let mut memory = MemorySource::new();
    for vessel in 0..2 {
        for shard_id in [0, 1, 3] {
            let mut shard = MemoryShard::new(ShardKey {
                vessel,
                shard: shard_id,
            })
            .unwrap();
            shard.register_field(field.clone()).unwrap();
            let buckets: Vec<_> = match shard_id {
                0 => vec![1, 65534, 65535],
                1 => vec![65536, 65537, 65538],
                _ => vec![196608],
            };
            shard
                .apply(
                    &buckets
                        .into_iter()
                        .map(|bucket| BucketRecord {
                            vessel,
                            bucket,
                            field: 1,
                            value: FieldValue::Int(i64::from(bucket % 3)),
                            rewrite: false,
                        })
                        .collect::<Vec<_>>(),
                )
                .unwrap();
            memory.insert(shard);
        }
    }
    let session = SqlSession::new_with_document_range_pruning(
        Arc::new(FixtureSource {
            memory,
            catalog: catalog.clone(),
        }),
        catalog,
        enabled,
    )
    .await
    .unwrap();
    let n = if large { 4097 } else { 3 };
    let ids = (0..n).map(|i| format!("d{i}")).collect::<Vec<_>>();
    let vessels = (0..n)
        .map(|i| format!("vessels.urn:test:{}", usize::from(i == 2)))
        .collect::<Vec<_>>();
    let starts = (0..n)
        .map(|i| EPOCH + if i == 1 { 65535 * 10 + 1 } else { 65534 * 10 })
        .collect::<Vec<_>>();
    let ends = (0..n)
        .map(|i| {
            if nullable && i == 1 {
                None
            } else {
                Some(EPOCH + if i == 1 { 65538 * 10 } else { 65536 * 10 })
            }
        })
        .collect::<Vec<_>>();
    let mut fields = ti_contracts::docs_schema().fields().to_vec();
    fields.push(Arc::new(Field::new("entity", DataType::Utf8, false)));
    let schema = Arc::new(Schema::new(fields));
    let string = |items: Vec<String>| Arc::new(StringArray::from(items)) as ArrayRef;
    let columns: Vec<ArrayRef> = vec![
        string(ids),
        string(vessels.clone()),
        string(vec!["alerts".into(); n]),
        Arc::new(TimestampSecondArray::from(starts).with_timezone("UTC")),
        Arc::new(TimestampSecondArray::from(ends).with_timezone("UTC")),
        string(vec!["alarm".into(); n]),
        string(vec!["alarm".into(); n]),
        Arc::new(Float64Array::from(vec![Some(1.0); n])),
        string(vessels),
    ];
    let batch = RecordBatch::try_new(schema.clone(), columns).unwrap();
    session
        .register_derived_table(
            "docs",
            Arc::new(MemTable::try_new(schema, vec![vec![batch]]).unwrap()),
        )
        .unwrap();
    session
}
fn canonical(batches: &[RecordBatch]) -> Vec<String> {
    let mut rows = ti_sql::rows_json(batches)
        .unwrap()
        .into_iter()
        .map(|r| serde_json::to_string(&r).unwrap())
        .collect::<Vec<_>>();
    rows.sort();
    rows
}
const QUERY: &str = "SELECT d.id,t.vessel,t.ts,t.speed FROM docs d JOIN telemetry t ON d.vessel=t.vessel AND t.ts>=d.ts_start AND t.ts<=d.ts_end WHERE t.speed>=0";
#[tokio::test]
async fn inclusive_ranges_overlap_vessels_offgrid_and_shard_boundaries_equal_residual() {
    let pushed = fixture(true, false, false).await;
    let residual = fixture(false, false, false).await;
    for sql in [
        QUERY.to_owned(),
        QUERY.replace("t.ts>=", "t.ts>").replace("t.ts<=", "t.ts<"),
        QUERY
            .replace("t.ts>=d.ts_start", "d.ts_start<=t.ts")
            .replace("t.ts<=d.ts_end", "d.ts_end>=t.ts"),
        QUERY.replace("docs d JOIN telemetry t", "telemetry t JOIN docs d"),
        QUERY.replace("WHERE t.speed>=0", "WHERE t.speed>=0 AND d.id='d0'"),
    ] {
        let a = canonical(&pushed.query(&sql).await.unwrap());
        let b = canonical(&residual.query(&sql).await.unwrap());
        assert_eq!(a, b, "{sql}");
        assert!(!a.is_empty(), "{sql}");
        let plan = pushed.explain(&sql).await.unwrap();
        assert!(
            plan.contains("document join vessel/time range union"),
            "{plan}"
        );
        assert!(plan.contains("HashJoinExec"), "{plan}");
    }
    let a = canonical(&pushed.query(QUERY).await.unwrap());
    // d0: 3 buckets, d1: 3 buckets, d2: 3 buckets. Overlap duplicates survive.
    assert_eq!(a.len(), 9);
    pushed.reset_diagnostics().unwrap();
    pushed.query(QUERY).await.unwrap();
    let reports = pushed.reports().unwrap();
    assert!(reports.iter().map(|r| r.materialized_rows).sum::<u64>() < 14);
}
#[tokio::test]
async fn null_ranges_large_builds_and_unrecognised_joins_keep_residual() {
    for (nullable, large) in [(true, false), (false, true)] {
        let a = fixture(true, nullable, large).await;
        let b = fixture(false, nullable, large).await;
        assert_eq!(
            canonical(&a.query(QUERY).await.unwrap()),
            canonical(&b.query(QUERY).await.unwrap())
        );
        assert!(!a
            .explain(QUERY)
            .await
            .unwrap()
            .contains("document join vessel/time range union"));
    }
    for sql in [
        QUERY
            .replace("JOIN telemetry", "LEFT JOIN telemetry")
            .replace(" WHERE t.speed>=0", ""),
        QUERY.replace(
            "t.ts>=d.ts_start AND t.ts<=d.ts_end",
            "(t.ts>=d.ts_start OR t.ts<=d.ts_end)",
        ),
        QUERY.replace("t.ts>=d.ts_start", "t.ts+INTERVAL '1 second'>=d.ts_start"),
        QUERY.replace("AND t.ts<=d.ts_end", ""),
        QUERY.replace("t.ts<=d.ts_end", "t.ts<=d.ts_end AND t.speed+1>0"),
    ] {
        let a = fixture(true, false, false).await;
        let b = fixture(false, false, false).await;
        assert_eq!(
            canonical(&a.query(&sql).await.unwrap()),
            canonical(&b.query(&sql).await.unwrap()),
            "{sql}"
        );
        assert!(
            !a.explain(&sql)
                .await
                .unwrap()
                .contains("document join vessel/time range union"),
            "{sql}"
        );
    }
}
