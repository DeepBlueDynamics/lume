use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn cli_import_and_hybrid_search_use_preloaded_vectors_offline() {
    let root = std::env::temp_dir().join(format!("lume-local-cli-{}", lume::uuid_v4()));
    std::fs::create_dir_all(root.join("docs")).unwrap();
    let fixture = Fixture(root);
    std::fs::write(fixture.0.join("docs/a.txt"), "captain sailed").unwrap();
    std::fs::write(fixture.0.join("docs/b.txt"), "engine failed").unwrap();
    let documents = fixture.0.join("embeddinggemma-2-2-test-docs.jsonl");
    let queries = fixture.0.join("embeddinggemma-2-2-test-queries.jsonl");
    let texts = fixture.0.join("queries.tsv");
    std::fs::write(
        &documents,
        "{\"id\":\"a\",\"vector\":[1,0]}\n{\"id\":\"b\",\"vector\":[0,1]}",
    )
    .unwrap();
    std::fs::write(&queries, r#"{"id":"q","vector":[1,0]}"#).unwrap();
    std::fs::write(&texts, "q\tcaptain\n").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let binary = env!("CARGO_BIN_EXE_lume");
    let result = Command::new(binary)
        .arg("index")
        .arg(fixture.0.join("docs"))
        .arg("--db")
        .arg(fixture.0.join("index"))
        .args([
            "--embed-model",
            "embeddinggemma-2",
            "--embed-dimensions",
            "2",
        ])
        .arg("--embed-docs")
        .arg(&documents)
        .arg("--embed-queries")
        .arg(&queries)
        .arg("--embed-query-texts")
        .arg(&texts)
        .args(["--shivvr-url", &base])
        .env("LUME_STEM", "0")
        .env_remove("LUME_QUERY_INVERSION")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let result = Command::new(binary)
        .args(["search", "captain", "--alpha", "1", "--graph", "0"])
        .arg("--db")
        .arg(fixture.0.join("index"))
        .args(["--shivvr-url", &base])
        .env("LUME_BLEND_NORM", "1")
        .env("LUME_STEM", "0")
        .env_remove("LUME_EMBED_MODEL")
        .env_remove("LUME_EMBED_DIMENSIONS")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let output = String::from_utf8(result.stdout).unwrap();
    assert!(output.contains("Hybrid Score:"), "{output}");
    assert!(output.contains("a.txt"), "{output}");
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}
