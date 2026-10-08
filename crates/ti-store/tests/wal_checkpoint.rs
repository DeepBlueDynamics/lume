use std::path::Path;
use ti_contracts::{
    Agg, BucketRecord, Catalog, CmpOp, FieldKind, FieldSpec, FieldValue, Predicate, ShardKey,
    ShardSink, ShardSource, VesselSpec,
};
use ti_store::{OpenShard, Store, Wal};

const KEY: ShardKey = ShardKey {
    vessel: 0,
    shard: 0,
};
const URN: &str = "vessels.urn:checkpoint";
fn setup(root: &Path) -> Store {
    let store = Store::open_or_create(root, 10).unwrap();
    store
        .catalog()
        .register_vessel(&VesselSpec {
            urn: URN.into(),
            name: None,
            mmsi: None,
        })
        .unwrap();
    for (id, path, kind) in [
        (0, "numeric", FieldKind::Bsi { scale: 3 }),
        (1, "count", FieldKind::Count),
        (2, "state", FieldKind::Set),
        (3, "numeric$source", FieldKind::Set),
        (4, "other", FieldKind::Bsi { scale: 3 }),
        (5, "position", FieldKind::Geo { res: 9 }),
    ] {
        assert_eq!(
            store
                .catalog()
                .register_field(&FieldSpec {
                    id,
                    path: path.into(),
                    agg: Some(Agg::Mean),
                    kind,
                    units: None,
                })
                .unwrap(),
            id
        );
    }
    for field in [2, 3] {
        store.catalog().register_set_value(field, "a").unwrap();
        store.catalog().register_set_value(field, "b").unwrap();
    }
    store
}
fn record(field: u32, value: FieldValue, rewrite: bool) -> BucketRecord {
    BucketRecord {
        vessel: 0,
        bucket: 3,
        field,
        value,
        rewrite,
    }
}
fn numeric(field: u32, value: i64, rewrite: bool) -> BucketRecord {
    record(field, FieldValue::Int(value), rewrite)
}
fn assert_numeric(store: &Store, field: u32, value: i64) {
    assert_eq!(
        store
            .eval(
                KEY,
                &Predicate::BsiCmp {
                    field,
                    op: CmpOp::Eq,
                    lo: value,
                    hi: None,
                }
            )
            .unwrap()
            .iter()
            .collect::<Vec<_>>(),
        vec![3]
    );
}
fn strip_checkpoint(root: &Path, field: u32) {
    let path = root.join(format!("shards/0/0/open/{field}.rbm"));
    let mut bytes = std::fs::read(&path).unwrap();
    assert_eq!(&bytes[bytes.len() - 20..bytes.len() - 12], b"LUMECP01");
    bytes.truncate(bytes.len() - 20);
    std::fs::write(path, bytes).unwrap();
}

#[test]
fn checkpoint_overlap_retains_clear_count_string_and_sources() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = setup(dir.path());
    store
        .apply(&[
            numeric(0, 30, false),
            numeric(1, 2, false),
            record(2, FieldValue::SetValue(0), false),
            record(3, FieldValue::SetValue(0), false),
            record(3, FieldValue::SetValue(1), false),
        ])
        .unwrap();
    store
        .apply(&[
            record(0, FieldValue::Clear, true),
            record(1, FieldValue::Clear, true),
            record(2, FieldValue::Clear, true),
            record(3, FieldValue::Clear, true),
        ])
        .unwrap();
    store
        .apply(&[
            numeric(0, 107, false),
            numeric(1, 8, false),
            record(2, FieldValue::SetValue(1), false),
            record(3, FieldValue::SetValue(1), false),
        ])
        .unwrap();
    store.flush_shards().unwrap();
    drop(store);
    let mut recovered = Store::open_or_create(dir.path(), 10).unwrap();
    assert_numeric(&recovered, 0, 107);
    assert_numeric(&recovered, 1, 8);
    assert!(recovered
        .eval(
            KEY,
            &Predicate::SetEq {
                field: 2,
                rows: vec![0],
                negate: false
            }
        )
        .unwrap()
        .is_empty());
    assert_eq!(
        recovered
            .eval(
                KEY,
                &Predicate::SetEq {
                    field: 2,
                    rows: vec![1],
                    negate: false
                }
            )
            .unwrap()
            .len(),
        1
    );
    assert!(recovered
        .eval(
            KEY,
            &Predicate::SetEq {
                field: 3,
                rows: vec![0],
                negate: false
            }
        )
        .unwrap()
        .is_empty());
    assert_eq!(
        recovered
            .eval(
                KEY,
                &Predicate::SetEq {
                    field: 3,
                    rows: vec![1],
                    negate: false
                }
            )
            .unwrap()
            .len(),
        1
    );
    assert!(recovered
        .apply(&[numeric(0, 108, false)])
        .unwrap_err()
        .to_string()
        .contains("requires rewrite"));
}

