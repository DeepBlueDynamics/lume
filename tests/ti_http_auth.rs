#![cfg(feature = "ti")]
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{Shutdown, TcpStream},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::Duration,
};

struct Server {
    child: Child,
    root: PathBuf,
    address: String,
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
impl Server {
    fn start(mode: &str, http_auth: bool, route_auth: bool) -> Self {
        let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("http-auth-{}", lume::uuid_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let store_root = root.join("store");
        drop(ti_store::Store::open_or_create(&store_root, 10).unwrap());
        let mut config = "width_seconds=10\n[query]\nwarm_on_open=false\n".to_string();
        if route_auth {
            let sync_file = root.join("sync-token");
            std::fs::write(&sync_file, "sync-secret\n").unwrap();
            config.push_str(&format!("\n[sync]\ntoken_file='{}'\n", sync_file.display()));
        }
        std::fs::write(store_root.join("ti.toml"), config).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_lume"));
        match mode {
            "plain" => {
                command.arg("serve");
            }
            "ti" => {
                command
                    .args(["serve", "--ti-store"])
                    .arg(&store_root)
                    .arg("--otlp");
            }
            "ingest" => {
                command
                    .args(["ti", "ingest", "--store"])
                    .arg(&store_root)
                    .args(["--signalk", "ws://127.0.0.1:9", "--serve", "--otlp"]);
            }
            _ => panic!("unknown mode"),
        }
        command
            .args(["--bind", "127.0.0.1", "--port", "0"])
            .env("LUME_TI_SELF_TELEMETRY", "0");
        if http_auth {
            let file = root.join("http-token");
            std::fs::write(&file, " \nhttp-secret\r\n").unwrap();
            command.arg("--http-token-file").arg(file);
        }
        if route_auth {
            let file = root.join("otlp-token");
            std::fs::write(&file, "otlp-secret\n").unwrap();
            command.arg("--otlp-token-file").arg(file);
        }
        let child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let mut server = Self {
            child,
            root,
            address: String::new(),
        };
        let output = server.child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(output).lines().map_while(Result::ok) {
                if let Some(address) =
                    line.strip_prefix("Lume MCP HTTP server listening on http://")
                {
                    let _ = tx.send(address.to_string());
                }
            }
        });
        server.address = rx
            .recv_timeout(Duration::from_secs(30))
            .expect("server startup");
        assert!(server.address.starts_with("127.0.0.1:"));
        server
    }

