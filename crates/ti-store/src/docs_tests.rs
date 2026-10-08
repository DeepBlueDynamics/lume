use super::*;
use std::{
    fs,
    io::Write,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
const VESSEL: &str = "vessels.urn:test:documents";
fn doc(id: &str, body: &str) -> Document {
    Document {
        id: id.into(),
        vessel: VESSEL.into(),
        kind: "notes".into(),
        ts_start: 1_780_000_000,
        ts_end: None,
        title: "Note".into(),
        body: body.into(),
    }
}
fn legacy(root: &Path, documents: &[Document]) {
    fs::create_dir_all(root.join("docs")).unwrap();
    fs::write(
        root.join("docs/documents.json"),
        serde_json::to_vec(
            &documents
                .iter()
                .map(StoredDocument::from)
                .collect::<Vec<_>>(),
        )
        .unwrap(),
    )
    .unwrap();
}
#[test]
fn legacy_migration_keeps_backup_and_finishes_interrupted_publication() {
    let dir = tempfile::tempdir().unwrap();
    legacy(dir.path(), &[doc("a", "leak"), doc("b", "water")]);
    let old = fs::read(dir.path().join("docs/documents.json")).unwrap();
    let migrated = DocStore::open(dir.path()).unwrap();
    assert_eq!(migrated.len(), 2);
    assert_eq!(
        fs::read(dir.path().join("docs/documents.json.bak")).unwrap(),
        old
    );
    assert!(!dir.path().join("docs/documents.json").exists());
    // Simulate a crash after log publication but before retiring legacy JSON.
    fs::write(dir.path().join("docs/documents.json"), &old).unwrap();
    let mut reopened = DocStore::open(dir.path()).unwrap();
    assert_eq!(reopened.len(), 2);
    assert!(!dir.path().join("docs/documents.json").exists());
    reopened.upsert_all([doc("a", "fixed")]).unwrap();
    assert_eq!(
        DocStore::open(dir.path())
            .unwrap()
            .iter()
            .next()
            .unwrap()
            .body,
        "fixed"
    );
    assert_eq!(
        fs::read(dir.path().join("docs/documents.json.bak")).unwrap(),
        old
    );
}
#[test]
fn interrupted_migration_before_log_and_conflicting_backup_are_safe() {
    let dir = tempfile::tempdir().unwrap();
    legacy(dir.path(), &[doc("a", "old")]);
    let path = dir.path().join("docs/documents.json");
    fs::copy(&path, dir.path().join("docs/documents.json.bak")).unwrap();
    fs::write(dir.path().join("docs/documents.tmp.orphan"), b"partial").unwrap();
    assert_eq!(DocStore::open(dir.path()).unwrap().len(), 1);
    let conflict = tempfile::tempdir().unwrap();
    legacy(conflict.path(), &[doc("a", "old")]);
    fs::write(conflict.path().join("docs/documents.json.bak"), b"preserve").unwrap();
    assert!(DocStore::open(conflict.path()).is_err());
    assert_eq!(
        fs::read(conflict.path().join("docs/documents.json.bak")).unwrap(),
        b"preserve"
    );
    assert!(!conflict.path().join("docs/documents.log").exists());
}
#[test]
fn torn_transaction_is_truncated_without_partial_batch_and_can_resume() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = DocStore::open(dir.path()).unwrap();
    store.upsert_all([doc("a", "old")]).unwrap();
    let path = dir.path().join("docs/documents.log");
    let committed = fs::read(&path).unwrap();
    store
        .upsert_all([doc("a", "new"), doc("b", "new")])
        .unwrap();
    let complete = fs::read(&path).unwrap();
    for cut in [1, 11, (complete.len() - committed.len()) / 2] {
        fs::write(&path, &complete[..committed.len() + cut]).unwrap();
        let mut reopened = DocStore::open(dir.path()).unwrap();
        assert_eq!(reopened.len(), 1);
        assert_eq!(reopened.iter().next().unwrap().body, "old");
        assert_eq!(fs::metadata(&path).unwrap().len(), committed.len() as u64);
        reopened.upsert_all([doc("c", "after recovery")]).unwrap();
        assert_eq!(DocStore::open(dir.path()).unwrap().len(), 2);
    }
}
#[test]
fn earlier_corruption_fails_and_unknown_version_is_guarded() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = DocStore::open(dir.path()).unwrap();
    store.upsert_all([doc("a", "one")]).unwrap();
    store.upsert_all([doc("b", "two")]).unwrap();
    let path = dir.path().join("docs/documents.log");
    let mut bytes = fs::read(&path).unwrap();
    bytes[24 + 12] ^= 1;
    fs::write(&path, &bytes).unwrap();
    assert!(DocStore::open(dir.path()).is_err());
    assert_eq!(fs::read(&path).unwrap(), bytes);
    bytes[8..12].copy_from_slice(&2u32.to_le_bytes());
    let crc = crc32fast::hash(&bytes[..20]);
    bytes[20..24].copy_from_slice(&crc.to_le_bytes());
    fs::write(&path, &bytes).unwrap();
    let error = DocStore::open(dir.path()).err().unwrap().to_string();
    assert!(error.contains("version 2"));
    assert!(error.contains("downgrade"));
}
#[test]
fn separate_writers_merge_and_readers_refresh_after_compaction() {
    let dir = tempfile::tempdir().unwrap();
    let mut reader = DocStore::open(dir.path()).unwrap();
    let mut a = DocStore::open(dir.path()).unwrap();
    let mut b = DocStore::open(dir.path()).unwrap();
    a.upsert_all([doc("a", "one")]).unwrap();
    b.upsert_all([doc("b", "two")]).unwrap();
    assert!(reader.refresh().unwrap());
    assert_eq!(reader.len(), 2);
    let version = reader.version();
    assert!(!reader.refresh().unwrap());
    assert_eq!(reader.version(), version);
    for n in 0..5 {
        a.upsert_all([doc("a", &format!("changed {n}"))]).unwrap();
    }
    assert!(reader.refresh().unwrap());
    assert_eq!(reader.len(), 2);
    assert_eq!(reader.iter().next().unwrap().body, "changed 4");
    b.delete(VESSEL, "a").unwrap();
    assert!(reader.refresh().unwrap());
    assert_eq!(reader.iter().next().unwrap().id, "b");
}
#[test]
fn concurrent_writers_do_not_lose_or_interleave_transactions() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let writers = (0..2)
        .map(|writer| {
            let root = root.clone();
            std::thread::spawn(move || {
                let mut store = DocStore::open(&root).unwrap();
                for n in 0..20 {
                    store
                        .upsert_all([doc(&format!("{writer}-{n}"), "written")])
                        .unwrap();
                }
            })
        })
        .collect::<Vec<_>>();
    for writer in writers {
        writer.join().unwrap();
    }
    assert_eq!(DocStore::open(&root).unwrap().len(), 40);
}
#[test]
fn invalid_batch_and_noops_never_publish_partial_state() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = DocStore::open(dir.path()).unwrap();
    store.upsert_all([doc("a", "old")]).unwrap();
    let path = dir.path().join("docs/documents.log");
    let before = fs::read(&path).unwrap();
    let version = store.version();
    store.upsert_all([doc("a", "old")]).unwrap();
    store.delete(VESSEL, "missing").unwrap();
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(store.version(), version);
    let mut invalid = doc("bad", "bad");
    invalid.ts_end = Some(invalid.ts_start);
    assert!(store.upsert_all([doc("a", "changed"), invalid]).is_err());
    assert_eq!(store.iter().next().unwrap().body, "old");
    assert_eq!(fs::read(&path).unwrap(), before);
    let mut other = doc("a", "other vessel");
    other.vessel = "agent.urn:other".into();
    store.upsert_all([other]).unwrap();
    assert_eq!(store.len(), 2);
}
#[test]
fn failed_append_or_sync_does_not_publish_optimistic_memory() {
    for failure in [1, 2] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = DocStore::open(dir.path()).unwrap();
        store.upsert_all([doc("a", "old")]).unwrap();
        let version = store.version();
        log::WRITE_FAULT.with(|fault| fault.set(failure));
        let result = store.upsert_all([doc("a", "new"), doc("b", "new")]);
        log::WRITE_FAULT.with(|fault| fault.set(0));
        assert!(result.is_err());
        assert_eq!(store.version(), version);
        assert_eq!(store.len(), 1);
        assert_eq!(store.iter().next().unwrap().body, "old");
        let reopened = DocStore::open(dir.path()).unwrap();
        if failure == 1 {
            assert_eq!(reopened.len(), 1);
            assert_eq!(reopened.iter().next().unwrap().body, "old");
        } else {
            // A complete but unacknowledged frame may survive failed fsync.
            assert_eq!(reopened.len(), 2);
        }
        store.upsert_all([doc("c", "recovered")]).unwrap();
        assert_eq!(
            DocStore::open(dir.path()).unwrap().len(),
            if failure == 1 { 2 } else { 3 }
        );
    }
}

