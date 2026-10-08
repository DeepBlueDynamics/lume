#![cfg(feature = "ti")]
// PUBLIC TEST RSA KEY: tests/fixtures/nuts-test-rsa.json is disposable test material.
use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
    Engine,
};
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc, Arc, Mutex,
    },
    thread::JoinHandle,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/nuts-test-rsa.json")).unwrap()
}
fn time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
fn claims(scopes: &[&str]) -> Value {
    json!({"sub":"sailor@example.test","user_id":"user-17","scopes":scopes,"iat":time()-1000,"exp":time()+1800})
}
fn jwt(claims: Value, kid: Option<&str>) -> String {
    let fixture = fixture();
    let key = ring::signature::RsaKeyPair::from_pkcs8(
        &STANDARD.decode(fixture["pkcs8"].as_str().unwrap()).unwrap(),
    )
    .unwrap();
    let mut header = json!({"alg":"RS256","typ":"JWT"});
    if let Some(kid) = kid {
        header["kid"] = json!(kid);
    }
    let message = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(header.to_string()),
        URL_SAFE_NO_PAD.encode(claims.to_string())
    );
    let mut signature = vec![0; key.public().modulus_len()];
    key.sign(
        &ring::signature::RSA_PKCS1_SHA256,
        &ring::rand::SystemRandom::new(),
        message.as_bytes(),
        &mut signature,
    )
    .unwrap();
    format!("{message}.{}", URL_SAFE_NO_PAD.encode(signature))
}
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("nuts-auth-{}", lume::uuid_v4()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Authority {
    url: String,
    stop: Arc<AtomicBool>,
    exchanges: Arc<AtomicUsize>,
    reply: Arc<Mutex<String>>,
    jwks_ok: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl Authority {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let exchanges = Arc::new(AtomicUsize::new(0));
        let reply = Arc::new(Mutex::new(jwt(claims(&["read", "write"]), None)));
        let jwks_ok = Arc::new(AtomicBool::new(true));
        let (done, count, token, online) = (
            stop.clone(),
            exchanges.clone(),
            reply.clone(),
            jwks_ok.clone(),
        );
        let thread = std::thread::spawn(move || {
            while !done.load(Ordering::SeqCst) {
                let Ok((mut stream, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0; 8192];
                let (headers, body_start) = loop {
                    let n = stream.read(&mut buffer).unwrap_or(0);
                    if n == 0 {
                        break (String::new(), 0);
                    }
                    bytes.extend_from_slice(&buffer[..n]);
                    if let Some(i) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                        break (String::from_utf8_lossy(&bytes[..i]).to_string(), i + 4);
                    }
                    if bytes.len() > 16384 {
                        break (String::new(), 0);
                    }
                };
                let length = headers
                    .lines()
                    .filter_map(|line| line.split_once(':'))
                    .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                    .and_then(|(_, v)| v.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                while bytes.len() < body_start + length && length <= 8192 {
                    let n = stream.read(&mut buffer).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buffer[..n]);
                }
                let (status, value) = if headers.starts_with("GET /.well-known/jwks.json ") {
                    if online.load(Ordering::SeqCst) {
                        let key = fixture();
                        (
                            200,
                            json!({"keys":[{"alg":"RS256","kty":"RSA","kid":"nuts-auth-key-1","use":"sig","n":key["n"],"e":key["e"]}]}),
                        )
                    } else {
                        (503, json!({}))
                    }
                } else if headers.starts_with("POST /auth ") {
                    count.fetch_add(1, Ordering::SeqCst);
                    let body = String::from_utf8_lossy(bytes.get(body_start..).unwrap_or_default());
                    if body == "token=ahp_test_secret"
                        && headers
                            .to_ascii_lowercase()
                            .contains("content-type: application/x-www-form-urlencoded")
                    {
                        (
                            200,
                            json!({"access_token":token.lock().unwrap().clone(),"token_type":"Bearer","expires_in":1800}),
                        )
                    } else {
                        (401, json!({"detail":"private-upstream-detail"}))
                    }
                } else {
                    (404, json!({}))
                };
                let body = value.to_string();
                let _ = write!(stream,"HTTP/1.1 {status} Stub\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
            }
        });
        Self {
            url,
            stop,
            exchanges,
            reply,
            jwks_ok,
            thread: Some(thread),
        }
    }
    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}
impl Drop for Authority {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct Server {
    child: Child,
    address: String,
    logs: Arc<Mutex<String>>,
    readers: Vec<JoinHandle<()>>,
}
impl Server {
    fn start(root: &Path, authority: &str, mode: &str, allow: &str, route_tokens: bool) -> Self {
        let store = root.join("store");
        drop(ti_store::Store::open_or_create(&store, 10).unwrap());
        let mut config = "width_seconds=10\n[query]\nwarm_on_open=false\n".to_string();
        if route_tokens {
            let path = root.join("sync-token");
            std::fs::write(&path, "sync-test-secret").unwrap();
            config.push_str(&format!("[sync]\ntoken_file='{}'\n", path.display()));
        }
        std::fs::write(store.join("ti.toml"), config).unwrap();
        let allow_file = root.join("allow.txt");
        std::fs::write(&allow_file, allow).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_lume"));
        command.current_dir(root);
        match mode {
            "plain" => {
                command.arg("serve");
            }
            "ti" => {
                command
                    .args(["serve", "--ti-store"])
                    .arg(&store)
                    .arg("--otlp");
            }
            "ingest" => {
                command.args(["ti", "ingest", "--store"]).arg(&store).args([
                    "--signalk",
                    "ws://127.0.0.1:9",
                    "--serve",
                    "--otlp",
                ]);
            }
            _ => panic!("invalid mode"),
        }
        if route_tokens {
            let path = root.join("otlp-token");
            std::fs::write(&path, "otlp-test-secret").unwrap();
            command.arg("--otlp-token-file").arg(path);
        }
        let mut child = command
            .args([
                "--bind",
                "127.0.0.1",
                "--port",
                "0",
                "--nuts-auth",
                authority,
                "--nuts-allow",
            ])
            .arg(format!("@{}", allow_file.display()))
            .env("LUME_TI_SELF_TELEMETRY", "0")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let logs = Arc::new(Mutex::new(String::new()));
        let (tx, rx) = mpsc::channel();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let out_logs = logs.clone();
        let err_logs = logs.clone();
        let readers = vec![
            std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if let Some(address) =
                        line.strip_prefix("Lume MCP HTTP server listening on http://")
                    {
                        let _ = tx.send(address.to_string());
                    }
                    out_logs.lock().unwrap().push_str(&format!("{line}\n"));
                }
            }),
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    err_logs.lock().unwrap().push_str(&format!("{line}\n"));
                }
            }),
        ];
        let mut server = Self {
            child,
            address: String::new(),
            logs,
            readers,
        };
        server.address = rx
            .recv_timeout(Duration::from_secs(30))
            .unwrap_or_else(|_| panic!("server startup failed: {}", server.logs.lock().unwrap()));
        assert!(server.address.starts_with("127.0.0.1:"));
        server
    }
    fn request(&self, method: &str, path: &str, body: &str, token: Option<&str>) -> u16 {
        let mut stream = TcpStream::connect(&self.address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let auth = token
            .map(|s| format!("Authorization: Bearer {s}\r\n"))
            .unwrap_or_default();
        write!(stream,"{method} {path} HTTP/1.1\r\nHost: localhost\r\nAccept: application/json\r\nContent-Type: application/json\r\n{auth}Content-Length: {}\r\n\r\n{body}",body.len()).unwrap();
        stream.shutdown(Shutdown::Write).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let status = response.split_whitespace().nth(1).unwrap().parse().unwrap();
        if status == 401 {
            assert_eq!(response.split_once("\r\n\r\n").unwrap().1, "");
        }
        status
    }
    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        for reader in self.readers.drain(..) {
            reader.join().unwrap();
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}
const MCP: &str = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
const INDEX: &str = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"lume_index","arguments":{}}}"#;
const QUERY: &str = r#"{"sql":"SELECT 42 AS answer"}"#;