    fn request(&self, method: &str, path: &str, body: &str, token: Option<&str>) -> String {
        let mut stream = TcpStream::connect(&self.address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let auth = token
            .map(|token| format!("Authorization: Bearer {token}\r\n"))
            .unwrap_or_default();
        write!(stream, "{method} {path} HTTP/1.1\r\nHost: localhost\r\nAccept: application/json\r\nContent-Type: application/json\r\n{auth}Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        stream.shutdown(Shutdown::Write).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }
}
fn status(response: &str, expected: u16) {
    assert!(
        response.starts_with(&format!("HTTP/1.1 {expected} ")),
        "{response}"
    );
    if expected == 401 {
        assert_eq!(response.split_once("\r\n\r\n").unwrap().1, "");
        assert!(response.contains("Content-Length: 0\r\n"));
    }
}
const MCP: &str = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
const QUERY: &str = r#"{"sql":"SELECT 42 AS answer"}"#;
const LOGS: &str = r#"{"resourceLogs":[]}"#;
const METRICS: &str = r#"{"resourceMetrics":[]}"#;

#[test]
fn serve_and_ingest_authenticate_ti_mcp_sse_and_otlp_before_dispatch() {
    for mode in ["ti", "ingest"] {
        let server = Server::start(mode, true, false);
        for (method, path, body) in [
            ("POST", "/ti/query", QUERY),
            ("GET", "/ti/status", ""),
            ("POST", "/mcp", MCP),
            ("POST", "/message", MCP),
            ("GET", "/sse", ""),
            ("POST", "/v1/logs", LOGS),
            ("POST", "/v1/metrics", METRICS),
            ("GET", "/ti/manifest", ""),
        ] {
            for token in [None, Some("wrong"), Some("http-secret-extra")] {
                status(&server.request(method, path, body, token), 401);
            }
            status(
                &server.request(method, path, body, Some("http-secret")),
                200,
            );
        }
        // Reject on headers alone without waiting for the claimed request body.
        let mut pending = TcpStream::connect(&server.address).unwrap();
        pending
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        pending
            .write_all(b"POST /mcp HTTP/1.1\r\nContent-Length: 8388609\r\n\r\n")
            .unwrap();
        let mut reply = String::new();
        pending.read_to_string(&mut reply).unwrap();
        status(&reply, 401);
        // Method/path/body errors cannot disclose anything before authorization.
        status(&server.request("POST", "/ti/query", "{", None), 401);
        status(&server.request("OPTIONS", "/ti/query", "", None), 401);
        status(&server.request("GET", "/unknown", "", None), 401);
        status(
            &server.request("GET", "/unknown", "", Some("http-secret")),
            404,
        );
    }
}

#[test]
fn three_distinct_credentials_are_scoped_to_their_own_routes() {
    let server = Server::start("ti", true, true);
    for (method, path, body, required) in [
        ("POST", "/ti/query", QUERY, "http-secret"),
        ("GET", "/ti/status", "", "http-secret"),
        ("POST", "/mcp", MCP, "http-secret"),
        ("GET", "/sse", "", "http-secret"),
        ("GET", "/ti/manifest", "", "sync-secret"),
        (
            "GET",
            "/ti/shards/vessels.urn%3Atest%3Ahttp/0/1/status",
            "",
            "sync-secret",
        ),
        ("POST", "/v1/logs", LOGS, "otlp-secret"),
        ("POST", "/v1/metrics", METRICS, "otlp-secret"),
    ] {
        status(&server.request(method, path, body, None), 401);
        for token in ["http-secret", "sync-secret", "otlp-secret"] {
            status(
                &server.request(method, path, body, Some(token)),
                if token == required { 200 } else { 401 },
            );
        }
    }
}

#[test]
fn absence_of_flag_preserves_plain_and_ti_access_and_route_auth() {
    let plain = Server::start("plain", false, false);
    status(&plain.request("POST", "/mcp", MCP, None), 200);
    let ti = Server::start("ti", false, true);
    for (method, path, body) in [
        ("POST", "/ti/query", QUERY),
        ("GET", "/ti/status", ""),
        ("POST", "/mcp", MCP),
        ("GET", "/sse", ""),
    ] {
        status(&ti.request(method, path, body, None), 200);
    }
    status(
        &ti.request("POST", "/v1/logs", LOGS, Some("otlp-secret")),
        200,
    );
    status(
        &ti.request("GET", "/ti/manifest", "", Some("sync-secret")),
        200,
    );
    assert!(ti
        .request("POST", "/v1/logs", LOGS, None)
        .starts_with("HTTP/1.1 401"));
}

#[test]
fn plain_serve_also_supports_the_opt_in_flag() {
    let server = Server::start("plain", true, false);
    for path in ["/mcp", "/message"] {
        status(&server.request("POST", path, MCP, None), 401);
        status(&server.request("POST", path, MCP, Some("wrong")), 401);
        status(&server.request("POST", path, MCP, Some("http-secret")), 200);
    }
}

#[test]
fn invalid_token_files_and_ingest_without_serve_fail_before_startup() {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("http-auth-errors-{}", lume::uuid_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let file = root.join("token");
    std::fs::write(&file, " \n\t").unwrap();
    for args in [
        vec!["serve", "--http-token-file", file.to_str().unwrap()],
        vec!["serve", "--http-token-file"],
        vec!["ti", "ingest", "--http-token-file", file.to_str().unwrap()],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_lume"))
            .args(args)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!String::from_utf8_lossy(&output.stdout).contains("listening"));
    }
    std::fs::remove_dir_all(root).unwrap();
}
