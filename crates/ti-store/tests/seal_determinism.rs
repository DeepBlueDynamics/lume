use std::fs;
use tempfile::tempdir;
use ti_contracts::{
    Agg, AggOp, AggPartial, BucketRecord, Catalog, CmpOp, FieldKind, FieldSpec, FieldValue,
    Predicate, ShardKey, ShardSink, ShardSource, VesselSpec,
};
use ti_store::Store;

#[test]
fn test_seal_determinism_all_field_types() {
    let dir1 = tempdir().unwrap();
    let dir2 = tempdir().unwrap();

    let build_and_seal = |dir: &std::path::Path| {
        let mut store = Store::open_or_create(dir, 10).unwrap();

        // Register vessel
        let v0 = store
            .catalog()
            .register_vessel(&VesselSpec {
                urn: "vessels.urn:mrn:signalk:uuid:boat-alpha".into(),
                name: Some("Alpha".into()),
                mmsi: Some("367000000".into()),
            })
            .unwrap();

        // 1. Presence field
        let f_pres = store
            .catalog()
            .register_field(&FieldSpec {
                id: 0,
                path: "propulsion.main.online".into(),
                agg: None,
                kind: FieldKind::Presence,
                units: None,
            })
            .unwrap();

        // 2. Ordinary Set field (single-valued)
        let f_state = store
            .catalog()
            .register_field(&FieldSpec {
                id: 1,
                path: "navigation.state".into(),
                agg: None,
                kind: FieldKind::Set,
                units: None,
            })
            .unwrap();
        let row_anchored = store
            .catalog()
            .register_set_value(f_state, "anchored")
            .unwrap();
        let row_sailing = store
            .catalog()
            .register_set_value(f_state, "sailing")
            .unwrap();
        let row_motoring = store
            .catalog()
            .register_set_value(f_state, "motoring")
            .unwrap();

        // 3. Multi-valued Source Set field
        let f_sources = store
            .catalog()
            .register_field(&FieldSpec {
                id: 2,
                path: "navigation.speedOverGround$source".into(),
                agg: None,
                kind: FieldKind::Set,
                units: None,
            })
            .unwrap();
        let row_n2k = store
            .catalog()
            .register_set_value(f_sources, "n2k.115")
            .unwrap();
        let row_gps = store
            .catalog()
            .register_set_value(f_sources, "gps.0")
            .unwrap();

        // 4. BSI field (signed numeric)
        let f_speed = store
            .catalog()
            .register_field(&FieldSpec {
                id: 3,
                path: "navigation.speedOverGround".into(),
                agg: Some(Agg::Mean),
                kind: FieldKind::Bsi { scale: 3 },
                units: Some("m/s".into()),
            })
            .unwrap();

        // 5. Count field (unsigned counter)
        let f_pump = store
            .catalog()
            .register_field(&FieldSpec {
                id: 4,
                path: "sensors.bilge.pumpCycles".into(),
                agg: Some(Agg::Edges),
                kind: FieldKind::Count,
                units: None,
            })
            .unwrap();

        // 6. Geo field
        let f_geo = store
            .catalog()
            .register_field(&FieldSpec {
                id: 5,
                path: "navigation.position".into(),
                agg: None,
                kind: FieldKind::Geo { res: 7 },
                units: None,
            })
            .unwrap();

        // Ingest records across several buckets
        let records = vec![
            // Bucket 10
            BucketRecord {
                vessel: v0,
                bucket: 10,
                field: f_pres,
                value: FieldValue::Present,
                rewrite: false,
            },
            BucketRecord {
                vessel: v0,
                bucket: 10,
                field: f_state,
                value: FieldValue::SetValue(row_anchored),
                rewrite: false,
            },
            BucketRecord {
                vessel: v0,
                bucket: 10,
                field: f_sources,
                value: FieldValue::SetValue(row_n2k),
                rewrite: false,
            },
            BucketRecord {
                vessel: v0,
                bucket: 10,
                field: f_sources,
                value: FieldValue::SetValue(row_gps),
                rewrite: false,
            },
            BucketRecord {
                vessel: v0,
                bucket: 10,
                field: f_speed,
                value: FieldValue::Int(0),
                rewrite: false,
            },
            BucketRecord {
                vessel: v0,
                bucket: 10,
                field: f_pump,
                value: FieldValue::Int(3),
                rewrite: false,
            },
            BucketRecord {
                vessel: v0,
                bucket: 10,
                field: f_geo,
                value: FieldValue::Cells(vec![0x882681a339fffff, 0x882681a339ffffe]),
                rewrite: false,
            },
            // Bucket 20
            BucketRecord {
                vessel: v0,
                bucket: 20,
                field: f_pres,
                value: FieldValue::Present,
                rewrite: false,
            },
            BucketRecord {
                vessel: v0,
                bucket: 20,
                field: f_state,
                value: FieldValue::SetValue(row_sailing),
                rewrite: false,
            },
            BucketRecord {
                vessel: v0,
                bucket: 20,
                field: f_speed,
                value: FieldValue::Int(5250),
                rewrite: false,
            },
            BucketRecord {
                vessel: v0,
                bucket: 20,
                field: f_pump,
                value: FieldValue::Int(0),
                rewrite: false,
            },
            // Bucket 30
            BucketRecord {
                vessel: v0,
                bucket: 30,
                field: f_pres,
                value: FieldValue::Present,
                rewrite: false,
            },
            BucketRecord {
                vessel: v0,
                bucket: 30,
                field: f_state,
                value: FieldValue::SetValue(row_motoring),
                rewrite: false,
            },
            BucketRecord {
                vessel: v0,
                bucket: 30,
                field: f_speed,
                value: FieldValue::Int(7100),
                rewrite: false,
            },
            // Bucket 20 rewrite: change state to motoring and speed to 6000
            BucketRecord {
                vessel: v0,
                bucket: 20,
                field: f_state,
                value: FieldValue::SetValue(row_motoring),
                rewrite: true,
            },
            BucketRecord {
                vessel: v0,
                bucket: 20,
                field: f_speed,
                value: FieldValue::Int(6000),
                rewrite: true,
            },
        ];

        // One apply is one transaction (spec/14): a (bucket, field) group must agree on
        // rewrite, so the bucket-20 rewrite is applied as its own transaction, as ingest does.
        let (initial, rewrites) = records.split_at(records.len() - 2);
        store.apply(initial).unwrap();
        store.apply(rewrites).unwrap();

        let shard_key = ShardKey {
            vessel: v0,
            shard: 0,
        };

        // Seal the shard
        let entry = store.seal(shard_key).unwrap();

        (entry, store)
    };

    let (entry1, store1) = build_and_seal(dir1.path());
    let (entry2, store2) = build_and_seal(dir2.path());

    // 1. Manifest entries must be completely equal
    assert_eq!(entry1, entry2);
    assert_eq!(entry1.hash, entry2.hash);
    assert_eq!(entry1.bytes, entry2.bytes);

    // 2. All .rbm files in shards/0/0/v1 must be byte-for-byte identical
    let v1_dir1 = dir1.path().join("shards/0/0/v1");
    let v1_dir2 = dir2.path().join("shards/0/0/v1");

    let mut entries1: Vec<_> = fs::read_dir(&v1_dir1)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    let mut entries2: Vec<_> = fs::read_dir(&v1_dir2)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();

    entries1.sort();
    entries2.sort();
    assert_eq!(entries1, entries2);
    assert!(!entries1.is_empty());

    for name in &entries1 {
        let f1 = v1_dir1.join(name);
        let f2 = v1_dir2.join(name);
        let bytes1 = fs::read(&f1).unwrap();
        let bytes2 = fs::read(&f2).unwrap();
        assert_eq!(
            bytes1, bytes2,
            "File {:?} differs between independent runs!",
            name
        );
    }

    // 3. Query results from both sealed stores must be identical
    let shard_key = ShardKey {
        vessel: 0,
        shard: 0,
    };

    // Test BSI range predicate
    let pred_speed = Predicate::BsiCmp {
        field: 3,
        op: CmpOp::Gt,
        lo: 5000,
        hi: None,
    };
    let hits1 = store1.eval(shard_key, &pred_speed).unwrap();
    let hits2 = store2.eval(shard_key, &pred_speed).unwrap();
    assert_eq!(hits1, hits2);
    assert_eq!(hits1.iter().collect::<Vec<_>>(), vec![20, 30]);

    // Test Set equality predicate: state = motoring
    let pred_state = Predicate::SetEq {
        field: 1,
        rows: vec![2], // motoring
        negate: false,
    };
    let state_hits1 = store1.eval(shard_key, &pred_state).unwrap();
    let state_hits2 = store2.eval(shard_key, &pred_state).unwrap();
    assert_eq!(state_hits1, state_hits2);
    assert_eq!(state_hits1.iter().collect::<Vec<_>>(), vec![20, 30]);

    // Test aggregation: sum of speed
    let agg1 = store1.agg(shard_key, &hits1, 3, AggOp::Sum).unwrap();
    let agg2 = store2.agg(shard_key, &hits2, 3, AggOp::Sum).unwrap();
    assert_eq!(agg1, agg2);
    assert_eq!(
        agg1,
        AggPartial::Sum {
            sum: 13100, // 6000 + 7100
            count: 2
        }
    );

    // Test Arrow RecordBatch materialization
    let batch1 = store1.read(shard_key, &hits1, &[1, 3]).unwrap();
    let batch2 = store2.read(shard_key, &hits2, &[1, 3]).unwrap();
    assert_eq!(batch1.num_rows(), batch2.num_rows());
    assert_eq!(batch1.num_columns(), batch2.num_columns());
    assert_eq!(batch1.schema(), batch2.schema());
}
