#![cfg(feature = "ti")]

use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::TcpListener,
    path::PathBuf,
    process::Command,
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc, Arc,
    },
    thread,
    time::Duration,
};
use ti_contracts::{
    Agg, BucketRecord, Catalog, FieldKind, FieldSpec, FieldValue, ShardSink, VesselSpec,
};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct TestDir {
    root: PathBuf,
    store_root: PathBuf,
}

impl TestDir {
    fn new() -> Self {
        let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
            "chat-sql-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let store_root = root.join("store");
        std::fs::create_dir_all(&store_root).unwrap();

        let mut store = ti_store::Store::open_or_create(&store_root, 10).unwrap();
        let vessel = store
            .catalog()
            .register_vessel(&VesselSpec {
                urn: "vessels.urn:test:chat".into(),
                name: Some("Test Vessel".into()),
                mmsi: None,
            })
            .unwrap();
        let field = store
            .catalog()
            .register_field(&FieldSpec {
                id: 0,
                path: "navigation.speedOverGround".into(),
                agg: Some(Agg::Mean),
                kind: FieldKind::Bsi { scale: 3 },
                units: Some("m/s".into()),
            })
            .unwrap();
        store
            .apply(&[BucketRecord {
                vessel,
                bucket: 1,
                field,
                value: FieldValue::Int(7200),
                rewrite: false,
            }])
            .unwrap();
        store
            .seal(ti_contracts::ShardKey { vessel, shard: 0 })
            .unwrap();
        store.shutdown().unwrap();
        std::fs::write(store_root.join("ti.toml"), "width_seconds = 10\n").unwrap();

        Self { root, store_root }
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

struct MockOllama {
    url: String,
    turn_count: Arc<AtomicUsize>,
    shutdown: Option<mpsc::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl MockOllama {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}");
        let (tx, rx) = mpsc::channel();
        let turn_count = Arc::new(AtomicUsize::new(0));
        let turns = turn_count.clone();

        let thread = thread::spawn(move || {
            listener.set_nonblocking(true).unwrap();
            while rx.try_recv().is_err() {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut buf = vec![0u8; 65536];
                        let mut total_read = 0;
                        while total_read < buf.len() {
                            match stream.read(&mut buf[total_read..]) {
                                Ok(0) => break,
                                Ok(n) => {
                                    total_read += n;
                                    if buf[..total_read].windows(4).any(|w| w == b"\r\n\r\n") {
                                        break;
                                    }
                                }
                                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                    thread::sleep(Duration::from_millis(5));
                                }
                                Err(_) => break,
                            }
                        }

                        let text = String::from_utf8_lossy(&buf[..total_read]);
                        // Parse Content-Length if body is still arriving
                        if let Some(pos) = text.find("\r\n\r\n") {
                            let headers = &text[..pos];
                            let mut content_length = 0;
                            for line in headers.lines() {
                                if line.to_lowercase().starts_with("content-length:") {
                                    if let Some(val) = line.split(':').nth(1) {
                                        if let Ok(len) = val.trim().parse::<usize>() {
                                            content_length = len;
                                        }
                                    }
                                }
                            }
                            let body_start = pos + 4;
                            let mut body_read = total_read - body_start;
                            while body_read < content_length {
                                match stream.read(&mut buf[total_read..]) {
                                    Ok(0) => break,
                                    Ok(n) => {
                                        total_read += n;
                                        body_read += n;
                                    }
                                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                        thread::sleep(Duration::from_millis(5));
                                    }
                                    Err(_) => break,
                                }
                            }
                        }

                        let current_turn = turns.fetch_add(1, Ordering::SeqCst) + 1;
                        let response_body = match current_turn {
                            1 => {
                                // Turn 1: Model calls ti_schema
                                json!({
                                    "message": {
                                        "role": "assistant",
                                        "content": "",
                                        "tool_calls": [
                                            {
                                                "function": {
                                                    "name": "ti_schema",
                                                    "arguments": {}
                                                }
                                            }
                                        ]
                                    }
                                })
                            }
                            2 => {
                                // Turn 2: Model attempts a query with a bad column
                                json!({
                                    "message": {
                                        "role": "assistant",
                                        "content": "",
                                        "tool_calls": [
                                            {
                                                "function": {
                                                    "name": "ti_query",
                                                    "arguments": {
                                                        "sql": "SELECT non_existent_column FROM telemetry"
                                                    }
                                                }
                                            }
                                        ]
                                    }
                                })
                            }
                            3 => {
                                // Turn 3: Model receives error, retries with corrected query
                                json!({
                                    "message": {
                                        "role": "assistant",
                                        "content": "",
                                        "tool_calls": [
                                            {
                                                "function": {
                                                    "name": "ti_query",
                                                    "arguments": {
                                                        "sql": "SELECT ts, \"navigation.speedOverGround\" FROM telemetry ORDER BY ts DESC LIMIT 5"
                                                    }
                                                }
                                            }
                                        ]
                                    }
                                })
                            }
                            _ => {
                                // Turn 4: Model provides final answer with fenced SQL code block
                                json!({
                                    "message": {
                                        "role": "assistant",
                                        "content": "The current speed over ground is recorded as 7.2 m/s.\n\n```sql\nSELECT ts, \"navigation.speedOverGround\" FROM telemetry ORDER BY ts DESC LIMIT 5\n```\n\nAll telemetry records confirmed.",
                                        "tool_calls": []
                                    }
                                })
                            }
                        };

                        let body_bytes = serde_json::to_vec(&response_body).unwrap();
                        let header = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body_bytes.len()
                        );
                        let _ = stream.write_all(header.as_bytes());
                        let _ = stream.write_all(&body_bytes);
                        let _ = stream.flush();
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            url,
            turn_count,
            shutdown: Some(tx),
            thread: Some(thread),
        }
    }
}

