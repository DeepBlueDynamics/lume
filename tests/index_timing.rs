_eq!(
        stderr.lines().filter(|line| line.contains("\"phase\":\"index.bm25_total\"")).count(),
        1,
        "ordinary indexing must build BM25 once: {stderr}"
    );
    assertuse std::process::Command;

#[test]
fn timing_is_opt_in_stderr_only_and_preserves_search_output() {
    let root = std::env::temp_dir().join(format!("lume-index-timing-{}", lume::uuid_v4()));
    std::fs::create_dir_all(root.join("docs")).unwrap();
    std::fs::write(
        root.join("docs/boat.txt"),
        "Captain repairs the bilge pump.",
    )
    .unwrap();
    let binary = env!("CARGO_BIN_EXE_lume");
    let built = Command::new(binary)
        .args(["index"])
        .arg(root.join("docs"))
        .arg("--db")
        .arg(root.join("index"))
        .env("LUME_TIMING", "1")
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let stderr = String::from_utf8_lossy(&built.stderr);
    for phase in [
        "index.walk",
        "index.read",
        "index.parse_sections",
        "index.tagging",
        "index.tokenize",
        "index.bm25_total",
        "index.spelling",
        "json.serialize",
        "json.write",
    ] {
        assert!(stderr.contains(phase), "{stderr}");
    }
    assert!(!String::from_utf8_lossy(&built.stdout).contains("LUME_TIMING"));
    let search = |timing: &str| {
        Command::new(binary)
            .args(["search", "--alpha", "0", "--graph", "0", "--db"])
            .arg(root.join("index"))
            .arg("bilge")
            .env("LUME_TIMING", timing)
            .output()
            .unwrap()
    };
    let off = search("0");
    let on = search("1");
    assert!(off.status.success() && on.status.success());
    assert_eq!(off.stdout, on.stdout);
    assert!(!String::from_utf8_lossy(&off.stderr).contains("LUME_TIMING"));
    let stderr = String::from_utf8_lossy(&on.stderr);
    for phase in [
        "open.read",
        "open.parse_reconstruct",
        "open.total",
        "open.bm25_reconstruct_postings",
    ] {
        assert!(stderr.contains(phase), "{stderr}");
    }
    std::fs::remove_dir_all(root).unwrap();
}
