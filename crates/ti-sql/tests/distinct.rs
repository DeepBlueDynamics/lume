use datafusion::{
    common::ScalarValue,
    logical_expr::{BinaryExpr, Expr, Operator, TableProviderFilterPushDown},
    prelude::{col, lit},
};
use std::{collections::BTreeMap, sync::Arc};
use ti_contracts::{Agg, BucketRecord, FieldKind, FieldSpec, FieldValue, ShardKey, EPOCH};
use ti_core::{MemoryShard, MemorySource};
use ti_sql::{FixtureSource, PushdownClassifier, SqlCatalog, SqlSession, VesselInfo};

async fn fixture() -> (
    SqlSession,
    Vec<MemoryShard>,
    datafusion::prelude::SessionContext,
) {
    let fields = vec![
        FieldSpec {
            id: 1,
            path: "n".into(),
            agg: Some(Agg::Mean),
            kind: FieldKind::Bsi { scale: 3 },
            units: None,
        },
        FieldSpec {
            id: 2,
            path: "state".into(),
            agg: None,
            kind: FieldKind::Set,
            units: None,
        },
        FieldSpec {
            id: 3,
            path: "samples".into(),
            agg: None,
            kind: FieldKind::Count,
            units: None,
        },
        FieldSpec {
            id: 4,
            path: "marker".into(),
            agg: None,
            kind: FieldKind::Presence,
            units: None,
        },
        FieldSpec {
            id: 5,
            path: "n$source".into(),
            agg: None,
            kind: FieldKind::Set,
            units: None,
        },
        FieldSpec {
            id: 6,
            path: "big".into(),
            agg: Some(Agg::Mean),
            kind: FieldKind::Bsi { scale: 0 },
            units: None,
        },
        FieldSpec {
            id: 7,
            path: "missing".into(),
            agg: Some(Agg::Mean),
            kind: FieldKind::Bsi { scale: 3 },
            units: None,
        },
    ];
    let dictionaries = BTreeMap::from([
        (2, BTreeMap::from([(0, "on".into()), (1, "off".into())])),
        (5, BTreeMap::from([(0, "gps".into())])),
    ]);
    let vessels = (0..2)
        .map(|ord| VesselInfo {
            ord,
            urn: format!("vessels.urn:test:{ord}"),
            name: None,
            mmsi: None,
            first_seen: EPOCH,
            last_seen: EPOCH + 700000,
        })
        .collect();
    let catalog = SqlCatalog::new(10, fields.clone(), vessels, dictionaries.clone()).unwrap();
    let mut memory = MemorySource::new();
    let mut retained = vec![];
    for vessel in 0..2 {
        for shard_ix in 0..2 {
            let mut shard = MemoryShard::new(ShardKey {
                vessel,
                shard: shard_ix,
            })
            .unwrap();
            for field in &fields {
                shard.register_field(field.clone()).unwrap();
            }
            for (field, dict) in &dictionaries {
                for (row, value) in dict {
                    shard.register_set_value(*field, *row, value).unwrap();
                }
            }
            let mut records = vec![];
            for slot in 0..6 {
                let bucket = 65533 + slot;
                if bucket >> 16 != shard_ix {
                    continue;
                }
                let mut put = |field, value| {
                    records.push(BucketRecord {
                        vessel,
                        bucket,
                        field,
                        value,
                        rewrite: false,
                    })
                };
                put(4, FieldValue::Present);
                put(5, FieldValue::SetValue(0));
                if let Some(value) =
                    [Some(1250), Some(-250), Some(0), None, Some(1250), None][slot as usize]
                {
                    put(1, FieldValue::Int(value));
                }
                if let Some(row) =
                    [Some(vessel), Some(1 - vessel), None, Some(0), None, None][slot as usize]
                {
                    put(2, FieldValue::SetValue(row));
                }
                if let Some(value) = [Some(2), Some(0), None, Some(7), Some(2), None][slot as usize]
                {
                    put(3, FieldValue::Int(value));
                }
                if let Some(value) = [
                    Some(9007199254740992),
                    Some(9007199254740993),
                    Some(9007199254740991),
                    None,
                    Some(9007199254740994),
                    None,
                ][slot as usize]
                {
                    put(6, FieldValue::Int(value));
                }
            }
            shard.apply(&records).unwrap();
            retained.push(shard.clone());
            memory.insert(shard);
        }
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
    let batches = session
        .query(r#"SELECT vessel,ts,"n@mean" AS n,state,samples,"big@mean" AS big,"missing@mean" AS missing FROM telemetry"#)
        .await
        .unwrap();
    let baseline = datafusion::prelude::SessionContext::new();
    let table =
        datafusion::datasource::MemTable::try_new(batches[0].schema(), vec![batches]).unwrap();
    baseline
        .register_table("telemetry", Arc::new(table))
        .unwrap();
    (session, retained, baseline)
}
fn canonical(batches: &[datafusion::arrow::record_batch::RecordBatch]) -> Vec<String> {
    let mut rows: Vec<_> = ti_sql::rows_json(batches)
        .unwrap()
        .iter()
        .map(|r| serde_json::to_string(r).unwrap())
        .collect();
    rows.sort();
    rows
}
async fn compare(
    session: &SqlSession,
    baseline: &datafusion::prelude::SessionContext,
    condition: &str,
) {
    session.reset_diagnostics().unwrap();
    let sql = format!(
        r#"SELECT vessel,ts,"n@mean" AS n,state,samples,"big@mean" AS big,"missing@mean" AS missing FROM telemetry WHERE {condition}"#
    );
    let pushed = session.query(&sql).await.unwrap();
    let reports = session.reports().unwrap();
    assert!(
        reports
            .iter()
            .flat_map(|r| &r.filters)
            .all(|(_, class, _)| class != "Unsupported"),
        "{condition}: {reports:?}"
    );
    // MemTable is an independent ordinary Arrow/DataFusion execution path with
    // no Lume bitmap provider or fixed-point predicate translation.
    let sql =
        format!("SELECT vessel,ts,n,state,samples,big,missing FROM telemetry WHERE {condition}");
    let residual = baseline.sql(&sql).await.unwrap().collect().await.unwrap();
    assert_eq!(canonical(&pushed), canonical(&residual), "{condition}");
}

#[tokio::test]
async fn scalar_distinct_and_negations_match_residual_rows_and_are_two_valued() {
    let (session, shards, baseline) = fixture().await;
    let classifier = PushdownClassifier {
        catalog: session.catalog.clone(),
    };
    for (name, value, text) in [
        ("n@mean", ScalarValue::Float64(Some(1.25)), "1.25"),
        ("n@mean", ScalarValue::Float64(None), "NULL"),
        ("n@mean", ScalarValue::Float64(Some(-0.25)), "-0.25"),
        (
            "big@mean",
            ScalarValue::Float64(Some(9007199254740992.0)),
            "9007199254740992",
        ),
        ("state", ScalarValue::Utf8(Some("on".into())), "'on'"),
        (
            "state",
            ScalarValue::Utf8(Some("unseen".into())),
            "'unseen'",
        ),
        ("state", ScalarValue::Utf8(None), "NULL"),
        ("samples", ScalarValue::UInt64(Some(2)), "2"),
        ("samples", ScalarValue::UInt64(None), "NULL"),
        ("missing@mean", ScalarValue::Float64(Some(0.0)), "0"),
        ("missing@mean", ScalarValue::Float64(None), "NULL"),
    ] {
        for (op, operator) in [
            (Operator::IsDistinctFrom, "IS DISTINCT FROM"),
            (Operator::IsNotDistinctFrom, "IS NOT DISTINCT FROM"),
        ] {
            let expression = Expr::BinaryExpr(BinaryExpr::new(
                Box::new(col(name)),
                op,
                Box::new(lit(value.clone())),
            ));
            let classification = classifier.classify(&expression).unwrap();
            assert_eq!(
                classification.class,
                TableProviderFilterPushDown::Exact,
                "{name} {operator} {text}"
            );
            for shard in &shards {
                let predicate = classification
                    .predicate
                    .as_ref()
                    .unwrap()
                    .for_shard(shard.key);
                let mask = shard.eval_masks(&predicate, None, None).unwrap();
                assert!(
                    mask.unknown.is_empty(),
                    "{name} {operator} {text}: {mask:?}"
                );
                let negated = classifier
                    .classify(&Expr::Not(Box::new(expression.clone())))
                    .unwrap();
                let negated = shard
                    .eval_masks(&negated.predicate.unwrap().for_shard(shard.key), None, None)
                    .unwrap();
                assert!(negated.unknown.is_empty());
                assert_eq!(negated.truth, mask.falsity);
                assert_eq!(&mask.truth | &negated.truth, shard.universe());
            }
            let sql_name = name.strip_suffix("@mean").unwrap_or(name);
            let condition = format!("{sql_name} {operator} {text}");
            for condition in [
                condition.clone(),
                format!("NOT ({condition})"),
                format!("NOT (NOT ({condition}))"),
                format!("({condition}) OR NOT ({condition})"),
                format!("NOT (({condition}) AND samples IS NOT DISTINCT FROM 0)"),
                format!("{text} {operator} {sql_name}"),
            ] {
                compare(&session, &baseline, &condition).await;
            }
        }
    }
}
#[tokio::test]
async fn arrays_and_column_comparisons_remain_residual() {
    let (session, _, baseline) = fixture().await;
    let classifier = PushdownClassifier {
        catalog: session.catalog.clone(),
    };
    for right in [lit("gps"), col("big@mean")] {
        let expression = Expr::BinaryExpr(BinaryExpr::new(
            Box::new(col("n$source")),
            Operator::IsDistinctFrom,
            Box::new(right),
        ));
        assert_eq!(
            classifier.classify(&expression).unwrap().class,
            TableProviderFilterPushDown::Unsupported
        );
    }
    let sql = "SELECT vessel,ts FROM telemetry WHERE n IS DISTINCT FROM big";
    let residual = "SELECT vessel,ts FROM telemetry WHERE n IS DISTINCT FROM big";
    assert_eq!(
        canonical(&session.query(sql).await.unwrap()),
        canonical(
            &baseline
                .sql(residual)
                .await
                .unwrap()
                .collect()
                .await
                .unwrap()
        )
    );
    assert!(session.explain(sql).await.unwrap().contains("FilterExec"));
}
