use datafusion::arrow::{
    array::{Array, Float64Array, Int64Array, StringArray},
    record_batch::RecordBatch,
};
use std::{collections::BTreeMap, sync::Arc};
use ti_contracts::{Agg, BucketRecord, FieldKind, FieldSpec, FieldValue, ShardKey, EPOCH};
use ti_core::{MemoryShard, MemorySource};
use ti_sql::{FixtureSource, SqlCatalog, SqlSession, VesselInfo};

async fn fixture() -> SqlSession {
    let fields = vec![
        FieldSpec {
            id: 1,
            path: "speed".into(),
            agg: Some(Agg::Mean),
            kind: FieldKind::Bsi { scale: 2 },
            units: Some("m/s".into()),
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
            path: "speed$source".into(),
            agg: None,
            kind: FieldKind::Set,
            units: None,
        },
        FieldSpec {
            id: 4,
            path: "present".into(),
            agg: None,
            kind: FieldKind::Presence,
            units: None,
        },
    ];
    let dictionaries = BTreeMap::from([
        (2, BTreeMap::from([(1, "on".into()), (2, "off".into())])),
        (3, BTreeMap::from([(1, "gps".into()), (2, "ais".into())])),
    ]);
    let vessels = (1..=2)
        .map(|ord| VesselInfo {
            ord,
            urn: format!("vessels.urn:test:{ord}"),
            name: None,
            mmsi: None,
            first_seen: EPOCH,
            last_seen: EPOCH + 65536,
        })
        .collect();
    let catalog = SqlCatalog::new(1, fields.clone(), vessels, dictionaries.clone()).unwrap();
    let mut memory = MemorySource::new();
    for (vessel, shard_ix) in [(1, 0), (1, 1), (2, 0)] {
        let mut shard = MemoryShard::new(ShardKey {
            vessel,
            shard: shard_ix,
        })
        .unwrap();
        for f in &fields {
            shard.register_field(f.clone()).unwrap();
        }
        for (field, dict) in &dictionaries {
            for (row, value) in dict {
                shard.register_set_value(*field, *row, value).unwrap();
            }
        }
        let mut records = Vec::new();
        for col in 0..4 {
            let bucket = (shard_ix << 16) | col;
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
            if col < 3 {
                put(1, FieldValue::Int([-100, 0, 125][col as usize]));
                put(2, FieldValue::SetValue(if col == 0 { 2 } else { 1 }));
            }
            if col == 0 {
                put(3, FieldValue::SetValue(1));
                put(3, FieldValue::SetValue(2));
            }
            if col == 1 {
                put(3, FieldValue::SetValue(2));
            }
        }
        shard.apply(&records).unwrap();
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
fn rows(batches: &[RecordBatch]) -> usize {
    batches.iter().map(RecordBatch::num_rows).sum()
}
async fn count(session: &SqlSession, condition: &str) -> i64 {
    let batches = session
        .query(&format!(
            "SELECT count(*) AS n FROM telemetry WHERE {condition}"
        ))
        .await
        .unwrap();
    batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .unwrap()
        .value(0)
}
#[tokio::test]
async fn numeric_null_logic_and_residuals() {
    let s = fixture().await;
    assert_eq!(count(&s, "speed >= 0").await, 6);
    assert_eq!(count(&s, "NOT (speed >= 0)").await, 3);
    assert_eq!(count(&s, "speed IS NULL").await, 3);
    assert_eq!(count(&s, "speed BETWEEN -1 AND 0").await, 6);
    assert_eq!(count(&s, "speed + 1 > 1").await, 3);
    assert_eq!(count(&s, "speed > 0 OR speed IS NULL").await, 6);
    assert_eq!(count(&s, "speed = 1.254").await, 3);
    let batches = s
        .query("SELECT speed, \"speed@mean\" FROM telemetry WHERE speed=1.25")
        .await
        .unwrap();
    assert_eq!(rows(&batches), 3);
    for b in batches {
        assert_eq!(
            b.column(0)
                .as_any()
                .downcast_ref::<Float64Array>()
                .unwrap()
                .value(0),
            1.25
        );
        assert_eq!(b.column(0).to_data(), b.column(1).to_data());
    }
}
#[tokio::test]
async fn sets_and_source_membership_preserve_nulls() {
    let s = fixture().await;
    assert_eq!(count(&s, "state IN ('on')").await, 6);
    assert_eq!(count(&s, "state NOT IN ('on')").await, 3);
    assert_eq!(count(&s, "state NOT IN ('on',NULL)").await, 0);
    assert_eq!(count(&s, "state IN ('on',NULL)").await, 6);
    assert_eq!(count(&s, "state IS DISTINCT FROM 'on'").await, 6);
    assert_eq!(count(&s, "\"speed$source\" = 'gps'").await, 3);
    assert_eq!(count(&s, "\"speed$source\" != 'gps'").await, 3);
    assert_eq!(count(&s, "\"speed$source\" IN ('gps','ais')").await, 6);
    assert_eq!(count(&s, "\"speed$source\" NOT IN ('gps',NULL)").await, 0);
    assert_eq!(count(&s, "\"speed$source\" IS NULL").await, 6);
}
#[tokio::test]
async fn timestamps_pruning_and_explain() {
    let s = fixture().await;
    assert_eq!(
        count(
            &s,
            "vessel='vessels.urn:test:1' AND ts < TIMESTAMP '2020-01-01 00:00:02.5'"
        )
        .await,
        3
    );
    assert_eq!(count(&s, "ts = TIMESTAMP '2020-01-01 00:00:00.5'").await, 0);
    assert_eq!(count(&s, "ts < TIMESTAMP '2019-12-31 23:59:59'").await, 0);
    let explain=s.explain("SELECT speed FROM telemetry WHERE vessel='vessels.urn:test:1' AND ts < TIMESTAMP '2020-01-01 00:00:04' AND speed>=0").await.unwrap();
    assert!(explain.contains("scanned=1 pruned=2"), "{explain}");
    assert!(explain.contains("Exact"), "{explain}");
    assert!(explain.contains("materialized rows=2"), "{explain}");
    assert!(explain.contains("bitmap cardinalities"), "{explain}");
    for (sql,counts) in [
        ("SELECT ts FROM telemetry WHERE vessel='vessels.urn:test:1' AND ts BETWEEN '2020-01-01 00:00:00.5' AND '2020-01-01 00:00:02.5'","scanned=1 pruned=2"),
        ("SELECT ts FROM telemetry WHERE ts > now() - INTERVAL '90 days'","scanned=0 pruned=3"),
        ("SELECT ts FROM telemetry WHERE ts IN (TIMESTAMP '2020-01-01 00:00:00.5')","scanned=0 pruned=3"),
    ] {
        let plan=s.explain(sql).await.unwrap();
        assert!(plan.contains("Exact") && !plan.contains("Unsupported"),"{plan}");
        assert!(plan.contains(counts),"{plan}");
    }
    assert_eq!(count(&s,"vessel='vessels.urn:test:1' AND ts BETWEEN '2020-01-01 00:00:00.5' AND '2020-01-01 00:00:02.5'").await,2);
}
#[tokio::test]
async fn catalogs_and_derived_write_guards() {
    let s = fixture().await;
    assert_eq!(rows(&s.query("SELECT * FROM vessels").await.unwrap()), 2);
    assert_eq!(rows(&s.query("SELECT * FROM paths").await.unwrap()), 4);
    assert_eq!(rows(&s.query("SELECT * FROM shards").await.unwrap()), 3);
    assert_eq!(rows(&s.query("SELECT * FROM docs").await.unwrap()), 0);
    for sql in [
        "DELETE FROM telemetry",
        "DROP TABLE paths",
        "INSERT INTO docs SELECT * FROM docs",
        "CREATE TABLE shards AS SELECT * FROM shards",
    ] {
        let error = s.query(sql).await.unwrap_err().to_string();
        assert!(error.contains("derived"), "{error}");
    }
    let batches = s
        .query("SELECT vessel FROM telemetry LIMIT 1")
        .await
        .unwrap();
    assert!(batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap()
        .value(0)
        .starts_with("vessels.urn:"));
    assert!(!batches[0].column(0).is_null(0));
}

#[tokio::test]
async fn read_only_sql_features_remain_available() {
    let s = fixture().await;
    for (sql,n) in [
        ("WITH a AS (SELECT vessel, speed FROM telemetry) SELECT * FROM a WHERE speed>0",3),
        ("WITH a AS (SELECT 'ordinary-string' AS ts, 1.254 AS speed) SELECT * FROM a WHERE ts='ordinary-string' AND speed=1.254",1),
        ("SELECT t.vessel,t.speed FROM telemetry t JOIN vessels v ON t.vessel=v.urn WHERE t.speed>0",3),
        ("SELECT vessel, speed, lag(speed) OVER (PARTITION BY vessel ORDER BY ts) FROM telemetry",12),
        ("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<3) SELECT * FROM n",3),
        ("SELECT vessel FROM telemetry WHERE speed>0 UNION SELECT vessel FROM telemetry WHERE speed<0",2),
        ("SELECT vessel,date_bin(INTERVAL '1 hour',ts),avg(speed) FROM telemetry GROUP BY 1,2",3),
        ("SELECT vessel,CASE WHEN speed>0 THEN 'positive' ELSE 'other' END FROM telemetry ORDER BY ts LIMIT 2",2),
        ("SELECT t.vessel FROM telemetry t WHERE speed>(SELECT avg(speed) FROM telemetry)",3),
    ] {
        assert_eq!(rows(&s.query(sql).await.unwrap()),n,"{sql}");
    }
}

#[tokio::test]
async fn prepared_timestamp_parameters_preserve_precision_and_pushdown() {
    use datafusion::common::ScalarValue;
    let s = fixture().await;
    let frame = s
        .prepare(
            "SELECT ts FROM telemetry WHERE vessel='vessels.urn:test:1' AND ts > $1 AND ts < $2",
        )
        .await
        .unwrap();
    let frame = frame
        .with_param_values(vec![
            ScalarValue::TimestampNanosecond(
                Some(EPOCH * 1_000_000_000 + 1_500_000_000),
                Some("UTC".into()),
            ),
            ScalarValue::TimestampNanosecond(
                Some(EPOCH * 1_000_000_000 + 3_500_000_000),
                Some("UTC".into()),
            ),
        ])
        .unwrap();
    let batches = frame.collect().await.unwrap();
    assert_eq!(rows(&batches), 2);
    let reports = s.reports().unwrap();
    let scan = reports.iter().find(|r| r.total_shards == 3).unwrap();
    assert_eq!(scan.scanned_shards, 1);
    assert!(
        scan.filters.iter().all(|(_, class, _)| class == "Exact"),
        "{scan:?}"
    );
    // The internal function is not callable through ordinary SQL name lookup.
    assert!(s
        .prepare("SELECT ti_timestamp(ts) FROM telemetry")
        .await
        .is_err());
}
