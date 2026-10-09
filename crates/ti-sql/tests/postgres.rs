use std::{collections::BTreeMap, path::PathBuf, sync::Arc};
use ti_contracts::{Agg, BucketRecord, FieldKind, FieldSpec, FieldValue, ShardKey};
use ti_core::{MemoryShard, MemorySource};
use ti_sql::{FixtureSource, SqlCatalog, SqlSession, TiEngine, VesselInfo};
async fn engine() -> TiEngine {
    let fields = vec![FieldSpec {
        id: 1,
        path: "navigation.speedOverGround".into(),
        agg: Some(Agg::Mean),
        kind: FieldKind::Bsi { scale: 3 },
        units: Some("m/s".into()),
    }];
    let catalog = SqlCatalog::new(
        10,
        fields.clone(),
        vec![VesselInfo {
            ord: 1,
            urn: "vessels.urn:test:http".into(),
            name: None,
            mmsi: None,
            first_seen: ti_contracts::EPOCH + 10,
            last_seen: ti_contracts::EPOCH + 20,
        }],
        BTreeMap::new(),
    )
    .unwrap();
    let mut shard = MemoryShard::new(ShardKey {
        vessel: 1,
        shard: 0,
    })
    .unwrap();
    shard.register_field(fields[0].clone()).unwrap();
    shard
        .apply(&[
            BucketRecord {
                vessel: 1,
                bucket: 1,
                field: 1,
                value: FieldValue::Int(2000),
                rewrite: false,
            },
            BucketRecord {
                vessel: 1,
                bucket: 2,
                field: 1,
                value: FieldValue::Int(4000),
                rewrite: false,
            },
        ])
        .unwrap();
    let mut memory = MemorySource::new();
    memory.insert(shard);
    let session = SqlSession::new(
        Arc::new(FixtureSource {
            memory,
            catalog: catalog.clone(),
        }),
        catalog,
    )
    .await
    .unwrap();
    let engine = TiEngine::from_session(session, PathBuf::new());
    ti_sql::postgres::register(&engine).await.unwrap();
    engine
}
#[tokio::test]
async fn grafana_13_and_psql_16_catalog_and_macro_queries() {
    let engine = engine().await;
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../../tests/golden/grafana-pg.json")).unwrap();
    for case in fixture["queries"].as_array().unwrap() {
        let sql = case["sql"].as_str().unwrap();
        if sql.is_empty() {
            continue;
        }
        let result = ti_sql::postgres::query(&engine, sql, vec![])
            .await
            .unwrap_or_else(|e| panic!("{}: {e}", case["id"]));
        assert_eq!(
            result["row_count"], case["rows"],
            "{}: {result}",
            case["id"]
        );
    }
}
#[tokio::test]
async fn typed_parameters_and_readonly_guard() {
    let engine = engine().await;
    let sql = "SELECT CAST($1 AS BIGINT) AS n, CAST($2 AS TEXT) AS label";
    let description = ti_sql::postgres::describe(&engine, sql, &[]).await.unwrap();
    assert_eq!(description["parameters"][0], "Int64");
    assert_eq!(
        ti_sql::postgres::type_name(description["parameters"][1].as_str().unwrap()).1,
        25
    );
    let result = ti_sql::postgres::query(
        &engine,
        sql,
        vec![
            ti_sql::postgres::Parameter::Int64(Some(42)),
            ti_sql::postgres::Parameter::Utf8(Some("O'Brien; DELETE FROM telemetry".into())),
        ],
    )
    .await
    .unwrap();
    assert_eq!(result["rows"][0]["n"], 42);
    assert_eq!(result["rows"][0]["label"], "O'Brien; DELETE FROM telemetry");
    for sql in [
        "DELETE FROM telemetry",
        "CREATE TABLE x (n INT)",
        "SELECT 1; DELETE FROM telemetry",
        "EXPLAIN DELETE FROM telemetry",
    ] {
        assert!(
            ti_sql::postgres::describe(&engine, sql, &[]).await.is_err(),
            "{sql}"
        );
    }
}
