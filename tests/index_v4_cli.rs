use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};
use std::time::{Duration, Instant};

struct Fixture {
    root: PathBuf,
    server: Option<Child>,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("lume-v4-cli-{}", lume::uuid_v4()));
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(
            root.join("docs/manual.txt"),
            "Bilge pump\n\nThe bilge pump removes water from the hull.",
        )
        .unwrap();
        Self { root, server: None }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(child) = self.server.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_lume"));
    for key in [
        "LUME_INDEX_FORMAT",
        "LUME_EMBED_MODEL",
        "LUME_EMBED_DIMENSIONS",
        "LUME_TIMING",
    ] {
        command.env_remove(key);
    }
    command
        .env("LUME_STEM", "1")
        .env("LUME_QUERY_INVERSION", "0");
    command
}
fn index(source: &Path, db: &Path, v4: bool, extra: &[&str]) -> Output {
    let mut cmd = command();
    if v4 {
        cmd.env("LUME_INDEX_FORMAT", "4");
    }
    cmd.arg("index")
        .arg("-f")
        .arg(source)
        .arg("--db")
        .arg(db)
        .args(extra)
        .output()
        .unwrap()
}
fn success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
#[test]
fn cli_v4_publish_search_sql_mcp_and_reindex_with_json_neighbor() {
    let mut fixture = Fixture::new();
    let source = fixture.root.join("docs");
    let v4 = fixture.root.join("v4");
    let json = fixture.root.join("json");
    success(&index(&source, &v4, true, &[]));
    success(&index(&source, &json, false, &[]));
    assert!(v4.join("index.json").exists());
    assert!(!v4.join("bm25.json").exists());
    assert!(json.join("bm25.json").exists());
    let old = lume::index_binary::generation::read_manifest(&v4).unwrap();
    let search = |db: &Path| {
        command()
            .args(["search", "--alpha", "0", "--graph", "0", "--db"])
            .arg(db)
            .arg("bilge pump")
            .output()
            .unwrap()
    };
    let binary_reply = search(&v4);
    let json_reply = search(&json);
    success(&binary_reply);
    success(&json_reply);
    // The CLI header intentionally names the selected database directory.
    let normalized = |output: &Output, db: &Path| {
        let text = std::str::from_utf8(&output.stdout).unwrap();
        let path = db.to_str().unwrap();
        assert!(text.lines().next().unwrap().contains(path));
        text.replacen(path, "<DB>", 1)
    };
    assert_eq!(
        normalized(&binary_reply, &v4),
        normalized(&json_reply, &json)
    );
    let sql = command()
        .args(["sql", "--db"])
        .arg(&v4)
        .arg("SELECT count(*) FROM sections")
        .output()
        .unwrap();
    success(&sql);
    assert!(!sql.stdout.is_empty());
    let rejected = index(&source, &v4, true, &["-o"]);
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr)
        .contains("entity-overlay publication is not yet implemented"));
    assert_eq!(
        lume::index_binary::generation::read_manifest(&v4)
            .unwrap()
            .generation,
        old.generation
    );

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    fixture.server = Some(
        command()
            .args(["serve", "--bind", "127.0.0.1", "--port"])
            .arg(port.to_string())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    let start = Instant::now();
    let mut stream = loop {
        if let Ok(stream) = TcpStream::connect(("127.0.0.1", port)) {
            break stream;
        }
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "MCP server did not start"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    let body = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"lume_search","arguments":{"db":v4,"query":"bilge pump","alpha":0,"graph":0}}}).to_string();
    write!(stream, "POST /mcp HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).unwrap();
    assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
    let (_, payload) = reply.split_once("\r\n\r\n").unwrap();
    let rpc: serde_json::Value = serde_json::from_str(payload).unwrap();
    assert!(rpc.get("error").is_none(), "{rpc}");
    assert!(payload.contains("bilge"), "{payload}");

    std::fs::write(
        source.join("manual.txt"),
        "Anchor\n\nAn anchor keeps the boat in place.",
    )
    .unwrap();
    success(&index(&source, &v4, true, &[]));
    let new = lume::index_binary::generation::read_manifest(&v4).unwrap();
    assert_ne!(old.generation, new.generation);
    assert!(v4.join("generations").join(old.generation).exists());
    let anchor = command()
        .args(["search", "--alpha", "0", "--graph", "0", "--db"])
        .arg(&v4)
        .arg("anchor")
        .output()
        .unwrap();
    success(&anchor);
    assert!(String::from_utf8_lossy(&anchor.stdout)
        .to_lowercase()
        .contains("anchor"));
    success(&search(&json));
}