#[test]
fn legacy_source_and_geo_replay_keep_ordered_clear_and_retention() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = setup(dir.path());
    store
        .apply(&[
            record(3, FieldValue::SetValue(0), false),
            record(3, FieldValue::SetValue(1), false),
            record(5, FieldValue::Cells(vec![11, 12]), false),
        ])
        .unwrap();
    store
        .apply(&[
            record(3, FieldValue::Clear, true),
            record(5, FieldValue::Clear, true),
        ])
        .unwrap();
    store
        .apply(&[
            record(3, FieldValue::SetValue(1), false),
            record(5, FieldValue::Cells(vec![13]), false),
        ])
        .unwrap();
    store.flush_shards().unwrap();
    drop(store);
    for field in [3, 5] {
        strip_checkpoint(dir.path(), field);
    }
    let recovered = Store::open_or_create(dir.path(), 10).unwrap();
    assert!(recovered
        .eval(
            KEY,
            &Predicate::SetEq {
                field: 3,
                rows: vec![0],
                negate: false
            }
        )
        .unwrap()
        .is_empty());
    assert_eq!(
        recovered
            .eval(
                KEY,
                &Predicate::SetEq {
                    field: 3,
                    rows: vec![1],
                    negate: false
                }
            )
            .unwrap()
            .len(),
        1
    );
    assert!(recovered
        .eval(
            KEY,
            &Predicate::GeoCover {
                field: 5,
                cells: vec![11, 12]
            }
        )
        .unwrap()
        .is_empty());
    assert_eq!(
        recovered
            .eval(
                KEY,
                &Predicate::GeoCover {
                    field: 5,
                    cells: vec![13]
                }
            )
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn partial_field_publication_replays_only_uncovered_fields() {
    for legacy in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = setup(dir.path());
        store
            .apply(&[numeric(0, 30, false), numeric(4, 40, false)])
            .unwrap();
        store.flush_shards().unwrap();
        let other = dir.path().join("shards/0/0/open/4.rbm");
        if legacy {
            strip_checkpoint(dir.path(), 4);
        }
        let old = std::fs::read(&other).unwrap();
        store
            .apply(&[numeric(0, 107, true), numeric(4, 207, true)])
            .unwrap();
        store.flush_shards().unwrap();
        // Exact durable state of a crash between the two field renames.
        std::fs::write(other, old).unwrap();
        drop(store);
        let recovered = Store::open_or_create(dir.path(), 10).unwrap();
        assert_numeric(&recovered, 0, 107);
        assert_numeric(&recovered, 4, 207);
    }
}

#[test]
fn legacy_reset_then_reinsert_and_rewrite_then_duplicate_upgrade() {
    for clear in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = setup(dir.path());
        store
            .apply(&[
                numeric(0, 30, false),
                numeric(1, 2, false),
                record(2, FieldValue::SetValue(0), false),
            ])
            .unwrap();
        store
            .apply(&[
                record(
                    0,
                    if clear {
                        FieldValue::Clear
                    } else {
                        FieldValue::Int(107)
                    },
                    true,
                ),
                record(
                    1,
                    if clear {
                        FieldValue::Clear
                    } else {
                        FieldValue::Int(8)
                    },
                    true,
                ),
                record(
                    2,
                    if clear {
                        FieldValue::Clear
                    } else {
                        FieldValue::SetValue(1)
                    },
                    true,
                ),
            ])
            .unwrap();
        store
            .apply(&[
                numeric(0, 107, false),
                numeric(1, 8, false),
                record(2, FieldValue::SetValue(1), false),
            ])
            .unwrap();
        store.flush_shards().unwrap();
        drop(store);
        for field in [0, 1, 2] {
            strip_checkpoint(dir.path(), field);
        }
        let mut recovered = Store::open_or_create(dir.path(), 10).unwrap();
        assert_numeric(&recovered, 0, 107);
        assert_numeric(&recovered, 1, 8);
        assert_eq!(
            recovered
                .eval(
                    KEY,
                    &Predicate::SetEq {
                        field: 2,
                        rows: vec![1],
                        negate: false
                    }
                )
                .unwrap()
                .len(),
            1
        );
        recovered.flush_shards().unwrap();
        assert!(recovered
            .open_shard(&KEY)
            .unwrap()
            .checkpoints
            .contains_key(&0));
        drop(recovered);
        assert_numeric(&Store::open_or_create(dir.path(), 10).unwrap(), 0, 107);
    }
}