impl Drop for MockOllama {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[test]
fn test_mock_ollama_sql_retry_and_json_shape() {
    let fixture = TestDir::new();
    let mock = MockOllama::start();

    let output = Command::new(env!("CARGO_BIN_EXE_lume"))
        .args([
            "chat",
            "--json",
            "--ti-store",
            fixture.store_root.to_str().unwrap(),
            "--ollama-url",
            &mock.url,
            "--ollama-model",
            "mock-test-model",
            "What was our maximum speed over ground yesterday?",
        ])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "Command failed with status {:?}:\nSTDOUT: {}\nSTDERR: {}",
        output.status.code(),
        stdout,
        stderr
    );

    let parsed: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|_| {
        panic!(
            "Failed to parse JSON output: {}\nSTDERR: {}",
            stdout, stderr
        )
    });

    // 1. Assert JSON top-level shape: { answer, sql, tool_calls }
    assert!(parsed.is_object(), "Expected JSON object");
    assert!(parsed.get("answer").is_some(), "Expected 'answer' field");
    assert!(parsed.get("sql").is_some(), "Expected 'sql' field");
    assert!(
        parsed.get("tool_calls").is_some(),
        "Expected 'tool_calls' field"
    );

    let answer = parsed["answer"].as_str().unwrap();
    assert!(
        answer.contains("7.2 m/s"),
        "Answer should contain the model result: {}",
        answer
    );
    assert!(
        answer.contains("```sql"),
        "Answer should contain fenced SQL block: {}",
        answer
    );

    // 2. Assert executed SQL statements
    let sql_list = parsed["sql"].as_array().unwrap();
    assert_eq!(
        sql_list.len(),
        2,
        "Expected 2 executed SQL statements: bad and corrected query"
    );
    assert_eq!(
        sql_list[0].as_str().unwrap(),
        "SELECT non_existent_column FROM telemetry"
    );
    assert_eq!(
        sql_list[1].as_str().unwrap(),
        "SELECT ts, \"navigation.speedOverGround\" FROM telemetry ORDER BY ts DESC LIMIT 5"
    );

    // 3. Assert tool call records and retry behavior
    let tool_calls = parsed["tool_calls"].as_array().unwrap();
    assert_eq!(
        tool_calls.len(),
        3,
        "Expected 3 tool calls: ti_schema, bad query, corrected query"
    );

    // Tool Call 1: ti_schema
    assert_eq!(tool_calls[0]["name"].as_str().unwrap(), "ti_schema");
    assert!(tool_calls[0]["error"].is_null());

    // Tool Call 2: bad ti_query (failed with error)
    assert_eq!(tool_calls[1]["name"].as_str().unwrap(), "ti_query");
    assert!(
        tool_calls[1]["error"].is_string(),
        "Second tool call should have recorded an error"
    );
    let err_msg = tool_calls[1]["error"].as_str().unwrap();
    assert!(
        err_msg.contains("non_existent_column")
            || err_msg.contains("not found")
            || err_msg.contains("schema"),
        "Error message should mention the invalid column: {}",
        err_msg
    );

    // Tool Call 3: corrected ti_query (succeeded with rows)
    assert_eq!(tool_calls[2]["name"].as_str().unwrap(), "ti_query");
    assert!(
        tool_calls[2]["error"].is_null(),
        "Third tool call should succeed without error"
    );
    assert_eq!(
        tool_calls[2]["rows"].as_u64(),
        Some(1),
        "Corrected query should have returned 1 row"
    );

    // 4. Assert turns on mock Ollama
    assert_eq!(
        mock.turn_count.load(Ordering::SeqCst),
        4,
        "Expected exactly 4 turns on Ollama"
    );
}

