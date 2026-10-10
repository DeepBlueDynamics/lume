use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("lume-scan-checkpoint-{}", lume::uuid_v4()));
        std::fs::create_dir_all(root.join("docs")).unwrap();
        Self(root)
    }
    fn index(&self, extra: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_lume"))
            .arg("index")
            .arg(self.0.join("docs"))
            .arg("--db")
            .arg(self.0.join("index"))
            .args(extra)
            .env("LUME_TIMING", "1")
            .env_remove("LUME_EMBED_MODEL")
            .env_remove("LUME_EMBED_DIMENSIONS")
            .output()
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn json(path: &Path) -> serde_json::Value {
    lume::search::load_json(path).unwrap()
}

#[test]
fn failed_scan_keeps_published_generation_and_resumes_frontmatter() {
    let fixture = Fixture::new();
    let doc = fixture.0.join("docs/boat.md");
    std::fs::write(&doc, "---\ncategory: manual\n---\n# Pump\nOld bilge instructions.").unwrap();
    let built = fixture.index(&[]);
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let db = fixture.0.join("index");
    let files = ["manifest.json", "state.json", "bm25.json", "spelling.json", "meta.json"];
    let published: Vec<_> = files.iter().map(|file| std::fs::read(db.join(file)).unwrap()).collect();
    std::fs::write(&doc, "---\ncategory: safety\n---\n# Pump\nNew emergency pump instructions.").unwrap();
    let missing = fixture.0.join("missing-vectors.jsonl");
    let failed = fixture.index(&[
        "--force", "--embed-model", "checkpoint-test", "--embed-dimensions", "2",
        "--embed-docs", missing.to_str().unwrap(),
    ]);
    assert!(!failed.status.success(), "vector import must fail before publication");
    assert!(db.join("index-scan-checkpoint.json").exists());
    for (file, before) in files.iter().zip(&published) {
        assert_eq!(&std::fs::read(db.join(file)).unwrap(), before, "{file} changed before publication");
    }
    let loaded = lume::search::LoadedIndex::open(&db).unwrap();
    assert!(loaded.bm25.sections.iter().any(|section| section.body.contains("Old bilge")));
    assert!(!loaded.bm25.sections.iter().any(|section| section.body.contains("New emergency")));

    let resumed = fixture.index(&[]);
    assert!(resumed.status.success(), "{}", String::from_utf8_lossy(&resumed.stderr));
    assert!(String::from_utf8_lossy(&resumed.stdout).contains("Resuming unpublished"));
    assert!(!db.join("index-scan-checkpoint.json").exists());
    let loaded = lume::search::LoadedIndex::open(&db).unwrap();
    assert!(loaded.bm25.sections.iter().any(|section| section.body.contains("New emergency")));
    let resumed_bm25 = json(&db.join("bm25.json"));
    let resumed_state = json(&db.join("state.json"));
    let resumed_meta = json(&db.join("meta.json"));
    // Rebuilding the same source from scratch must preserve every stored BM25
    // and state value, and frontmatter (apart from the fresh generation UUID).
    let fresh = fixture.index(&["--force"]);
    assert!(fresh.status.success(), "{}", String::from_utf8_lossy(&fresh.stderr));
    assert_eq!(resumed_bm25, json(&db.join("bm25.json")));
    assert_eq!(resumed_state, json(&db.join("state.json")));
    let mut fresh_meta = json(&db.join("meta.json"));
    fresh_meta["generation"] = resumed_meta["generation"].clone();
    assert_eq!(resumed_meta, fresh_meta);
}

#[test]
fn stale_checkpoint_cannot_override_a_new_published_generation() {
    let fixture = Fixture::new();
    std::fs::write(fixture.0.join("docs/boat.txt"), "Captain repairs bilge pump.").unwrap();
    let built = fixture.index(&[]);
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let db = fixture.0.join("index");
    let before = json(&db.join("bm25.json"));
    let checkpoint = serde_json::json!({
        "version": 1,
        "target_dir": fixture.0.join("docs"),
        "published_manifest": {"generation": "stale", "sections": 1},
        "cached_files": {},
        "frontmatter_by_file": {}
    });
    lume::search::save_json(&db.join("index-scan-checkpoint.json"), &checkpoint).unwrap();
    let built = fixture.index(&[]);
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    assert!(String::from_utf8_lossy(&built.stderr).contains("Ignoring stale source"));
    assert_eq!(before, json(&db.join("bm25.json")));
}
