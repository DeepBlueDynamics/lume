use lume::index_binary::{generation, overlays, snapshot};
use lume::search::{LoadedIndex, OpenEnvChecks, SearchMode, SearchOptions};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};

struct Fixture {
    root: PathBuf,
    tick: u64,
}
impl Fixture {
    fn new() -> Self {
        let root = loop {
            let root = std::env::temp_dir().join(format!("lume-incremental-{}", lume::uuid_v4()));
            match std::fs::create_dir(&root) {
                Ok(()) => break root,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("Cannot create {}: {error}", root.display()),
            }
        };
        std::fs::create_dir(root.join("docs")).unwrap();
        let mut fixture = Self { root, tick: 0 };
        fixture.write("boat.md", "---\ncategory: manual\n---\n# Pump\nBilge pump removes water. Check the hose, strainer, float switch and battery before operating.");
        fixture.write(
            "engine.txt",
            "Engine\n\nThe diesel engine drives the boat. Check engine oil and cooling water.",
        );
        fixture
    }
    fn source(&self) -> PathBuf {
        self.root.join("docs")
    }
    fn db(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }
    fn write(&mut self, name: &str, text: &str) {
        self.tick += 2;
        let path = self.source().join(name);
        std::fs::write(&path, text).unwrap();
        let modified = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000 + self.tick);
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
    }
    fn mutate(&mut self, operation: &str) {
        match operation {
            "unchanged" => {}
            "add" => self.write("anchor.txt", "Anchor\n\nThe anchor holds the boat against wind and current."),
            "change" => self.write("boat.md", "---\ncategory: safety\n---\n# Pump\nEmergency bilge pump instructions. Close the leaking seacock and run the backup pump."),
            "delete" => std::fs::remove_file(self.source().join("engine.txt")).unwrap(),
            "rename" => std::fs::rename(self.source().join("engine.txt"), self.source().join("motor.txt")).unwrap(),
            _ => panic!("unknown operation"),
        }
    }
    fn index(&self, db: &Path, binary: bool, extra: &[&str]) -> Output {
        let mut cmd = command();
        if binary {
            cmd.env("LUME_INDEX_FORMAT", "4");
        }
        cmd.arg("index")
            .arg(self.source())
            .arg("--db")
            .arg(db)
            .args(extra)
            .output()
            .unwrap()
    }
    fn parity(&self, actual_db: &Path, suffix: &str) {
        let actual = load(actual_db);
        for binary in [false, true] {
            let fresh = self.db(&format!("fresh-{suffix}-{binary}"));
            success(&self.index(&fresh, binary, &[]));
            assert_replies_equal(&actual, &load(&fresh));
            assert_meta_equal(&actual, &load(&fresh));
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn command() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lume"));
    for key in [
        "LUME_INDEX_FORMAT",
        "LUME_EMBED_MODEL",
        "LUME_EMBED_DIMENSIONS",
        "NUTS_SERVICES_TOKEN",
        "LUME_TAG_DICT",
    ] {
        cmd.env_remove(key);
    }
    cmd.env("LUME_STEM", "1").env("LUME_QUERY_INVERSION", "0");
    cmd
}
fn success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
fn load(db: &Path) -> LoadedIndex {
    LoadedIndex::open_with_checks(db, OpenEnvChecks::default()).unwrap()
}
fn replies(index: &LoadedIndex) -> Vec<serde_json::Value> {
    let mut replies = Vec::new();
    for graph_beta in [0.0, 0.4] {
        for query in [
            "bilge",
            "pump",
            "water",
            "anchor",
            "engine",
            "bilge OR anchor",
            "bilge NOT anchor",
        ] {
            let result = lume::search::search(
                index,
                query,
                &SearchOptions {
                    mode: SearchMode::LexicalOnly,
                    graph_beta,
                    limit: 100,
                    ..Default::default()
                },
            )
            .unwrap();
            replies.push(serde_json::to_value(result).unwrap());
        }
    }
    replies
}
fn assert_replies_equal(actual: &LoadedIndex, expected: &LoadedIndex) {
    assert_eq!(
        replies(actual),
        replies(expected),
        "complete serialized replies"
    );
    for query in ["bilge", "pump", "anchor", "engine"] {
        let options = SearchOptions {
            mode: SearchMode::LexicalOnly,
            ..Default::default()
        };
        let bits = |index: &LoadedIndex| {
            lume::search::search(index, query, &options)
                .unwrap()
                .hits
                .into_iter()
                .map(|hit| {
                    (
                        hit.section_index,
                        hit.score.to_bits(),
                        hit.bm25_score.to_bits(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(bits(actual), bits(expected), "exact score bits");
    }
}
fn assert_meta_equal(actual: &LoadedIndex, expected: &LoadedIndex) {
    let disk = |index: &LoadedIndex| {
        index.meta.as_ref().map(|meta| {
            let mut value = serde_json::to_value(meta.to_disk().unwrap()).unwrap();
            value["generation"] = serde_json::Value::Null;
            value
        })
    };
    assert_eq!(
        disk(actual),
        disk(expected),
        "metadata except publication UUID"
    );
}

#[test]
fn no_force_creation_and_each_source_update_match_fresh_json_and_v4() {
    let mut fixture = Fixture::new();
    let db = fixture.db("v4");
    success(&fixture.index(&db, true, &[]));
    fixture.parity(&db, "initial");
    for operation in ["unchanged", "add", "change", "rename", "delete"] {
        let old = load(&db);
        let old_replies = replies(&old);
        let previous = generation::read_manifest(&db).unwrap();
        fixture.mutate(operation);
        // Existing v4 must stay v4 even with no format environment variable.
        success(&fixture.index(&db, false, &[]));
        let current = generation::read_manifest(&db).unwrap();
        assert_ne!(previous.generation, current.generation);
        assert!(db.join("generations").join(previous.generation).exists());
        assert_eq!(
            replies(&old),
            old_replies,
            "old resident snapshot remains valid"
        );
        fixture.parity(&db, operation);
    }
    success(&fixture.index(&db, false, &["-f"]));
    fixture.parity(&db, "forced");
}

#[test]
fn index_update_restores_v4_sources_and_preserves_unchanged_frontmatter() {
    let mut fixture = Fixture::new();
    let db = fixture.db("v4");
    success(&fixture.index(&db, true, &[]));
    fixture.mutate("add");
    let updated = command()
        .args(["index", "update"])
        .arg("--db")
        .arg(&db)
        .output()
        .unwrap();
    success(&updated);
    fixture.parity(&db, "index-update");
}

#[test]
fn each_failed_update_keeps_old_generation_and_resumes_to_fresh_parity() {
    for operation in ["unchanged", "add", "change", "delete", "rename"] {
        let mut fixture = Fixture::new();
        let db = fixture.db("v4");
        success(&fixture.index(&db, true, &[]));
        let pointer = std::fs::read(db.join("index.json")).unwrap();
        let old = load(&db);
        let old_replies = replies(&old);
        fixture.mutate(operation);
        let missing = fixture.root.join("missing-vectors.jsonl");
        let failed = fixture.index(
            &db,
            false,
            &[
                "--embed-model",
                "checkpoint-test",
                "--embed-dimensions",
                "2",
                "--embed-docs",
                missing.to_str().unwrap(),
            ],
        );
        assert!(!failed.status.success(), "missing vector input must fail");
        assert_eq!(std::fs::read(db.join("index.json")).unwrap(), pointer);
        assert_eq!(replies(&old), old_replies);
        assert_eq!(replies(&load(&db)), old_replies);
        assert!(db.join("index-scan-checkpoint.json").exists());
        let resumed = fixture.index(&db, false, &[]);
        success(&resumed);
        assert!(String::from_utf8_lossy(&resumed.stdout).contains("Resuming unpublished"));
        assert!(!db.join("index-scan-checkpoint.json").exists());
        fixture.parity(&db, operation);
    }
}

#[test]
fn stale_checkpoint_cannot_override_new_v4_generation() {
    let fixture = Fixture::new();
    let db = fixture.db("v4");
    success(&fixture.index(&db, true, &[]));
    let before = load(&db);
    let checkpoint = serde_json::json!({
        "version": 1, "target_dir": fixture.source(),
        "published_manifest": {"generation": "stale"},
        "cached_files": {}, "frontmatter_by_file": {}
    });
    lume::search::save_json(&db.join("index-scan-checkpoint.json"), &checkpoint).unwrap();
    let output = fixture.index(&db, false, &[]);
    success(&output);
    assert!(String::from_utf8_lossy(&output.stderr).contains("Ignoring stale source"));
    assert_replies_equal(&load(&db), &before);
}

#[test]
fn v4_changed_text_drops_stale_entities_intentionally_unlike_json_title_line_reuse() {
    let mut fixture = Fixture::new();
    let db = fixture.db("v4");
    success(&fixture.index(&db, true, &[]));
    let initial = load(&db);
    let replacements = initial
        .bm25
        .sections
        .iter()
        .enumerate()
        .map(|(section, text)| overlays::Replacement {
            section: section as u32,
            source_hash: overlays::source_hash(text),
            entities: vec!["Lagoon".into()],
        })
        .collect();
    overlays::publish(&db, replacements, |_| Ok(())).unwrap();
    let old = load(&db);
    fixture.mutate("change");
    success(&fixture.index(&db, false, &[]));
    let new = load(&db);
    assert!(generation::read_manifest(&db)
        .unwrap()
        .entity_overlay
        .is_none());
    for section in &new.bm25.sections {
        if section.filename.as_deref().unwrap().ends_with("boat.md") {
            assert!(
                section.entities.is_empty(),
                "changed source must not retain stale entities"
            );
        } else {
            assert_eq!(section.entities, ["Lagoon"]);
        }
    }
    assert!(old.bm25.sections.iter().all(|s| s.entities == ["Lagoon"]));
    let fresh = fixture.db("fresh-entities");
    success(&fixture.index(&fresh, true, &[]));
    let expected = load(&fresh);
    let retained = expected
        .bm25
        .sections
        .iter()
        .enumerate()
        .filter(|(_, text)| !text.filename.as_deref().unwrap().ends_with("boat.md"))
        .map(|(section, text)| overlays::Replacement {
            section: section as u32,
            source_hash: overlays::source_hash(text),
            entities: vec!["Lagoon".into()],
        })
        .collect();
    overlays::publish(&fresh, retained, |_| Ok(())).unwrap();
    assert_replies_equal(&new, &load(&fresh));
}

#[test]
fn corrupt_v4_snapshot_fails_closed_without_changing_pointer() {
    let fixture = Fixture::new();
    let db = fixture.db("v4");
    success(&fixture.index(&db, true, &[]));
    let pointer = std::fs::read(db.join("index.json")).unwrap();
    let manifest = generation::read_manifest(&db).unwrap();
    let text = generation::generation_directory(&db, &manifest)
        .unwrap()
        .join("text.bin");
    std::fs::write(text, b"corrupt").unwrap();
    assert!(!fixture.index(&db, false, &[]).status.success());
    assert_eq!(std::fs::read(db.join("index.json")).unwrap(), pointer);
    assert!(snapshot::restore_state(&db).is_err());
}

#[test]
fn failed_fresh_v4_creation_resumes_without_publishing_partial_tables() {
    let fixture = Fixture::new();
    let db = fixture.db("v4");
    let missing = fixture.root.join("missing-vectors.jsonl");
    let failed = fixture.index(
        &db,
        true,
        &[
            "--embed-model",
            "checkpoint-test",
            "--embed-dimensions",
            "2",
            "--embed-docs",
            missing.to_str().unwrap(),
        ],
    );
    assert!(!failed.status.success());
    assert!(!db.join("index.json").exists());
    assert!(db.join("index-scan-checkpoint.json").exists());
    let resumed = fixture.index(&db, true, &[]);
    success(&resumed);
    assert!(String::from_utf8_lossy(&resumed.stdout).contains("Resuming unpublished"));
    fixture.parity(&db, "fresh-resumed");
}