#[test]
fn rsa_claims_scopes_and_every_entrypoint_are_checked_without_secret_logs() {
    let authority = Authority::new();
    let scratch = Scratch::new();
    let mut server = Server::start(
        &scratch.0,
        &authority.url,
        "ti",
        "sailor@example.test",
        false,
    );
    let valid = jwt(claims(&["read", "write"]), Some("nuts-auth-key-1"));
    let no_kid = jwt(claims(&["read", "write"]), None);
    let read = jwt(claims(&["read"]), None);
    let write = jwt(claims(&["write"]), None);
    let missing = jwt(claims(&[]), None);
    let mut expired = claims(&["read", "write"]);
    expired["exp"] = json!(time() - 120);
    let expired = jwt(expired, None);
    let mut unlisted = claims(&["read", "write"]);
    unlisted["sub"] = json!("other@example.test");
    unlisted["user_id"] = json!("other-user");
    let unlisted = jwt(unlisted, None);
    let wrong_kid = jwt(claims(&["read", "write"]), Some("wrong-kid"));
    let mut parts: Vec<_> = valid.split('.').map(str::to_owned).collect();
    parts[1] = URL_SAFE_NO_PAD.encode(claims(&["read"]).to_string());
    let bad_sig = parts.join(".");
    for (method, path, body) in [
        ("POST", "/ti/query", QUERY),
        ("GET", "/ti/status", ""),
        ("POST", "/mcp", MCP),
        ("POST", "/message", MCP),
        ("GET", "/sse", ""),
        ("POST", "/v1/logs", r#"{"resourceLogs":[]}"#),
    ] {
        for token in [
            None,
            Some("not-a-jwt"),
            Some(expired.as_str()),
            Some(unlisted.as_str()),
            Some(wrong_kid.as_str()),
            Some(bad_sig.as_str()),
            Some(missing.as_str()),
        ] {
            assert_eq!(
                server.request(method, path, body, token),
                401,
                "route {path}"
            );
        }
        for token in [&valid, &no_kid] {
            assert_eq!(
                server.request(method, path, body, Some(token)),
                200,
                "route {path}"
            );
        }
    }
    assert_eq!(
        server.request("POST", "/v1/logs", r#"{"resourceLogs":[]}"#, Some(&read)),
        401
    );
    assert_eq!(
        server.request("POST", "/v1/logs", r#"{"resourceLogs":[]}"#, Some(&write)),
        200
    );
    assert_eq!(server.request("GET", "/ti/status", "", Some(&write)), 401);
    assert_eq!(server.request("POST", "/mcp", INDEX, Some(&read)), 401);
    // Missing indexing arguments return a tool error, but authorization succeeds.
    assert_eq!(server.request("POST", "/mcp", INDEX, Some(&valid)), 200);
    assert_eq!(server.request("GET", "/health", "", None), 200);
    assert_eq!(server.request("GET", "/unknown", "", None), 401);
    assert_eq!(server.request("OPTIONS", "/ti/query", "", None), 401);
    assert_eq!(server.request("POST", "/ti/query", "{", None), 401);
    // Explicit 60-second expiry leeway, and future iat rejection.
    let mut leeway = claims(&["read"]);
    leeway["exp"] = json!(time() - 20);
    assert_eq!(
        server.request("GET", "/ti/status", "", Some(&jwt(leeway, None))),
        200
    );
    let mut future = claims(&["read"]);
    future["iat"] = json!(time() + 120);
    assert_eq!(
        server.request("GET", "/ti/status", "", Some(&jwt(future, None))),
        401
    );
    server.stop();
    let logs = server.logs.lock().unwrap();
    for secret in [
        &valid, &no_kid, &read, &write, &expired, &unlisted, &bad_sig, &missing,
    ] {
        assert!(!logs.contains(secret));
    }
    assert!(!logs.contains("private-upstream-detail"));
    let cache = std::fs::read_to_string(scratch.0.join("store/auth/jwks.json")).unwrap();
    assert!(!cache.contains(&valid));
}

#[test]
fn ahp_exchange_is_form_encoded_verified_cached_and_never_persisted() {
    let authority = Authority::new();
    let scratch = Scratch::new();
    let mut server = Server::start(&scratch.0, &authority.url, "ti", "user-17", false);
    // Even a successful exchange must supply a valid, allowlisted signed JWT.
    *authority.reply.lock().unwrap() = "not-a-signed-jwt".into();
    assert_eq!(
        server.request("GET", "/ti/status", "", Some("ahp_test_secret")),
        401
    );
    *authority.reply.lock().unwrap() = jwt(claims(&["read", "write"]), None);
    for _ in 0..3 {
        assert_eq!(
            server.request("GET", "/ti/status", "", Some("ahp_test_secret")),
            200
        );
    }
    assert_eq!(authority.exchanges.load(Ordering::SeqCst), 2);
    assert_eq!(
        server.request("GET", "/ti/status", "", Some("ahp_wrong_secret")),
        401
    );
    assert_eq!(authority.exchanges.load(Ordering::SeqCst), 3);
    server.stop();
    let logs = server.logs.lock().unwrap();
    assert!(!logs.contains("ahp_test_secret"));
    assert!(!logs.contains("ahp_wrong_secret"));
    assert!(!logs.contains("private-upstream-detail"));
    let cache = std::fs::read_to_string(scratch.0.join("store/auth/jwks.json")).unwrap();
    assert!(!cache.contains("ahp_"));
    assert!(!cache.contains("access_token"));
}

#[test]
fn cached_jwks_allows_offline_start_and_different_service_cache_is_rejected() {
    let mut authority = Authority::new();
    let scratch = Scratch::new();
    let valid = jwt(claims(&["read"]), None);
    let mut server = Server::start(&scratch.0, &authority.url, "ti", "user-17", false);
    server.stop();
    authority.shutdown();
    let mut offline = Server::start(&scratch.0, &authority.url, "ti", "user-17", false);
    assert_eq!(offline.request("GET", "/ti/status", "", Some(&valid)), 200);
    assert_eq!(
        offline.request("GET", "/ti/status", "", Some("ahp_test_secret")),
        401
    );
    offline.stop();
    assert!(offline
        .logs
        .lock()
        .unwrap()
        .contains("using cached public keys"));
    let other = Authority::new();
    other.jwks_ok.store(false, Ordering::SeqCst);
    let output = Command::new(env!("CARGO_BIN_EXE_lume"))
        .args([
            "serve",
            "--bind",
            "127.0.0.1",
            "--nuts-auth",
            &other.url,
            "--nuts-allow",
            "user-17",
            "--ti-store",
        ])
        .arg(scratch.0.join("store"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cache does not match auth URL"));
}

#[test]
fn plain_and_ingest_servers_use_nuts_auth_and_route_tokens_override_it() {
    let authority = Authority::new();
    let read = jwt(claims(&["read"]), None);
    for mode in ["plain", "ingest"] {
        let scratch = Scratch::new();
        let server = Server::start(&scratch.0, &authority.url, mode, "user-17", false);
        assert_eq!(server.request("POST", "/mcp", MCP, None), 401);
        assert_eq!(server.request("POST", "/mcp", MCP, Some(&read)), 200);
        assert_eq!(server.request("GET", "/health", "", None), 200);
    }
    let scratch = Scratch::new();
    let server = Server::start(&scratch.0, &authority.url, "ti", "user-17", true);
    assert_eq!(server.request("GET", "/ti/manifest", "", Some(&read)), 401);
    assert_eq!(
        server.request("GET", "/ti/manifest", "", Some("sync-test-secret")),
        200
    );
    assert_eq!(
        server.request(
            "POST",
            "/v1/logs",
            r#"{"resourceLogs":[]}"#,
            Some(&jwt(claims(&["read", "write"]), None))
        ),
        401
    );
    assert_eq!(
        server.request(
            "POST",
            "/v1/logs",
            r#"{"resourceLogs":[]}"#,
            Some("otlp-test-secret")
        ),
        200
    );
}

#[test]
fn startup_refuses_remote_no_auth_empty_allowlist_or_unavailable_uncached_jwks() {
    // A documentation-only non-loopback IP: refusal happens before any bind.
    assert!(lume::http_auth::validate_bind("192.0.2.1", false).is_err());
    assert!(lume::http_auth::validate_bind("192.0.2.1", true).is_ok());
    assert!(lume::http_auth::validate_bind("127.0.0.1", false).is_ok());
    let mut authority = Authority::new();
    authority.shutdown();
    let scratch = Scratch::new();
    for args in [
        vec![
            "serve",
            "--nuts-auth",
            authority.url.as_str(),
            "--nuts-allow",
            "user-17",
            "--bind",
            "127.0.0.1",
        ],
        vec!["serve", "--nuts-auth", "--nuts-allow", ""],
        vec!["serve", "--nuts-auth"],
        vec!["serve", "--nuts-allow", "user-17"],
        vec!["serve"], // plain default bind remains wildcard, but now needs auth.
        vec!["serve", "--bind", "192.0.2.1"],
        vec!["ti", "ingest", "--nuts-auth"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_lume"))
            .current_dir(&scratch.0)
            .args(args)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!String::from_utf8_lossy(&output.stdout).contains("listening"));
    }
}
