use arrow_array::{Array, ListArray, StringArray};
use std::path::Path;
use ti_contracts::{
    BucketRecord, Catalog, FieldKind, FieldSpec, FieldValue, ShardKey, ShardSink, ShardSource,
    VesselSpec,
};
use ti_store::Store;

#[test]
fn canonical_source_and_geo_lists_use_frozen_item_nullability() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("test-output")
        .join(format!("read-lists-{}", std::process::id()));
    let mut store = Store::open_or_create(&root, 1).unwrap();
    let vessel = store
        .catalog()
        .register_vessel(&VesselSpec {
            urn: "vessels.urn:test:lists".into(),
            name: None,
            mmsi: None,
        })
        .unwrap();
    let source = store
        .catalog()
        .register_field(&FieldSpec {
            id: 0,
            path: "speed$source".into(),
            agg: None,
            kind: FieldKind::Set,
            units: None,
        })
        .unwrap();
    let geo = store
        .catalog()
        .register_field(&FieldSpec {
            id: 0,
            path: "position".into(),
            agg: None,
            kind: FieldKind::Geo { res: 8 },
            units: None,
        })
        .unwrap();
    let row = store.catalog().register_set_value(source, "gps").unwrap();
    let row2 = store.catalog().register_set_value(source, "ais").unwrap();
    let present = store
        .catalog()
        .register_field(&FieldSpec {
            id: 0,
            path: "present".into(),
            agg: None,
            kind: FieldKind::Presence,
            units: None,
        })
        .unwrap();
    store
        .apply(&[
            BucketRecord {
                vessel,
                bucket: 0,
                field: source,
                value: FieldValue::SetValue(row),
                rewrite: false,
            },
            BucketRecord {
                vessel,
                bucket: 0,
                field: source,
                value: FieldValue::SetValue(row2),
                rewrite: false,
            },
            BucketRecord {
                vessel,
                bucket: 1,
                field: present,
                value: FieldValue::Present,
                rewrite: false,
            },
            BucketRecord {
                vessel,
                bucket: 0,
                field: geo,
                value: FieldValue::Cells(vec![1, 2]),
                rewrite: false,
            },
        ])
        .unwrap();
    let selected = [0, 1].into_iter().collect();
    let batch = store
        .read(ShardKey { vessel, shard: 0 }, &selected, &[source, geo])
        .unwrap();
    assert_eq!(batch.num_rows(), 2);
    let frozen = ti_contracts::telemetry_schema(&store.catalog().fields().unwrap()).unwrap();
    assert_eq!(
        batch.schema().as_ref(),
        &frozen.project(&[0, 1, 2, 3]).unwrap()
    );
    let sources = batch
        .column(2)
        .as_any()
        .downcast_ref::<ListArray>()
        .unwrap();
    let values = sources.value(0);
    let values = values.as_any().downcast_ref::<StringArray>().unwrap();
    assert_eq!(values.value(0), "ais");
    assert_eq!(values.value(1), "gps");
    assert!(sources.is_null(1));
    assert!(batch.column(3).is_null(1));
    for index in [2, 3] {
        match batch.schema().field(index).data_type() {
            ti_contracts::arrow_schema::DataType::List(item) => assert!(!item.is_nullable()),
            other => panic!("expected List, got {other:?}"),
        }
    }
    store.seal(ShardKey { vessel, shard: 0 }).unwrap();
    let sealed = store
        .read(ShardKey { vessel, shard: 0 }, &selected, &[source, geo])
        .unwrap();
    assert_eq!(batch, sealed);
    store.shutdown().unwrap();
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}
