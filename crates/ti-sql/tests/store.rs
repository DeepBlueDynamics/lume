use std::{collections::BTreeMap, path::Path, sync::Arc};
use ti_contracts::{Catalog, FieldValue, ShardSink, VesselSpec};
use ti_sql::{session_from_store, verify, FixtureSnapshot};
use ti_store::Store;

#[tokio::test]
async fn durable_store_materialization_catalogs_and_stored_outputs() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixture");
    let mut snapshot: FixtureSnapshot =
        serde_json::from_slice(&std::fs::read(fixture.join("snapshot.json")).unwrap()).unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("test-output")
        .join(format!("store-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let mut store = Store::open_or_create(&root, snapshot.width_seconds).unwrap();
    let mut vessels = BTreeMap::new();
    for vessel in &snapshot.vessels {
        let ord = store
            .catalog()
            .register_vessel(&VesselSpec {
                urn: vessel.urn.clone(),
                name: vessel.name.clone(),
                mmsi: vessel.mmsi.clone(),
            })
            .unwrap();
        vessels.insert(vessel.ord, ord);
    }
    let mut fields = BTreeMap::new();
    for field in &snapshot.fields {
        fields.insert(field.id, store.catalog().register_field(field).unwrap());
    }
    let mut rows = BTreeMap::new();
    for (field, values) in &snapshot.dictionaries {
        for (row, value) in values {
            rows.insert(
                (*field, *row),
                store
                    .catalog()
                    .register_set_value(fields[field], value)
                    .unwrap(),
            );
        }
    }
    for record in &mut snapshot.records {
        record.vessel = vessels[&record.vessel];
        if let FieldValue::SetValue(row) = &mut record.value {
            *row = rows[&(record.field, *row)];
        }
        record.field = fields[&record.field];
    }
    store.apply(&snapshot.records).unwrap();
    // Both mutable and reopened sealed paths must agree with the scalar oracle.
    let session = session_from_store(Arc::new(store), 1).await.unwrap();
    let report = verify(&session, &fixture, None, None).await.unwrap();
    assert_eq!(report.failed, 0, "{report:?}");
    assert_eq!((report.passed, report.excluded), (6, 4));
    let all = session
        .query("SELECT * FROM telemetry ORDER BY ts")
        .await
        .unwrap();
    assert_eq!(all.iter().map(|b| b.num_rows()).sum::<usize>(), 4);
    let first = &all[0];
    assert_eq!(
        first.column(first.schema().index_of("speed").unwrap()),
        first.column(first.schema().index_of("speed@mean").unwrap())
    );
    assert_eq!(
        first
            .column(first.schema().index_of("notes").unwrap())
            .null_count(),
        first.num_rows()
    );
    drop(session);
    let mut reopened = Store::open_or_create(&root, 1).unwrap();
    // Initial Arc dropped without shutdown; buffered WAL is replayed on reopen.
    reopened
        .seal(ti_contracts::ShardKey {
            vessel: vessels[&1],
            shard: 0,
        })
        .unwrap();
    reopened.shutdown().unwrap();
    drop(reopened);
    let session = ti_sql::open_store(&root, 1).await.unwrap();
    let report = verify(&session, &fixture, None, None).await.unwrap();
    assert_eq!(report.failed, 0, "{report:?}");
    assert_eq!((report.passed, report.excluded), (6, 4));
    let reopened_all = session
        .query("SELECT * FROM telemetry ORDER BY ts")
        .await
        .unwrap();
    assert_eq!(
        ti_sql::rows_json(&all).unwrap(),
        ti_sql::rows_json(&reopened_all).unwrap()
    );
    let sealed = session
        .query("SELECT sealed, bytes, hash FROM shards")
        .await
        .unwrap();
    let rows = ti_sql::rows_json(&sealed).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["sealed"], true);
    assert!(rows[0]["bytes"].as_u64().unwrap() > 0);
    assert!(rows[0]["hash"].as_str().unwrap().len() == 64);
    drop(session);
    std::fs::remove_dir_all(root).unwrap();
}