#[test]
fn trailers_do_not_change_query_rows_or_seal_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = setup(dir.path());
    store
        .apply(&[numeric(0, 107, false), numeric(1, 8, false)])
        .unwrap();
    store.flush().unwrap();
    let with = Store::open_readonly(dir.path(), 10).unwrap();
    let columns = with.eval(KEY, &Predicate::All).unwrap();
    let expected = with.read(KEY, &columns, &[0, 1]).unwrap();
    drop(with);
    for field in [0, 1] {
        strip_checkpoint(dir.path(), field);
    }
    let without = Store::open_readonly(dir.path(), 10).unwrap();
    assert_eq!(expected, without.read(KEY, &columns, &[0, 1]).unwrap());
    // A legacy direct OpenShard uses precisely the pre-A14 sealed encoder.
    let mut legacy = OpenShard::new(KEY);
    for field in [0, 1] {
        legacy
            .register_field(store.catalog().field(field).unwrap())
            .unwrap();
    }
    legacy
        .apply(&[numeric(0, 107, false), numeric(1, 8, false)])
        .unwrap();
    let reference = tempfile::tempdir().unwrap();
    let original = legacy.seal_to(reference.path(), URN, 1, 10).unwrap();
    let sealed = store.seal(KEY).unwrap();
    assert_eq!(original.hash, sealed.hash);
    for field in [0, 1] {
        let relative = format!("shards/0/0/v1/{field}.rbm");
        assert_eq!(
            std::fs::read(reference.path().join(&relative)).unwrap(),
            std::fs::read(dir.path().join(relative)).unwrap()
        );
    }
}

#[test]
fn checkpointed_replay_never_coalesces_away_an_invalid_replacement() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = setup(dir.path());
    store.apply(&[numeric(0, 30, false)]).unwrap();
    store.flush().unwrap();
    assert!(store.apply(&[numeric(0, 40, false)]).is_err());
    // A later valid rewrite cannot hide the invalid uncovered frame.
    store.apply(&[numeric(0, 50, true)]).unwrap();
    store.shutdown().unwrap();
    drop(store);
    assert!(Store::open_or_create(dir.path(), 10)
        .err()
        .unwrap()
        .to_string()
        .contains("requires rewrite"));
}

#[test]
fn first_upgrade_and_truncation_never_reuse_sequences() {
    let dir = tempfile::tempdir().unwrap();
    let (mut wal, _) = Wal::open_or_create(dir.path(), 0, URN).unwrap();
    assert_eq!(wal.append(&[numeric(0, 1, false)]).unwrap(), 1);
    assert_eq!(wal.append(&[numeric(0, 2, true)]).unwrap(), 2);
    wal.sync().unwrap();
    drop(wal);
    assert!(!dir.path().join("0.seq").exists());
    let (mut wal, records) = Wal::open_or_create(dir.path(), 0, URN).unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(wal.append(&[numeric(0, 3, true)]).unwrap(), 3);
    wal.truncate_after_flush().unwrap();
    drop(wal);
    let (mut wal, records) = Wal::open_or_create(dir.path(), 0, URN).unwrap();
    assert!(records.is_empty());
    assert_eq!(wal.append(&[numeric(0, 4, true)]).unwrap(), 4);
    wal.sync().unwrap();
    drop(wal);
    let (_, records) = Wal::open_or_create(dir.path(), 0, URN).unwrap();
    assert_eq!(records[0].0, 4);
}

#[test]
fn failed_flush_syncs_wal_and_retry_preserves_all_fields() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = setup(dir.path());
    store
        .apply(&[numeric(0, 30, false), numeric(4, 40, false)])
        .unwrap();
    let open = dir.path().join("shards/0/0/open");
    std::fs::create_dir_all(&open).unwrap();
    let obstruction = open.join(format!("4.tmp.{}", std::process::id()));
    std::fs::create_dir(&obstruction).unwrap();
    assert!(store.flush_shards().is_err());
    assert!(store.wal(0).unwrap().is_synced());
    assert!(store.open_shard(&KEY).unwrap().dirty);
    std::fs::remove_dir(obstruction).unwrap();
    store.flush_shards().unwrap();
    drop(store);
    let recovered = Store::open_or_create(dir.path(), 10).unwrap();
    assert_numeric(&recovered, 0, 30);
    assert_numeric(&recovered, 4, 40);
}

#[test]
fn corrupt_checkpoint_and_sequence_floor_fail_explicitly() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = setup(dir.path());
    store.apply(&[numeric(0, 1, false)]).unwrap();
    store.flush().unwrap();
    drop(store);
    let field = dir.path().join("shards/0/0/open/0.rbm");
    let mut bytes = std::fs::read(&field).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    std::fs::write(&field, &bytes).unwrap();
    assert!(Store::open_or_create(dir.path(), 10)
        .err()
        .unwrap()
        .to_string()
        .contains("checkpoint"));
    assert!(Store::open_readonly(dir.path(), 10)
        .err()
        .unwrap()
        .to_string()
        .contains("checkpoint"));
    bytes[last] ^= 1;
    std::fs::write(field, bytes).unwrap();
    std::fs::write(dir.path().join("wal/0.seq"), b"broken").unwrap();
    assert!(Store::open_or_create(dir.path(), 10)
        .err()
        .unwrap()
        .to_string()
        .contains("sequence floor"));
}
