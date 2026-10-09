//! A flush rewrites only the fields changed since the previous flush (D47), and a
//! reopened store still sees every field.

use std::fs;

use tempfile::tempdir;
use ti_contracts::{
    Agg, BucketRecord, Catalog, FieldKind, FieldSpec, FieldValue, ShardKey, ShardSink, VesselSpec,
};
use ti_store::Store;

fn record(vessel: u32, bucket: u32, field: u32, value: i64) -> BucketRecord {
    BucketRecord {
        vessel,
        bucket,
        field,
        value: FieldValue::Int(value),
        rewrite: false,
    }
}

#[test]
fn flush_rewrites_only_changed_fields_and_reopen_keeps_all() {
    let dir = tempdir().unwrap();
    let mut store = Store::open_or_create(dir.path(), 10).unwrap();
    let vessel = store
        .catalog()
        .register_vessel(&VesselSpec {
            urn: "vessels.urn:partial".into(),
            name: None,
            mmsi: None,
        })
        .unwrap();
    let mut fields = Vec::new();
    for (id, path) in ["navigation.speedOverGround", "environment.depth.belowKeel"]
        .into_iter()
        .enumerate()
    {
        fields.push(
            store
                .catalog()
                .register_field(&FieldSpec {
                    id: id as u32,
                    path: path.into(),
                    agg: Some(Agg::Mean),
                    kind: FieldKind::Bsi { scale: 3 },
                    units: None,
                })
                .unwrap(),
        );
    }
    let (speed, depth) = (fields[0], fields[1]);

    store
        .apply(&[record(vessel, 1, speed, 10), record(vessel, 1, depth, 20)])
        .unwrap();
    store.flush().unwrap();

    let open = dir
        .path()
        .join("shards")
        .join(vessel.to_string())
        .join("0/open");
    let depth_file = open.join(format!("{depth}.rbm"));
    let speed_file = open.join(format!("{speed}.rbm"));
    let depth_before = fs::read(&depth_file).unwrap();
    let speed_before = fs::read(&speed_file).unwrap();
    let depth_modified = fs::metadata(&depth_file).unwrap().modified().unwrap();

    std::thread::sleep(std::time::Duration::from_millis(20));
    store.apply(&[record(vessel, 2, speed, 11)]).unwrap();
    store.flush().unwrap();

    assert_ne!(
        fs::read(&speed_file).unwrap(),
        speed_before,
        "changed field rewritten"
    );
    assert_eq!(fs::read(&depth_file).unwrap(), depth_before);
    assert_eq!(
        fs::metadata(&depth_file).unwrap().modified().unwrap(),
        depth_modified,
        "unchanged field must not be rewritten"
    );
    assert!(
        fs::read_dir(&open)
            .unwrap()
            .flatten()
            .all(|e| !e.file_name().to_string_lossy().contains(".tmp.")),
        "no temporary files left behind"
    );

    let key = ShardKey { vessel, shard: 0 };
    let expected = store.open_shard(&key).unwrap().data.fields.clone();
    store.shutdown().unwrap();
    drop(store);

    let reopened = Store::open_or_create(dir.path(), 10).unwrap();
    assert_eq!(reopened.open_shard(&key).unwrap().data.fields, expected);
}

/// One flush can stage more field files than the default 1,024 open-file limit.
#[test]
fn flush_stages_more_fields_than_the_descriptor_limit() {
    let dir = tempdir().unwrap();
    let mut store = Store::open_or_create(dir.path(), 10).unwrap();
    let vessel = store
        .catalog()
        .register_vessel(&VesselSpec {
            urn: "vessels.urn:many-fields".into(),
            name: None,
            mmsi: None,
        })
        .unwrap();
    let mut records = Vec::new();
    for id in 0..1_500u32 {
        let field = store
            .catalog()
            .register_field(&FieldSpec {
                id,
                path: format!("sensors.s{id}"),
                agg: Some(Agg::Mean),
                kind: FieldKind::Bsi { scale: 0 },
                units: None,
            })
            .unwrap();
        records.push(record(vessel, 1, field, i64::from(id)));
    }
    store.apply(&records).unwrap();
    store.flush().unwrap();

    let open = dir
        .path()
        .join("shards")
        .join(vessel.to_string())
        .join("0/open");
    assert_eq!(fs::read_dir(open).unwrap().count(), 1_500);
}
