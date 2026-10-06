use std::{io::{BufRead, BufReader, Write}, path::{Path, PathBuf}, process::{Command, Stdio}};
use ti_contracts::{Agg, BucketRecord, Catalog, FieldKind, FieldSpec, FieldValue, Predicate, ShardKey, ShardSink, ShardSource, VesselSpec};
use ti_store::Store;
const KEY: ShardKey = ShardKey { vessel:0,shard:0 };
fn setup(root: &Path) -> (Store,u32,u32) {
    let store = Store::open_or_create(root,10).unwrap();
    store.catalog().register_vessel(&VesselSpec{urn:"vessels.urn:repair".into(),name:None,mmsi:None}).unwrap();
    let register = |path: &str| store.catalog().register_field(&FieldSpec{id:0,path:path.into(),agg:Some(Agg::Mean),
        kind:FieldKind::Bsi{scale:3},units:None}).unwrap();
    let first = register("first"); let other = register("other");
    (store,first,other)
}
fn record(bucket: u32, field: u32, value: i64) -> BucketRecord {
    BucketRecord{vessel:0,bucket,field,value:FieldValue::Int(value),rewrite:true}
}
fn scratch() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("sealed-repair-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&path).unwrap(); path
}
#[test]
fn append_and_reseal_preserve_unrelated_data_and_identical_hash() {
    let root = scratch(); let (mut store,first,other) = setup(&root);
    let records = [record(1,first,1000),record(2,other,2000)];
    store.apply(&records).unwrap();
    let original = store.seal(KEY).unwrap();
    store.apply(&records).unwrap();
    let identical = store.seal(KEY).unwrap();
    assert_eq!(original.hash,identical.hash);
    assert!(identical.version>original.version);
    store.apply(&[record(3,first,3000)]).unwrap();
    assert_eq!(store.eval(KEY,&Predicate::All).unwrap().iter().collect::<Vec<_>>(),vec![1,2,3]);
    assert_eq!(store.eval(KEY,&Predicate::Present(other)).unwrap().iter().collect::<Vec<_>>(),vec![2]);
    let repaired = store.seal(KEY).unwrap();
    assert_ne!(repaired.hash,original.hash);
    assert!(repaired.version>identical.version);
    let again = store.seal(KEY).unwrap();
    assert_eq!(again.hash,repaired.hash); // An unchanged sealed shard cannot become empty.
    drop(store);
    let reopened = Store::open_or_create(&root,10).unwrap();
    assert_eq!(reopened.eval(KEY,&Predicate::All).unwrap().len(),3);
    drop(reopened); std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn retention_cutoff_survives_restart_and_prevents_resurrection() {
    let root = scratch(); let (mut store,first,_) = setup(&root);
    store.apply(&[record(1,first,1000)]).unwrap(); store.seal(KEY).unwrap();
    assert_eq!(store.enforce_retention(ti_contracts::EPOCH+700_000,10_000).unwrap(),1);
    store.apply(&[record(1,first,2000)]).unwrap();
    assert!(store.shards(None,0,u32::MAX).is_empty());
    assert_eq!(store.dropped_late_records(),1);
    drop(store);
    let mut store = Store::open_or_create(&root,10).unwrap();
    store.apply(&[record(2,first,3000)]).unwrap();
    assert_eq!(store.dropped_late_records(),2);
    assert!(store.shards(None,0,u32::MAX).is_empty());
    drop(store); std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn kill_during_sealed_repair_recovers_wal_and_preserves_authoritative_seal() {
    if let Ok(root) = std::env::var("SEALED_REPAIR_WORKER") {
        let (mut store,first,_) = setup(Path::new(&root));
        store.apply(&[record(3,first,3000)]).unwrap();
        store.shutdown().unwrap(); // Force acknowledged WAL to disk before the intentional kill.
        match std::env::var("REPAIR_PHASE").unwrap().as_str() {
            "wal" => {},
            "open" => {store.flush().unwrap();},
            "pre-manifest" => {
                store.flush().unwrap();
                let mut staged = ti_store::OpenShard { data: store.open_shard(&KEY).unwrap().data.clone(), dirty: true, has_data: true };
                staged.seal_to(Path::new(&root),"vessels.urn:repair",2,10).unwrap();
                assert!(Path::new(&root).join("shards/0/0/open").exists());
            },
            phase => panic!("unexpected phase {phase}"),
        }
        println!("REPAIR_READY"); std::io::stdout().flush().unwrap();
        loop {std::thread::park();}
    }
    for phase in ["wal","open","pre-manifest"] {
        let root = scratch(); let (mut store,first,other) = setup(&root);
        store.apply(&[record(1,first,1000),record(2,other,2000)]).unwrap();
        let original = store.seal(KEY).unwrap(); drop(store);
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact","kill_during_sealed_repair_recovers_wal_and_preserves_authoritative_seal","--nocapture"])
            .env("SEALED_REPAIR_WORKER",&root).env("REPAIR_PHASE",phase).stdout(Stdio::piped()).spawn().unwrap();
        let mut reader = BufReader::new(child.stdout.take().unwrap());
        loop {
            let mut line=String::new();
            assert!(reader.read_line(&mut line).unwrap()>0,"worker exited before ready");
            if line.contains("REPAIR_READY"){break;}
        }
        child.kill().unwrap(); child.wait().unwrap();
        let mut recovered = Store::open_or_create(&root,10).unwrap();
        assert_eq!(recovered.manifest().get(KEY).unwrap().hash,original.hash);
        assert_eq!(recovered.eval(KEY,&Predicate::All).unwrap().iter().collect::<Vec<_>>(),vec![1,2,3]);
        assert_eq!(recovered.eval(KEY,&Predicate::Present(other)).unwrap().iter().collect::<Vec<_>>(),vec![2]);
        let repaired = recovered.seal(KEY).unwrap();
        assert_eq!(repaired.version,2);
        assert_ne!(repaired.hash,original.hash);
        drop(recovered); std::fs::remove_dir_all(root).unwrap();
    }
}