#[test]
fn compaction_child() {
    let Some(root) = std::env::var_os("LUME_DOCSTORE_CRASH_ROOT") else {
        return;
    };
    let mut store = DocStore::open(Path::new(&root)).unwrap();
    store.upsert_all([doc("a", "after")]).unwrap();
    panic!("compaction checkpoint was not reached");
}
#[test]
fn killing_compaction_preserves_committed_documents_at_each_phase() {
    for phase in ["partial_temp", "synced_temp", "renamed"] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = DocStore::open(dir.path()).unwrap();
        store
            .upsert_all([doc("a", "initial"), doc("b", "keep")])
            .unwrap();
        store.upsert_all([doc("a", "before1")]).unwrap();
        store.upsert_all([doc("a", "before2")]).unwrap();
        drop(store);
        let marker = dir.path().join("checkpoint");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "docs::append_tests::compaction_child",
                "--nocapture",
            ])
            .env("LUME_DOCSTORE_CRASH_ROOT", dir.path())
            .env("LUME_DOCSTORE_CRASH_PHASE", phase)
            .env("LUME_DOCSTORE_CRASH_MARKER", &marker)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !marker.exists() {
            if child.try_wait().unwrap().is_some() || Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("child did not reach {phase}");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        child.kill().unwrap();
        child.wait().unwrap();
        let mut reopened = DocStore::open(dir.path()).unwrap();
        assert_eq!(reopened.len(), 2);
        assert_eq!(reopened.iter().next().unwrap().body, "after");
        assert_eq!(reopened.iter().last().unwrap().body, "keep");
        reopened.upsert_all([doc("c", "still writable")]).unwrap();
        assert_eq!(DocStore::open(dir.path()).unwrap().len(), 3);
    }
}
#[test]
fn implausible_tail_length_is_recovered_before_allocation() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = DocStore::open(dir.path()).unwrap();
    store.upsert_all([doc("a", "keep")]).unwrap();
    let path = dir.path().join("docs/documents.log");
    let length = fs::metadata(&path).unwrap().len();
    let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
    let mut prefix = u64::MAX.to_le_bytes().to_vec();
    prefix.extend_from_slice(&[0; 4]);
    prefix.extend_from_slice(&crc32fast::hash(&prefix).to_le_bytes());
    file.write_all(&prefix).unwrap();
    file.sync_all().unwrap();
    drop(file);
    assert_eq!(DocStore::open(dir.path()).unwrap().len(), 1);
    assert_eq!(fs::metadata(path).unwrap().len(), length);
}
