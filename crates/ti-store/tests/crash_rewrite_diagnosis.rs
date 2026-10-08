//! Deterministic flush/WAL overlap regression.
use ti_contracts::{
    Agg, BucketRecord, Catalog, CmpOp, FieldKind, FieldSpec, FieldValue, Predicate, ShardKey,
    ShardSink, ShardSource, VesselSpec,
};
use ti_store::Store;

#[test]
fn flushed_rewrite_skips_covered_old_insert() {
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
        let recovered = Store::open_or_create(dir.path(), 10).unwrap();
        let rows = recovered
            .eval(
                ShardKey { vessel, shard: 0 },
                &Predicate::BsiCmp {
                    field,
                    op: CmpOp::Eq,
                    lo: 107,
                    hi: None,
                },
            )
            .unwrap();
        assert_eq!(rows.iter().collect::<Vec<_>>(), vec![3]);
    }
}
