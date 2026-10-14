use std::path::Path;
use std::process::Command;

const BODY: &str = "The bilge pump removes water from the hull. Check the float switch, \
                    the strainer and the discharge hose before every passage.";

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn scanned(root: &Path, db: &Path, format: Option<&str>, extra: &[&str]) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lume"));
    cmd.arg("index").arg(root).arg("--db").arg(db).args(extra);
    match format {
        Some(fmt) => {
            cmd.env("LUME_INDEX_FORMAT", fmt);
        }
        None => {
            cmd.env_remove("LUME_INDEX_FORMAT");
        }
    }
    let output = cmd.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn index_applies_defaults_gitignore_and_lumeignore() {
    let base = std::env::temp_dir().join(format!("lume-ignore-cli-{}", lume::uuid_v4()));
    let root = base.join("docs");
    write(&root.join("keep.md"), &format!("# Keep\n{BODY}"));
    write(&root.join("generated.md"), &format!("# Generated\n{BODY}"));
    write(&root.join("._keep.md"), &format!("# Sidecar\n{BODY}"));
    write(
        &root.join("node_modules/pkg/readme.md"),
        &format!("# Pkg\n{BODY}"),
    );
    write(&root.join("notes/wanted.md"), &format!("# Wanted\n{BODY}"));
    write(&root.join(".gitignore"), "generated.md\nnotes/\n");
    write(
        &root.join(".lumeignore"),
        "# keep the notes after all\n!notes/\n",
    );

    let with_git = scanned(&root, &base.join("db1"), None, &[]);
    assert!(base.join("db1/index.json").exists());
    assert!(!base.join("db1/bm25.json").exists());
    assert!(
        with_git.contains("Scanned 2 indexable files"),
        "expected keep.md and notes/wanted.md:\n{with_git}"
    );
    assert!(with_git.contains("Ignore rules: defaults 3, .gitignore 2, .lumeignore 1"));

    let without_git = scanned(&root, &base.join("db2"), Some("json"), &["--no-gitignore"]);
    assert!(base.join("db2/bm25.json").exists());
    assert!(!base.join("db2/index.json").exists());
    assert!(
        without_git.contains("Scanned 3 indexable files"),
        "expected keep.md, generated.md and notes/wanted.md:\n{without_git}"
    );
    std::fs::remove_dir_all(&base).unwrap();
}
