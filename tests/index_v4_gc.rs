use lume::index_binary::{gc, generation};
use lume::search::{LoadedIndex, OpenEnvChecks, SearchMode, SearchOptions};
use std::path::PathBuf;
use std::process::Command;

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_lume"));
    command
        .env("LUME_STEM", "1")
        .env("LUME_QUERY_INVERSION", "0")
        .env_remove("LUME_EMBED_MODEL")
        .env_remove("LUME_EMBED_DIMENSIONS");
    command
}
#[test]
fn loaded_old_snapshot_keeps_answering_after_its_generation_is_collected() {
    let root = std::env::temp_dir().join(format!("lume-gc-cli-{}", lume::uuid_v4()));
    std::fs::create_dir(&root).unwrap();
    let fixture = Fixture(root);
    let source = fixture.0.join("docs");
    std::fs::create_dir(&source).unwrap();
    let db = fixture.0.join("index");
    let write = |text: &str| {
        std::fs::write(source.join("boat.txt"), text).unwrap();
        let output = command()
            .arg("index")
            .arg("-f")
            .arg(&source)
            .arg("--db")
            .arg(&db)
            .env("LUME_INDEX_FORMAT", "4")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    write("Bilge pump removes water.");
    let old =
        std::sync::Arc::new(LoadedIndex::open_with_checks(&db, OpenEnvChecks::default()).unwrap());
    let initial = generation::read_manifest(&db).unwrap();
    let options = SearchOptions {
        mode: SearchMode::LexicalOnly,
        ..Default::default()
    };
    let before =
        serde_json::to_value(lume::search::search(&old, "bilge", &options).unwrap()).unwrap();
    write("Anchor holds the boat.");
    write("Engine drives the boat.");
    let current = generation::read_manifest(&db).unwrap();
    let preview = command()
        .args(["index", "gc", "--db"])
        .arg(&db)
        .args(["--gc-grace-secs", "0", "--dry-run"])
        .output()
        .unwrap();
    assert!(preview.status.success());
    assert!(String::from_utf8_lossy(&preview.stdout).contains("Would remove"));
    assert!(db.join("generations").join(&initial.generation).exists());
    let cleaned = command()
        .args(["index", "gc", "--db"])
        .arg(&db)
        .args(["--gc-grace-secs", "0"])
        .output()
        .unwrap();
    assert!(
        cleaned.status.success(),
        "{}",
        String::from_utf8_lossy(&cleaned.stderr)
    );
    assert!(!db.join("generations").join(initial.generation).exists());
    assert!(db.join("generations").join(current.generation).exists());
    assert_eq!(
        serde_json::to_value(lume::search::search(&old, "bilge", &options).unwrap()).unwrap(),
        before
    );
    assert!(gc::collect(&db, &gc::Options::default())
        .unwrap()
        .removed
        .is_empty());
}