#[test]
fn test_mock_ollama_chat_events_ndjson() {
    let fixture = TestDir::new();
    let mock = MockOllama::start();

    let output = Command::new(env!("CARGO_BIN_EXE_lume"))
        .args([
            "chat",
            "--json",
            "--events",
            "--ti-store",
            fixture.store_root.to_str().unwrap(),
            "--ollama-url",
            &mock.url,
            "--ollama-model",
            "mock-test-model",
            "What was our maximum speed over ground yesterday?",
        ])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "Command failed with status {:?}:\nSTDOUT: {}\nSTDERR: {}",
        output.status.code(),
        stdout,
        stderr
    );

    // 1. Stdout must be unchanged valid JSON with answer, sql, tool_calls
    let parsed: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|_| {
        panic!(
            "Failed to parse stdout JSON: {}\nSTDERR: {}",
            stdout, stderr
        )
    });
    assert!(parsed.get("answer").is_some());
    assert!(parsed.get("sql").is_some());
    assert!(parsed.get("tool_calls").is_some());

    // 2. Stderr must contain NDJSON lines with thinking, tool_call, tool_result events
    let lines: Vec<&str> = stderr
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    assert!(!lines.is_empty(), "Expected NDJSON event lines on stderr");

    let mut events = Vec::new();
    for line in lines {
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            events.push(v);
        }
    }

    assert!(
        !events.is_empty(),
        "Expected valid JSON events parsed from stderr: {}",
        stderr
    );

    let thinking_events: Vec<&Value> = events
        .iter()
        .filter(|e| e.get("event").and_then(|v| v.as_str()) == Some("thinking"))
        .collect();
    assert!(
        !thinking_events.is_empty(),
        "Expected thinking events in stderr stream"
    );
    for e in &thinking_events {
        assert!(e.get("turn").and_then(|v| v.as_u64()).is_some());
    }

    let tool_call_events: Vec<&Value> = events
        .iter()
        .filter(|e| e.get("event").and_then(|v| v.as_str()) == Some("tool_call"))
        .collect();
    assert_eq!(
        tool_call_events.len(),
        3,
        "Expected 3 tool_call events (ti_schema, bad query, corrected query)"
    );
    assert_eq!(tool_call_events[0]["name"].as_str().unwrap(), "ti_schema");
    assert_eq!(tool_call_events[1]["name"].as_str().unwrap(), "ti_query");
    assert_eq!(tool_call_events[2]["name"].as_str().unwrap(), "ti_query");

    let tool_result_events: Vec<&Value> = events
        .iter()
        .filter(|e| e.get("event").and_then(|v| v.as_str()) == Some("tool_result"))
        .collect();
    assert_eq!(tool_result_events.len(), 3, "Expected 3 tool_result events");

    // Check ti_schema result
    assert_eq!(tool_result_events[0]["name"].as_str().unwrap(), "ti_schema");
    assert!(tool_result_events[0]["elapsed_ms"].is_number());
    assert!(tool_result_events[0]["error"].is_null());

    // Check bad query result (has error)
    assert_eq!(tool_result_events[1]["name"].as_str().unwrap(), "ti_query");
    assert!(tool_result_events[1]["elapsed_ms"].is_number());
    assert!(tool_result_events[1]["error"].is_string());

    // Check corrected query result (has rows)
    assert_eq!(tool_result_events[2]["name"].as_str().unwrap(), "ti_query");
    assert!(tool_result_events[2]["elapsed_ms"].is_number());
    assert!(tool_result_events[2]["error"].is_null());
    assert_eq!(tool_result_events[2]["rows"].as_u64(), Some(1));

    // Ensure no secrets or API keys leaked into any event
    for e in &events {
        let text = e.to_string();
        assert!(!text.to_lowercase().contains("bearer"));
        assert!(!text.to_lowercase().contains("api_key"));
    }
}
