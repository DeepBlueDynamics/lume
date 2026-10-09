use tempfile::tempdir;
use ti_contracts::{
    Agg, BucketRecord, Catalog, FieldKind, FieldSpec, FieldValue, ShardKey, ShardSink, ShardSource,
    StoreConfig, TiConfig, VesselSpec, EPOCH,
};
use ti_store::{Store, StoreSet};

#[test]
fn test_store_retention_enforcement() {
    let tmp = tempdir().unwrap();
    let mut store = Store::open_or_create(tmp.path(), 10).unwrap();

    let v0 = store
        .catalog()
        .register_vessel(&VesselSpec {
            urn: "vessels.urn:mrn:signalk:uuid:boat-test-1".into(),
            name: Some("Test Boat".into()),
            mmsi: None,
        })
        .unwrap();

    let f0 = store
        .catalog()
        .register_field(&FieldSpec {
            id: 0,
            path: "navigation.speedOverGround".into(),
            agg: Some(Agg::Last),
            kind: FieldKind::Bsi { scale: 3 },
            units: Some("m/s".into()),
        })
        .unwrap();

    // Shard 0: bucket 0
    let key0 = ShardKey {
        vessel: v0,
        shard: 0,
    };
    store
        .apply(&[BucketRecord {
            vessel: v0,
            bucket: 0,
            field: f0,
            value: FieldValue::Int(5000),
            rewrite: false,
        }])
        .unwrap();
    store.seal(key0).unwrap();

    // Shard 1: bucket 65536
    let key1 = ShardKey {
        vessel: v0,
        shard: 1,
    };
    store
        .apply(&[BucketRecord {
            vessel: v0,
            bucket: 65536,
            field: f0,
            value: FieldValue::Int(6000),
            rewrite: false,
        }])
        .unwrap();
    store.seal(key1).unwrap();

    // Both shards exist in manifest
    assert!(store.manifest().get(key0).is_some());
    assert!(store.manifest().get(key1).is_some());

    let shard0_dir = tmp.path().join("shards").join(v0.to_string()).join("0");
    let shard1_dir = tmp.path().join("shards").join(v0.to_string()).join("1");
    assert!(shard0_dir.exists());
    assert!(shard1_dir.exists());

    // Shard 0 end_ts = EPOCH + 65536 * 10 = EPOCH + 655360.
    // Let now_timestamp = EPOCH + 655360 + 1000.
    // Retention = 500s. Cutoff = now_timestamp - 500 = EPOCH + 655360 + 500.
    // Since shard 0 end_ts (EPOCH + 655360) <= cutoff, shard 0 is dropped!
    // Shard 1 end_ts = EPOCH + 131072 * 10 > cutoff, so shard 1 is retained.
    let now_ts = EPOCH + 655360 + 1000;
    let dropped = store.enforce_retention(now_ts, 500).unwrap();
    assert_eq!(dropped, 1, "Exactly 1 shard should be dropped");

    // Shard 0 must be removed from manifest and disk
    assert!(
        store.manifest().get(key0).is_none(),
        "Shard 0 must be gone from manifest"
    );
    assert!(
        !shard0_dir.exists(),
        "Shard 0 dir must be deleted from disk"
    );

    // Shard 1 must still exist and be readable
    assert!(
        store.manifest().get(key1).is_some(),
        "Shard 1 must remain in manifest"
    );
    assert!(shard1_dir.exists(), "Shard 1 dir must remain on disk");
    let active_shards = store.shards(None, 0, 131072);
    assert_eq!(active_shards, vec![key1]);
}

