#![cfg(feature = "ti")]
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::Duration,
};
const METRICS: &str = include_str!("golden/otlp/metrics.json");
const LOGS: &str = include_str!("golden/otlp/logs.json");
struct Server {
    child: Child,
    root: PathBuf,
    url: String,
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
        let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
            "otlp-{}-{}-{}",
            std::process::id(),
            standalone,
            auth
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
        };
        let mut output = BufReader::new(server.child.stdout.take().unwrap());
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        server.url = line.split_whitespace().last().unwrap().to_string();
        assert!(server.url.starts_with("http://127.0.0.1:"), "{line}");
        server
    }
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
    let metadata = ureq::get(&format!("{}/ti/schema", server.url))
        .call()
        .unwrap()
        .into_json::<Value>()
        .unwrap();
    assert!(metadata.to_string().contains("{token}"));
}
