use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn post(port: u16, body: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    write!(
        stream,
        "POST /mcp HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
    .unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).unwrap();
    reply
}

#[test]
fn serve_writes_nothing_to_stdout_or_stderr() {
    let root = std::env::temp_dir().join(format!("lume-serve-quiet-{}", lume::uuid_v4()));
    std::fs::create_dir_all(root.join("docs")).unwrap();
    std::fs::write(
        root.join("docs/pump.md"),
        "# Pump\nThe bilge pump removes water from the hull. Check the float switch and \
         the strainer before every passage, and clear any debris from the intake.\n",
    )
    .unwrap();
    let db = root.join("index");
    let indexed = Command::new(env!("CARGO_BIN_EXE_lume"))
        .arg("index")
        .arg(root.join("docs"))
        .arg("--db")
        .arg(&db)
        .output()
        .unwrap();
    assert!(indexed.status.success());

    let port = {
        let probe = TcpListener::bind("127.0.0.1:0").unwrap();
        probe.local_addr().unwrap().port()
    };
    let mut server = Command::new(env!("CARGO_BIN_EXE_lume"))
        .args(["serve", "--bind", "127.0.0.1", "--port"])
        .arg(port.to_string())
        .current_dir(&root)
        .env_remove("LUME_DEBUG")
        .env_remove("LUME_QUIET")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "server did not start"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    let db_arg = serde_json::Value::String(db.to_string_lossy().into_owned());
    let calls = [
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        serde_json::json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"lume_search","arguments":{"db":db_arg,"query":"bilge pump"}}}),
        serde_json::json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"lume_search","arguments":{"db":db_arg,"query":"-pump"}}}),
    ];
    for call in calls {
        let reply = post(port, &call.to_string());
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
    }

    server.kill().unwrap();
    let output = server.wait_with_output().unwrap();
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        output.stdout.is_empty(),
        "stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        output.stderr.is_empty(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