#[test]
fn test_store_set_multi_store_retention() {
    let tmp = tempdir().unwrap();
    let mut config = TiConfig {
        store_root: tmp.path().to_string_lossy().to_string(),
        ..Default::default()
    };

    let mut stores = std::collections::BTreeMap::new();
    stores.insert(
        "default".to_string(),
        StoreConfig {
            width: "10s".into(),
            retention: "730d".into(),
            shore_retention: None,
            root: None,
            paths: None,
            aggs: std::collections::BTreeMap::new(),
        },
    );
    stores.insert(
        "hr".to_string(),
        StoreConfig {
            width: "1s".into(),
            retention: "90d".into(),
            shore_retention: None,
            root: None,
            paths: Some(vec!["navigation.*".into()]),
            aggs: std::collections::BTreeMap::new(),
        },
    );
    config.stores = stores;

    let mut store_set = StoreSet::open_or_create(&config).unwrap();
    assert!(store_set.store("default").is_some());
    assert!(store_set.store("hr").is_some());

    // Register vessel in both stores
    let v_default = store_set
        .store_mut("default")
        .unwrap()
        .catalog()
        .register_vessel(&VesselSpec {
            urn: "vessels.urn:mrn:signalk:uuid:boat-multi".into(),
            name: None,
            mmsi: None,
        })
        .unwrap();
    let v_hr = store_set
        .store_mut("hr")
        .unwrap()
        .catalog()
        .register_vessel(&VesselSpec {
            urn: "vessels.urn:mrn:signalk:uuid:boat-multi".into(),
            name: None,
            mmsi: None,
        })
        .unwrap();

    let f_default = store_set
        .store_mut("default")
        .unwrap()
        .catalog()
        .register_field(&FieldSpec {
            id: 0,
            path: "navigation.speedOverGround".into(),
            agg: Some(Agg::Last),
            kind: FieldKind::Bsi { scale: 3 },
            units: None,
        })
        .unwrap();
    let f_hr = store_set
        .store_mut("hr")
        .unwrap()
        .catalog()
        .register_field(&FieldSpec {
            id: 0,
            path: "navigation.speedOverGround".into(),
            agg: Some(Agg::Last),
            kind: FieldKind::Bsi { scale: 3 },
            units: None,
        })
        .unwrap();

    // Write a shard 0 to default (width 10s -> shard 0 covers 655360s = 7.58 days)
    let k_default0 = ShardKey {
        vessel: v_default,
        shard: 0,
    };
    store_set
        .store_mut("default")
        .unwrap()
        .apply(&[BucketRecord {
            vessel: v_default,
            bucket: 0,
            field: f_default,
            value: FieldValue::Int(1000),
            rewrite: false,
        }])
        .unwrap();
    store_set
        .store_mut("default")
        .unwrap()
        .seal(k_default0)
        .unwrap();

    // Write a shard 0 to hr (width 1s -> shard 0 covers 65536s = 18.2 hours)
    let k_hr0 = ShardKey {
        vessel: v_hr,
        shard: 0,
    };
    store_set
        .store_mut("hr")
        .unwrap()
        .apply(&[BucketRecord {
            vessel: v_hr,
            bucket: 0,
            field: f_hr,
            value: FieldValue::Int(1000),
            rewrite: false,
        }])
        .unwrap();
    store_set.store_mut("hr").unwrap().seal(k_hr0).unwrap();

    // At day 100 (now_ts = EPOCH + 100 * 86400):
    // For HR store: retention is 90d (90 * 86400). Shard 0 (ended at 65536s ~ 0.75d) is older than 90d -> DROPPED!
    // For default store: retention is 730d. Shard 0 (ended at 655360s ~ 7.58d) is younger than 730d -> RETAINED!
    let now_ts = EPOCH + 100 * 86400;
    let dropped_map = store_set.enforce_retention(now_ts, &config).unwrap();
    assert_eq!(
        dropped_map.get("hr"),
        Some(&1),
        "HR store should drop shard 0"
    );
    assert_eq!(
        dropped_map.get("default"),
        Some(&0),
        "Default store should retain shard 0"
    );

    assert!(store_set
        .store("hr")
        .unwrap()
        .manifest()
        .get(k_hr0)
        .is_none());
    assert!(store_set
        .store("default")
        .unwrap()
        .manifest()
        .get(k_default0)
        .is_some());
}

#[test]
fn test_retention_only_drops_sealed_shards_and_reopen() {
    let tmp = tempdir().unwrap();
    let mut store = Store::open_or_create(tmp.path(), 10).unwrap();

    let v0 = store
        .catalog()
        .register_vessel(&VesselSpec {
            urn: "vessels.urn:mrn:signalk:uuid:boat-test-wal".into(),
            name: Some("WAL Boat".into()),
            mmsi: None,
        })
        .unwrap();

    let f0 = store
        .catalog()
        .register_field(&FieldSpec {
            id: 0,
            path: "navigation.speedOverGround".into(),
            agg: Some(Agg::Last),
            kind: FieldKind::Bsi { scale: 3 },
            units: Some("m/s".into()),
        })
        .unwrap();

    // Shard 0: bucket 0 (timestamp 0). Apply and leave OPEN.
    let key0 = ShardKey {
        vessel: v0,
        shard: 0,
    };
    store
        .apply(&[BucketRecord {
            vessel: v0,
            bucket: 0,
            field: f0,
            value: FieldValue::Int(1234),
            rewrite: false,
        }])
        .unwrap();

    // Shard 1: bucket 65536. Apply and SEAL.
    let key1 = ShardKey {
        vessel: v0,
        shard: 1,
    };
    store
        .apply(&[BucketRecord {
            vessel: v0,
            bucket: 65536,
            field: f0,
            value: FieldValue::Int(5678),
            rewrite: false,
        }])
        .unwrap();
    store.seal(key1).unwrap();

    // Shard 0 is in open_shards, not manifest.
    assert!(store.manifest().get(key0).is_none());
    assert!(store.manifest().get(key1).is_some());

    // Both shards have timestamps older than cutoff.
    let now_ts = EPOCH + 2_000_000;
    let dropped = store.enforce_retention(now_ts, 500).unwrap();
    // Only sealed shard (shard 1) must be dropped!
    assert_eq!(dropped, 1, "Only sealed shard 1 should be dropped");
    assert!(
        store.manifest().get(key1).is_none(),
        "Sealed shard 1 was dropped"
    );

    // Open shard 0 must NOT be dropped:
    let active = store.shards(None, 0, 100);
    assert_eq!(active, vec![key0], "Open shard 0 must remain active");

    // Drop store and reopen to test WAL replay behavior
    drop(store);

    let store2 = Store::open_or_create(tmp.path(), 10).unwrap();
    // Shard 0 must still exist from WAL replay / open shard recovery:
    let active2 = store2.shards(None, 0, 100);
    assert_eq!(active2, vec![key0], "Open shard 0 must survive reopen");
    // Shard 1 must NOT be resurrected by WAL replay:
    assert!(
        store2.manifest().get(key1).is_none(),
        "Dropped sealed shard 1 must not resurrect"
    );
    let matching = store2.eval(key0, &ti_contracts::Predicate::All).unwrap();
    assert_eq!(matching.iter().collect::<Vec<_>>(), vec![0]);
}
