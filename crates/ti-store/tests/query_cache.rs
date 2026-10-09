use std::sync::Arc;
use ti_contracts::{
    Agg, AggOp, AggPartial, BucketRecord, Catalog, CmpOp, FieldKind, FieldSpec, FieldValue,
    Predicate, ShardKey, ShardSink, ShardSource, VesselSpec,
};
use ti_store::Store;
const BUDGET: u64 = 64 * 1024 * 1024;

#[test]
fn preload_first_query_hits_without_full_load_and_preserves_universe() {
    let dir = tempfile::tempdir().unwrap();
    let (_writer, shard, field, _) = fixture(dir.path());
    let control = ti_store::QueryCacheControl::open(dir.path()).unwrap();
    control.set_budget(BUDGET).unwrap();
    let report = control
        .warm(
            dir.path(),
            None,
            &["navigation.speedOverGround@mean".into()],
        )
        .unwrap();
    assert_eq!((report.shards, report.fields), (1, 1));
    assert!(report.bytes > 0 && report.bytes <= BUDGET as usize);
    let reader = Store::open_readonly_with_cache(dir.path(), 10, BUDGET).unwrap();
    assert_eq!(
        reader
            .eval(shard, &predicate(field))
            .unwrap()
            .iter()
            .collect::<Vec<_>>(),
        vec![2]
    );
    assert_eq!(
        reader
            .eval(shard, &Predicate::All)
            .unwrap()
            .iter()
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    let stats = reader.query_cache_stats().unwrap();
    assert!(stats.hits > 0);
    assert_eq!((stats.full_loads, stats.field_loads), (0, 0));
}

#[test]
fn preload_budget_stops_newest_first_without_evicting_request_entries() {
    let dir = tempfile::tempdir().unwrap();
    let (mut writer, old, field, _) = fixture(dir.path());
    let newer = ShardKey {
        vessel: old.vessel,
        shard: 1,
    };
    writer
        .apply(&[BucketRecord {
            vessel: old.vessel,
            bucket: 65538,
            field,
            value: FieldValue::Int(4000),
            rewrite: false,
        }])
        .unwrap();
    writer.seal(newer).unwrap();
    let reader = Store::open_readonly_with_cache(dir.path(), 10, BUDGET).unwrap();
    let control = ti_store::QueryCacheControl::open(dir.path()).unwrap();
    control.set_budget(BUDGET).unwrap();
    reader.eval(newer, &predicate(field)).unwrap();
    let charge = control.stats().unwrap().used_bytes as u64;
    control.clear().unwrap();
    let report = control.warm(dir.path(), Some(charge), &[]).unwrap();
    assert_eq!((report.shards, report.fields), (1, 1));
    assert_eq!(report.bytes, charge as usize);
    assert!(report.stopped_at_budget);
    let before = control.stats().unwrap();
    assert_eq!(reader.eval(newer, &predicate(field)).unwrap().len(), 1);
    assert_eq!(control.stats().unwrap().full_loads, before.full_loads);
    control.set_budget(charge).unwrap();
    let report = control.warm(dir.path(), Some(BUDGET), &[]).unwrap();
    assert_eq!(report.bytes, 0);
    assert!(report.stopped_at_budget);
    assert_eq!(control.stats().unwrap().evictions, before.evictions);
    assert_eq!(reader.eval(newer, &predicate(field)).unwrap().len(), 1);
    assert_eq!(control.stats().unwrap().full_loads, before.full_loads);
}

#[test]
fn preload_configuration_defaults_and_disabled_budget_are_explicit() {
    let defaults = ti_contracts::QueryLimits::default();
    assert!(defaults.warm_on_open);
    assert_eq!(defaults.warm_budget_bytes, None);
    assert!(defaults.warm_fields.is_empty());
    let config = ti_contracts::TiConfig::from_toml(
        "[query]\nwarm_on_open=false\nwarm_budget_bytes=4096\nwarm_fields=['navigation.speedOverGround']\n"
    ).unwrap();
    assert!(!config.query.warm_on_open);
    assert_eq!(config.query.warm_budget_bytes, Some(4096));
    assert!(ti_contracts::TiConfig::from_toml("[query]\nwarm_field=[]\n").is_err());
    let dir = tempfile::tempdir().unwrap();
    let (_writer, _, _, _) = fixture(dir.path());
    let control = ti_store::QueryCacheControl::open(dir.path()).unwrap();
    control.set_budget(0).unwrap();
    let report = control.warm(dir.path(), Some(BUDGET), &[]).unwrap();
    assert_eq!((report.bytes, report.budget_bytes), (0, 0));
    assert!(report.stopped_at_budget);
    control.set_budget(BUDGET).unwrap();
    assert!(control
        .warm(dir.path(), None, &["no.such.path".into()])
        .is_err());
}
fn fixture(root: &std::path::Path) -> (Store, ShardKey, u32, u32) {
    let mut store = Store::open_or_create(root, 10).unwrap();
    let vessel = store
        .catalog()
        .register_vessel(&VesselSpec {
            urn: "vessels.urn:test:cached".into(),
            name: None,
            mmsi: None,
        })
        .unwrap();
    let field = store
        .catalog()
        .register_field(&FieldSpec {
            id: 0,
            path: "navigation.speedOverGround".into(),
            agg: Some(Agg::Mean),
            kind: FieldKind::Bsi { scale: 3 },
            units: Some("m/s".into()),
        })
        .unwrap();
    let other = store
        .catalog()
        .register_field(&FieldSpec {
            id: 0,
            path: "navigation.state".into(),
            agg: None,
            kind: FieldKind::Set,
            units: None,
        })
        .unwrap();
    let row = store
        .catalog()
        .register_set_value(other, "sailing")
        .unwrap();
    store
        .apply(&[
            BucketRecord {
                vessel,
                bucket: 1,
                field,
                value: FieldValue::Int(2000),
                rewrite: false,
            },
            BucketRecord {
                vessel,
                bucket: 2,
                field,
                value: FieldValue::Int(4000),
                rewrite: false,
            },
            // Bucket 3 belongs only to the unprojected field: universe must include it.
            BucketRecord {
                vessel,
                bucket: 3,
                field: other,
                value: FieldValue::SetValue(row),
                rewrite: false,
            },
        ])
        .unwrap();
    let shard = ShardKey { vessel, shard: 0 };
    store.seal(shard).unwrap();
    (store, shard, field, other)
}
fn predicate(field: u32) -> Predicate {
    Predicate::BsiCmp {
        field,
        op: CmpOp::Gt,
        lo: 3000,
        hi: None,
    }
}
#[test]
fn warm_eval_read_agg_and_reopened_snapshot_do_not_reload_fields() {
    let dir = tempfile::tempdir().unwrap();
    let (_writer, shard, field, other) = fixture(dir.path());
    let reader = Store::open_readonly_with_cache(dir.path(), 10, BUDGET).unwrap();
    assert_eq!(reader.eval(shard, &Predicate::All).unwrap().len(), 3);
    assert_eq!(
        reader
            .eval(shard, &predicate(field))
            .unwrap()
            .iter()
            .collect::<Vec<_>>(),
        vec![2]
    );
    let all = reader.eval(shard, &Predicate::All).unwrap();
    let batch = reader.read(shard, &all, &[field, other]).unwrap();
    assert_eq!(batch.num_rows(), 3);
    assert_eq!(
        reader.agg(shard, &all, field, AggOp::Sum).unwrap(),
        AggPartial::Sum {
            sum: 6000,
            count: 2
        }
    );
    let before = reader.query_cache_stats().unwrap();
    assert_eq!(before.full_loads, 1);
    for _ in 0..3 {
        assert_eq!(reader.eval(shard, &predicate(field)).unwrap().len(), 1);
        assert_eq!(reader.read(shard, &all, &[field, other]).unwrap(), batch);
        assert_eq!(
            reader.agg(shard, &all, field, AggOp::CountAll).unwrap(),
            AggPartial::Count(3)
        );
    }
    let reopened = Store::open_readonly_with_cache(dir.path(), 10, BUDGET).unwrap();
    assert_eq!(reopened.read(shard, &all, &[field, other]).unwrap(), batch);
    let after = reopened.query_cache_stats().unwrap();
    assert_eq!(after.full_loads, before.full_loads);
    assert_eq!(after.field_loads, before.field_loads);
    assert!(after.hits > before.hits);
    assert!(after.used_bytes <= after.budget_bytes);
}
#[test]
fn measurement_control_clears_only_decoded_entries() {
    let dir = tempfile::tempdir().unwrap();
    let (_writer, shard, field, _) = fixture(dir.path());
    let reader = Store::open_readonly_with_cache(dir.path(), 10, BUDGET).unwrap();
    let control = ti_store::QueryCacheControl::open(dir.path()).unwrap();
    control.set_budget(BUDGET).unwrap();
    assert_eq!(reader.eval(shard, &predicate(field)).unwrap().len(), 1);
    let before = control.stats().unwrap();
    assert!(before.entries > 0);
    control.clear().unwrap();
    assert_eq!(control.stats().unwrap().entries, 0);
    assert_eq!(control.stats().unwrap().used_bytes, 0);
    assert_eq!(reader.eval(shard, &predicate(field)).unwrap().len(), 1);
    assert!(control.stats().unwrap().full_loads > before.full_loads);
    control.set_budget(0).unwrap();
    assert_eq!(
        reader
            .read(shard, &[1, 2].into_iter().collect(), &[field])
            .unwrap()
            .num_rows(),
        2
    );
    assert_eq!(control.stats().unwrap().entries, 0);
}
#[test]
fn repaired_manifest_version_never_reuses_old_values() {
    let dir = tempfile::tempdir().unwrap();
    let (mut writer, shard, field, _) = fixture(dir.path());
    let reader = Store::open_readonly_with_cache(dir.path(), 10, BUDGET).unwrap();
    assert_eq!(reader.eval(shard, &predicate(field)).unwrap().len(), 1);
    writer
        .apply(&[BucketRecord {
            vessel: shard.vessel,
            bucket: 1,
            field,
            value: FieldValue::Int(9000),
            rewrite: true,
        }])
        .unwrap();
    let repaired = writer.seal(shard).unwrap();
    assert_eq!(repaired.version, 2);
    let updated = Store::open_readonly_with_cache(dir.path(), 10, BUDGET).unwrap();
    assert_eq!(updated.eval(shard, &predicate(field)).unwrap().len(), 2);
    // Already running queries retain their v1 snapshot.
    assert_eq!(reader.eval(shard, &predicate(field)).unwrap().len(), 1);
    assert_eq!(updated.eval(shard, &predicate(field)).unwrap().len(), 2);
}
#[test]
fn cache_off_tiny_budget_and_open_shards_return_identical_rows() {
    let dir = tempfile::tempdir().unwrap();
    let (mut writer, shard, field, _) = fixture(dir.path());
    let reader = Store::open_readonly_with_cache(dir.path(), 10, 0).unwrap();
    let all = reader.eval(shard, &Predicate::All).unwrap();
    let expected = reader.read(shard, &all, &[field]).unwrap();
    for budget in [1, 4096, BUDGET] {
        let reader = Store::open_readonly_with_cache(dir.path(), 10, budget).unwrap();
        assert_eq!(reader.read(shard, &all, &[field]).unwrap(), expected);
        assert_eq!(
            reader.agg(shard, &all, field, AggOp::Sum).unwrap(),
            AggPartial::Sum {
                sum: 6000,
                count: 2
            }
        );
        let stats = reader.query_cache_stats().unwrap();
        assert!(stats.used_bytes <= stats.budget_bytes);
    }
    let loaded = writer.query_cache_stats().unwrap().full_loads;
    writer
        .apply(&[BucketRecord {
            vessel: shard.vessel,
            bucket: 1,
            field,
            value: FieldValue::Int(7000),
            rewrite: true,
        }])
        .unwrap();
    assert_eq!(writer.eval(shard, &predicate(field)).unwrap().len(), 2);
    writer
        .apply(&[BucketRecord {
            vessel: shard.vessel,
            bucket: 1,
            field,
            value: FieldValue::Int(1000),
            rewrite: true,
        }])
        .unwrap();
    assert_eq!(writer.eval(shard, &predicate(field)).unwrap().len(), 1);
    assert_eq!(writer.query_cache_stats().unwrap().full_loads, loaded);
}
#[test]
fn parallel_queries_share_immutable_results_with_bounded_charge() {
    let dir = tempfile::tempdir().unwrap();
    let (_writer, shard, field, _) = fixture(dir.path());
    let reader = Arc::new(Store::open_readonly_with_cache(dir.path(), 10, BUDGET).unwrap());
    let workers: Vec<_> = (0..8)
        .map(|_| {
            let reader = Arc::clone(&reader);
            std::thread::spawn(move || {
                for _ in 0..10 {
                    assert_eq!(reader.eval(shard, &predicate(field)).unwrap().len(), 1);
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    let stats = reader.query_cache_stats().unwrap();
    assert!(stats.used_bytes <= stats.budget_bytes);
    assert!(stats.hits > 0);
}
#[test]
fn query_config_changes_budget_without_editing_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let (_writer, _, _, _) = fixture(dir.path());
    let path = dir.path().join("ti.toml");
    let text = "width_seconds=10\n[query]\nsealed_cache_bytes=67108864\n";
    std::fs::write(&path, text).unwrap();
    let reader = Store::open_readonly(dir.path(), 10).unwrap();
    assert_eq!(
        reader.query_cache_stats().unwrap().budget_bytes,
        BUDGET as usize
    );
    assert_eq!(std::fs::read_to_string(path).unwrap(), text);
    assert!(ti_contracts::TiConfig::from_toml("[query]\nsealed_cache_byte=12\n").is_err());
    assert_eq!(
        ti_contracts::QueryLimits::default().sealed_cache_bytes,
        256 * 1024 * 1024
    );
}
