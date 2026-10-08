//! Temporary diagnosis: no production recovery behavior is changed.
use ti_contracts::{
    Agg, BucketRecord, Catalog, FieldKind, FieldSpec, FieldValue, ShardSink, VesselSpec,
};
use ti_store::Store;

#[test]
fn old_insert_in_untruncated_wal_conflicts_with_flushed_rewrite() {
    for _ in 0..20 {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open_or_create(dir.path(), 10).unwrap();
        let vessel = store
            .catalog()
            .register_vessel(&VesselSpec {
                urn: "vessels.urn:test:crash-rewrite".into(),
                name: None,
                mmsi: None,
            })
            .unwrap();
        let field = store
            .catalog()
            .register_field(&FieldSpec {
                id: 0,
                path: "navigation.speed".into(),
                agg: Some(Agg::Mean),
                kind: FieldKind::Bsi { scale: 3 },
                units: None,
            })
            .unwrap();
        store
            .apply(&[BucketRecord {
                vessel,
                bucket: 3,
                field,
                value: FieldValue::Int(30),
                rewrite: false,
            }])
            .unwrap();
        store
            .apply(&[BucketRecord {
                vessel,
                bucket: 3,
                field,
                value: FieldValue::Int(107),
                rewrite: true,
            }])
            .unwrap();
        store.flush_shards().unwrap();
        store.shutdown().unwrap();
        // Preserve the exact durable state before truncate_wals(), with both
        // the rewritten snapshot and the older insert in the complete WAL.
        drop(store);
        let error = Store::open_or_create(dir.path(), 10)
            .err()
            .expect("diagnostic should reproduce");
        assert!(
            error
                .to_string()
                .contains("numeric replacement requires rewrite"),
            "{error}"
        );
    }
}
