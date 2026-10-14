use std::process::Command;

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
    for (db_name, format_env, build_phases, open_phases) in [
        (
            "index-v4",
            None,
            &[
                "index.walk",
                "index.read",
                "index.parse_sections",
                "index.tagging",
                "index.tokenize",
                "index.bm25_total",
                "index.spelling",
                "v4.encode.core",
                "v4.publish.write",
            ][..],
            &["v4.open.read", "v4.decode.sections", "open.total"][..],
        ),
        (
            "index-json",
            Some("json"),
            &[
                "index.walk",
                "index.read",
                "index.parse_sections",
                "index.tagging",
                "index.tokenize",
                "index.bm25_total",
                "index.spelling",
                "json.serialize",
                "json.write",
            ][..],
            &[
                "open.read",
                "open.parse_reconstruct",
                "open.total",
                "open.bm25_reconstruct_postings",
            ][..],
        ),
    ] {
        let db = root.join(db_name);
        let mut cmd = Command::new(binary);
        cmd.args(["index"])
            .arg(root.join("docs"))
            .arg("--db")
            .arg(&db)
            .env("LUME_TIMING", "1");
        match format_env {
            Some(fmt) => {
                cmd.env("LUME_INDEX_FORMAT", fmt);
            }
            None => {
                cmd.env_remove("LUME_INDEX_FORMAT");
            }
        }
        let built = cmd.output().unwrap();
        assert!(
            built.status.success(),
            "{}",
            String::from_utf8_lossy(&built.stderr)
        );
        let stderr = String::from_utf8_lossy(&built.stderr);
        for phase in build_phases {
            assert!(stderr.contains(phase), "{stderr}");
        }
        assert_eq!(
            stderr
                .lines()
                .filter(|line| line.contains("\"phase\":\"index.bm25_total\""))
                .count(),
            1,
            "ordinary indexing must build BM25 once: {stderr}"
        );
        assert!(!String::from_utf8_lossy(&built.stdout).contains("LUME_TIMING"));
        let search = |timing: &str| {
            Command::new(binary)
                .args(["search", "--alpha", "0", "--graph", "0", "--db"])
                .arg(&db)
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
        for phase in open_phases {
            assert!(stderr.contains(phase), "{stderr}");
        }
    }
    std::fs::remove_dir_all(root).unwrap();
}
