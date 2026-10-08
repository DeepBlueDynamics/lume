#![cfg(feature = "ti")]
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Barrier, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
const METRICS: &str = include_str!("golden/otlp/metrics.json");
const LOGS: &str = include_str!("golden/otlp/logs.json");
struct Server {
    child: Child,
    root: PathBuf,
    url: String,
    standalone: bool,
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
impl Server {
    fn start(standalone: bool, enabled: bool, auth: bool) -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
            "otlp-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let store = root.join("store");
        let mut command = Command::new(env!("CARGO_BIN_EXE_lume"));
        if standalone {
            command.args(["ti", "otlp", "--store"]);
        } else {
            drop(ti_store::Store::open_or_create(&store, 10).unwrap());
            std::fs::write(store.join("ti.toml"), "width_seconds=10\n").unwrap();
            command.args(["serve", "--ti-store"]);
        }
        command.arg(&store).args(["--port", "0"]);
        if enabled && !standalone {
            command.arg("--otlp");
        }
        if auth {
            let path = root.join("token");
            std::fs::write(&path, "test-otlp-token\n").unwrap();
            command.arg("--otlp-token-file").arg(path);
        }
        let child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let mut server = Self {
            child,
            root,
            url: String::new(),
            standalone,
        };
        let mut output = BufReader::new(server.child.stdout.take().unwrap());
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        server.url = line.split_whitespace().last().unwrap().to_string();
        assert!(server.url.starts_with("http://127.0.0.1:"), "{line}");
        server
    }
    fn restart(&mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
        self.child = Command::new(env!("CARGO_BIN_EXE_lume"))
            .args(["ti", "otlp", "--store"])
            .arg(self.root.join("store"))
            .args(["--port", "0", "--otlp-token-file"])
            .arg(self.root.join("token"))
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let mut output = BufReader::new(self.child.stdout.take().unwrap());
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        self.url = line.split_whitespace().last().unwrap().to_string();
        assert!(self.url.starts_with("http://127.0.0.1:"), "{line}");
    }
    // The test helper hands ureq's own error back so tests can match status codes.
    #[allow(clippy::result_large_err)]
    fn post(
        &self,
        path: &str,
        body: &str,
        token: Option<&str>,
    ) -> Result<ureq::Response, ureq::Error> {
        let mut request = ureq::post(&format!("{}{path}", self.url))
            .set("Content-Type", "application/json")
            .set("Accept", "application/json")
            .timeout(Duration::from_secs(30));
        if let Some(token) = token {
            request = request.set("Authorization", &format!("Bearer {token}"));
        }
        request.send_string(body)
    }
    fn sql(&self, sql: &str) -> Value {
        if self.standalone {
            let runtime = ti_sql::surface_runtime().unwrap();
            let documents = |root: &std::path::Path, store: &ti_store::Store, width: u64| {
                Ok(std::sync::Arc::new(lume::ti_text::LumeText::open(
                    root,
                    store.catalog().clone(),
                    width,
                )?)
                    as std::sync::Arc<dyn ti_contracts::DocumentIndex>)
            };
            let engine = runtime
                .block_on(ti_sql::TiEngine::open(
                    &self.root.join("store"),
                    None,
                    Some(&documents),
                ))
                .unwrap();
            return runtime.block_on(engine.query(sql, 500)).unwrap();
        }
        self.post("/ti/query", &json!({"sql":sql}).to_string(), None)
            .unwrap()
            .into_json()
            .unwrap()
    }
}
#[test]
fn golden_otlp_metrics_logs_sql_and_auth() {
    let server = Server::start(false, true, true);
    assert!(matches!(
        server.post("/v1/metrics", METRICS, None),
        Err(ureq::Error::Status(401, _))
    ));
    assert!(matches!(
        server.post("/v1/metrics", METRICS, Some("wrong")),
        Err(ureq::Error::Status(401, _))
    ));
    let protobuf = ureq::post(&format!("{}/v1/metrics", server.url))
        .set("Content-Type", "application/x-protobuf")
        .set("Authorization", "Bearer test-otlp-token")
        .send_bytes(&[0x0a, 0x00]);
    match protobuf {
        Err(ureq::Error::Status(415, response)) => {
            let body: Value = response.into_json().unwrap();
            assert!(body["error"]
                .as_str()
                .unwrap()
                .contains("lume accepts OTLP http/json only; set protocol json"));
        }
        other => panic!("Expected protobuf 415: {other:?}"),
    }
    let reply: Value = server
        .post("/v1/metrics", METRICS, Some("test-otlp-token"))
        .unwrap()
        .into_json()
        .unwrap();
    assert_eq!(reply, json!({}));
    let rows = server.sql("SELECT vessel, sum(\"claude_code.token.usage\") AS tokens FROM telemetry_agents GROUP BY vessel");
    assert_eq!(rows["rows"][0]["vessel"], "agent.urn:pane-123");
    assert_eq!(rows["rows"][0]["tokens"], 30.0);
    let histogram = server.sql("SELECT \"claude_code.tool.duration.sum\", \"claude_code.tool.duration.count\" FROM telemetry_agents WHERE \"claude_code.tool.duration.count\" IS NOT NULL");
    assert_eq!(histogram["row_count"], 1);
    assert!(
        histogram["rows"][0].to_string().contains("4.5"),
        "{histogram}"
    );
    server
        .post("/v1/logs", LOGS, Some("test-otlp-token"))
        .unwrap();
    server
        .post("/v1/logs", LOGS, Some("test-otlp-token"))
        .unwrap();
    let docs = server.sql("SELECT vessel, title FROM docs WHERE match(body, 'service.rs')");
    assert_eq!(docs["row_count"], 1);
    assert_eq!(docs["rows"][0]["vessel"], "agent.urn:pane-123");
    assert_eq!(docs["rows"][0]["title"], "file.edit");
    assert!(matches!(
        server.post("/v1/logs", "{", Some("test-otlp-token")),
        Err(ureq::Error::Status(400, _))
    ));
    assert_eq!(
        server.sql("SELECT count(*) AS n FROM telemetry_agents")["rows"][0]["n"],
        2
    );
    // Exporting twice inside a bucket keeps both gauge values, rather than overwriting the first snapshot.
    let point = |value| {
        json!({"resourceMetrics":[{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":"another-agent"}}]},"scopeMetrics":[{"metrics":[{"name":"load","gauge":{"dataPoints":[{"timeUnixNano":"1577836811000000000","asDouble":value}]}}]}]}]}).to_string()
    };
    server
        .post("/v1/metrics", &point(2), Some("test-otlp-token"))
        .unwrap();
    server
        .post("/v1/metrics", &point(4), Some("test-otlp-token"))
        .unwrap();
    assert_eq!(
        server
            .sql("SELECT \"load\" FROM telemetry_agents WHERE vessel = 'agent.urn:another-agent'")
            ["rows"][0]["load@mean"],
        3.0
    );
    // Reject before allocating/reading a body that exceeds the OTLP cap.
    let address = server.url.strip_prefix("http://").unwrap();
    let mut socket = TcpStream::connect(address).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    write!(
        socket,
        "POST /v1/logs HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n",
        ti_ingest::otlp::MAX_BODY + 1
    )
    .unwrap();
    let mut response = String::new();
    socket.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 413"), "{response}");
}
#[test]
fn standalone_defaults_to_loopback_and_optional_auth() {
    let server = Server::start(true, true, false);
    server.post("/v1/logs", LOGS, None).unwrap();
    assert_eq!(
        server.sql("SELECT title FROM docs WHERE match(body, 'service.rs')")["row_count"],
        1
    );
}
#[test]
fn standalone_exposes_only_otlp_posts_with_or_without_auth() {
    for auth in [false, true] {
        let server = Server::start(true, true, auth);
        for path in [
            "/ti/query",
            "/ti/status",
            "/mcp",
            "/message",
            "/sse",
            "/ti/schema",
        ] {
            assert!(
                matches!(
                    server.post(path, "{}", Some("test-otlp-token")),
                    Err(ureq::Error::Status(404, _))
                ),
                "{path}"
            );
            assert!(
                matches!(
                    ureq::get(&format!("{}{path}", server.url)).call(),
                    Err(ureq::Error::Status(404, _))
                ),
                "{path}"
            );
        }
        assert!(matches!(
            ureq::get(&format!("{}/v1/metrics", server.url)).call(),
            Err(ureq::Error::Status(404, _))
        ));
        server
            .post("/v1/logs", LOGS, auth.then_some("test-otlp-token"))
            .unwrap();
        server
            .post("/v1/metrics", METRICS, auth.then_some("test-otlp-token"))
            .unwrap();
    }
}
#[test]
fn endpoints_are_disabled_without_opt_in() {
    let server = Server::start(false, false, false);
    assert!(matches!(
        server.post("/v1/logs", LOGS, None),
        Err(ureq::Error::Status(404, _))
    ));
}

#[test]
fn monotonic_delta_cumulative_reset_and_dimensions_have_correct_window_totals() {
    // A separate process/root avoids mixing counter restart semantics with gauge tests.
    let server = Server::start(true, true, true);
    for (mode, payload, expected) in [
        ("delta", include_str!("golden/otlp/delta.json"), 30.0),
        (
            "cumulative",
            include_str!("golden/otlp/cumulative.json"),
            35.0,
        ),
    ] {
        server
            .post("/v1/metrics", payload, Some("test-otlp-token"))
            .unwrap();
        // Batch retry must not increment delta totals twice.
        server
            .post("/v1/metrics", payload, Some("test-otlp-token"))
            .unwrap();
        let sql = format!("SELECT max(\"claude_code.token.usage.input.model.claude-sonnet@last\") - min(\"claude_code.token.usage.input.model.claude-sonnet@last\") AS tokens FROM telemetry_agents WHERE vessel = 'agent.urn:{mode}'");
        let result = server.sql(&sql);
        assert_eq!(result["rows"][0]["tokens"], expected, "{mode}: {result}");
    }
    let path = "claude_code.token.usage.input.model.claude-sonnet@last";
    let result = server.sql(&format!(
        "SELECT \"{path}\" FROM telemetry_agents WHERE vessel = 'agent.urn:delta' ORDER BY ts"
    ));
    assert_eq!(result["row_count"], 2);
    assert_eq!(result["rows"][1][path], 30.0);
    assert!(result.to_string().contains("{token}"));
}

#[test]
fn monotonic_totals_survive_receiver_and_exporter_restarts() {
    for (mode, payload, expected) in [
        ("delta", include_str!("golden/otlp/delta.json"), 30.0),
        (
            "cumulative",
            include_str!("golden/otlp/cumulative.json"),
            35.0,
        ),
    ] {
        // Split at a bucket boundary: samples within a closed bucket cannot be repaired
        // after restart because the bounded numeric snapshot cache is memory-only.
        for split in [1, 3] {
            let mut server = Server::start(true, true, true);
            let mut first: Value = serde_json::from_str(payload).unwrap();
            let mut rest = first.clone();
            let points = first["resourceMetrics"][0]["scopeMetrics"][0]["metrics"][0]["sum"]
                ["dataPoints"]
                .as_array_mut()
                .unwrap();
            let remaining = points.split_off(split.min(points.len() - 1));
            rest["resourceMetrics"][0]["scopeMetrics"][0]["metrics"][0]["sum"]["dataPoints"] =
                json!(remaining);
            server
                .post("/v1/metrics", &first.to_string(), Some("test-otlp-token"))
                .unwrap();
            let checkpoint = server.root.join("store/stores/agents/otlp-counters.json");
            assert!(checkpoint.exists());
            assert!(!checkpoint.with_extension("json.tmp").exists());
            server.restart();
            server
                .post("/v1/metrics", &rest.to_string(), Some("test-otlp-token"))
                .unwrap();
            let sql = format!("SELECT max(\"claude_code.token.usage.input.model.claude-sonnet@last\") - min(\"claude_code.token.usage.input.model.claude-sonnet@last\") AS tokens FROM telemetry_agents WHERE vessel = 'agent.urn:{mode}'");
            assert_eq!(
                server.sql(&sql)["rows"][0]["tokens"],
                expected,
                "{mode}, split {split}"
            );
        }
    }
}
#[test]
fn corrupt_checkpoint_fails_startup_without_silent_reset() {
    let server = Server::start(true, true, true);
    server
        .post(
            "/v1/metrics",
            include_str!("golden/otlp/delta.json"),
            Some("test-otlp-token"),
        )
        .unwrap();
    let checkpoint = server.root.join("store/stores/agents/otlp-counters.json");
    std::fs::write(&checkpoint, "{corrupt").unwrap();
    // The currently running receiver owns the store, so stop it before reopening.
    let mut server = server;
    server.child.kill().unwrap();
    server.child.wait().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_lume"))
        .args(["ti", "otlp", "--store"])
        .arg(server.root.join("store"))
        .args(["--port", "0"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("Invalid OTLP counters file"), "{error}");
    assert!(error.contains("refusing to reset totals"), "{error}");
    assert_eq!(std::fs::read_to_string(checkpoint).unwrap(), "{corrupt");
}
fn log_with_marker(marker: &str, seconds: i64) -> String {
    json!({"resourceLogs":[{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":"kill-agent"}}]},
        "scopeLogs":[{"logRecords":[{
            "timeUnixNano": format!("{seconds}000000000"),
            "eventName": marker,
            "body": {"stringValue": marker}
        }]}]}]}).to_string()
}
#[test]
fn lone_otlp_post_is_not_held_for_a_batch_window() {
    let server = Server::start(true, true, false);
    let started = Instant::now();
    server
        .post(
            "/v1/logs",
            &log_with_marker("lone-post", 1_577_836_811),
            None,
        )
        .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "lone POST waited {:?}",
        started.elapsed()
    );
    assert_eq!(
        server.sql("SELECT title FROM docs WHERE title = 'lone-post'")["row_count"],
        1
    );
}
#[test]
fn acked_logs_survive_kill_before_the_next_group() {
    let mut server = Server::start(true, true, false);
    let url = server.url.clone();
    let pid = server.child.id();
    let acked = Arc::new(Mutex::new(Vec::new()));
    let killed = Arc::new(AtomicBool::new(false));
    let barrier = Arc::new(Barrier::new(16));
    let mut handles = Vec::new();
    for i in 0..16 {
        let url = url.clone();
        let acked = Arc::clone(&acked);
        let killed = Arc::clone(&killed);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            let marker = format!("kill-marker-{i}");
            let body = log_with_marker(&marker, 1_577_836_820 + i);
            let posted = ureq::post(&format!("{url}/v1/logs"))
                .set("Content-Type", "application/json")
                .set("Accept", "application/json")
                .timeout(Duration::from_secs(30))
                .send_string(&body);
            if posted.is_ok() {
                acked.lock().unwrap().push(marker);
                if !killed.swap(true, Ordering::SeqCst) {
                    let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
                }
            }
        }));
    }
    for handle in handles {
        handle.join().unwrap();
    }
    let _ = server.child.kill();
    let _ = server.child.wait();
    let acked = acked.lock().unwrap().clone();
    assert!(
        !acked.is_empty(),
        "expected at least one acknowledged batch before the kill"
    );
    let docs = ti_store::DocStore::open(&server.root.join("store")).unwrap();
    let titles: Vec<String> = docs.iter().map(|doc| doc.title.clone()).collect();
    for marker in &acked {
        assert!(
            titles.iter().any(|title| title == marker),
            "acked {marker} missing after kill; have {titles:?}"
        );
    }
}
