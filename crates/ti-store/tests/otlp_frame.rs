use std::path::Path;
use ti_contracts::{
    Agg, BucketRecord, Catalog, FieldKind, FieldSpec, FieldValue, ShardSink, VesselSpec,
};
use ti_store::{Store, Wal};

const URN: &str = "vessels.urn:ingest";

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
    store
        .catalog()
        .register_field(&FieldSpec {
            id: 0,
            path: "numeric".into(),
            agg: Some(Agg::Mean),
            kind: FieldKind::Bsi { scale: 3 },
            units: None,
        })
        .unwrap();
    store
}

fn record() -> BucketRecord {
    BucketRecord {
        vessel: 0,
        bucket: 3,
        field: 0,
        value: FieldValue::Int(42),
        rewrite: false,
    }
}

#[test]
fn vessel_apply_matches_legacy_append_and_writes_no_otlp_magic() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = setup(dir.path());
    let sample = record();
    ShardSink::apply(&mut store, std::slice::from_ref(&sample)).unwrap();
    let written = std::fs::read(store.wal(0).unwrap().path()).unwrap();
    assert!(store.wal(0).unwrap().has_unsynced());
    assert!(!written.windows(8).any(|window| window == b"LUMEOC01"));

    let plain_dir = tempfile::tempdir().unwrap();
    let (mut plain, _) = Wal::open_or_create(plain_dir.path(), 0, URN).unwrap();
    plain.append(&[sample]).unwrap();
    let legacy = std::fs::read(plain.path()).unwrap();
    assert_eq!(written, legacy);
}

#[test]
fn commit_otlp_syncs_one_magic_frame_and_replays_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = setup(dir.path());
    let sample = record();
    let counters = br#"{"version":1,"counters":[]}"#.to_vec();
    store
        .commit_otlp(std::slice::from_ref(&sample), &[(0, counters.clone())])
        .unwrap();
    assert!(store.wal(0).unwrap().is_synced());
    let written = std::fs::read(store.wal(0).unwrap().path()).unwrap();
    assert!(written.windows(8).any(|window| window == b"LUMEOC01"));
    drop(store);

    let store = Store::open_or_create(dir.path(), 10).unwrap();
    let frames = store.otlp_counter_frames().unwrap();
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].2, counters);
    let recovered = Wal::recover(&dir.path().join("wal").join("0.wal")).unwrap();
    assert_eq!(recovered.2.len(), 1);
    assert_eq!(recovered.2[0].1, vec![sample]);
}
