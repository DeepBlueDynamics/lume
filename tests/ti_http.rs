#![cfg(feature = "ti")]
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader},
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::Duration,
};
use ti_contracts::{
    Agg, BucketRecord, Catalog, FieldKind, FieldSpec, FieldValue, ShardSink, VesselSpec,
};
#[path = "support/library.rs"]
mod library_tests;
#[path = "support/pg_extended.rs"]
mod pg_extended_tests;
#[path = "support/pg.rs"]
mod pg_tests;
use library_tests::rebuild_manual;
static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
struct Server {
    child: Child,
    root: PathBuf,
    url: String,
    pg: String,
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
impl Server {
    fn start() -> Self {
        Self::start_on(None)
    }
    fn start_on(bind: Option<&str>) -> Self {
        Self::start_with(bind, false)
    }
    fn start_with(bind: Option<&str>, pg: bool) -> Self {
        Self::start_with_auth(bind, pg, None)
    }
    fn start_with_auth(bind: Option<&str>, pg: bool, verifier: Option<&str>) -> Self {
        Self::start_config(bind, pg, verifier, false)
    }
    fn start_config(bind: Option<&str>, pg: bool, verifier: Option<&str>, history: bool) -> Self {
        Self::start_config_pg(bind, pg, verifier, history, None, false)
    }
    fn start_config_pg(
        bind: Option<&str>,
        pg: bool,
        verifier: Option<&str>,
        history: bool,
        pg_bind: Option<&str>,
        external_auth: bool,
    ) -> Self {
        Self::start_with_docs(bind, pg, verifier, history, pg_bind, external_auth, false)
    }
    fn start_with_docs(
        bind: Option<&str>,
        pg: bool,
        verifier: Option<&str>,
        history: bool,
        pg_bind: Option<&str>,
        external_auth: bool,
        docs: bool,
    ) -> Self {
        let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
            "http-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let store_root = root.join("store");
        let mut store = ti_store::Store::open_or_create(&store_root, 10).unwrap();
        let vessel = store
            .catalog()
            .register_vessel(&VesselSpec {
                urn: "vessels.urn:test:http".into(),
                name: None,
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
            .apply(&[
                BucketRecord {
                    vessel,
                    bucket: 1,
                    field,
                    value: FieldValue::Int(2000),
                    rewrite: false,
                },
                BucketRecord {
                    vessel,
                    bucket: 2,
                    field,
                    value: FieldValue::Int(4000),
                    rewrite: false,
                },
            ])
            .unwrap();
        if history {
            for (path, agg, scale, values) in [
                ("navigation.speedOverGround", Agg::Min, 3, [1000, 3000]),
                ("navigation.speedOverGround", Agg::Max, 3, [3000, 5000]),
                ("navigation.speedOverGround", Agg::Last, 3, [2500, 4500]),
                (
                    "navigation.position.latitude",
                    Agg::Last,
                    6,
                    [60000000, 61000000],
                ),
                (
                    "navigation.position.longitude",
                    Agg::Last,
                    6,
                    [24000000, 25000000],
                ),
            ] {
                let field = store
                    .catalog()
                    .register_field(&FieldSpec {
                        id: 0,
                        path: path.into(),
                        agg: Some(agg),
                        kind: FieldKind::Bsi { scale },
                        units: None,
                    })
                    .unwrap();
                let records: Vec<_> = values
                    .into_iter()
                    .enumerate()
                    .map(|(i, value)| BucketRecord {
                        vessel,
                        bucket: i as u32 + 1,
                        field,
                        value: FieldValue::Int(value),
                        rewrite: false,
                    })
                    .collect();
                store.apply(&records).unwrap();
            }
        }
        store
            .seal(ti_contracts::ShardKey { vessel, shard: 0 })
            .unwrap();
        store.shutdown().unwrap();
        drop(store);
        let auth_path = root.join("pg-auth.toml");
        if let Some(verifier) = verifier {
            let text = format!(
                "width_seconds=0\n[[auth.scram_users]]\nusername='lume'\nverifier='{verifier}'\n"
            );
            if external_auth {
                std::fs::write(&auth_path, text).unwrap();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&auth_path, std::fs::Permissions::from_mode(0o600))
                        .unwrap();
                }
                // Existing settings and store user must neither be overwritten nor merged.
                std::fs::write(store_root.join("ti.toml"), format!("width_seconds=10\n[signal_k]\nurl='ws://127.0.0.1:29999'\n[[auth.scram_users]]\nusername='store-user'\nverifier='{verifier}'\n")).unwrap();
            } else {
                std::fs::write(
                    store_root.join("ti.toml"),
                    text.replace("width_seconds=0", "width_seconds=10"),
                )
                .unwrap();
            }
        }
        if docs {
            ti_store::DocStore::open(&store_root)
                .unwrap()
                .upsert_all([ti_contracts::Document {
                    id: "alert:bilge".into(),
                    vessel: "vessels.urn:test:http".into(),
                    kind: "alerts".into(),
                    ts_start: ti_contracts::EPOCH + 10,
                    ts_end: Some(ti_contracts::EPOCH + 20),
                    title: "Bilge alarm".into(),
                    body: "bilge pump alarm".into(),
                }])
                .unwrap();
            let dir = root.join("manual");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("manual.md"), "## Bilge manual\n\nA bilge pump original procedure. Inspect the inlet, strainer, float switch and discharge hose. Test operation and keep the boat safe before departure.\n").unwrap();
            rebuild_manual(&root);
        }
        let mut command = Command::new(env!("CARGO_BIN_EXE_lume"));
        command
            .args(["serve", "--port", "0", "--ti-store"])
            .arg(&store_root);
        if docs {
            command.arg("--docs-index").arg(root.join("index"));
        }
        if let Some(bind) = bind {
            command.args(["--bind", bind]);
        }
        if pg {
            command.args(["--pg", "0"]);
        }
        if let Some(pg_bind) = pg_bind {
            command.args(["--pg-bind", pg_bind]);
        }
        if external_auth {
            command.arg("--pg-auth-config").arg(&auth_path);
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
            pg: String::new(),
        };
        let mut line = String::new();
        let mut output = BufReader::new(server.child.stdout.take().unwrap());
        output.read_line(&mut line).unwrap();
        if pg {
            let address = line.split_whitespace().last().unwrap();
            assert!(
                address.starts_with(&format!("{}:", pg_bind.or(bind).unwrap_or("127.0.0.1"))),
                "{line}"
            );
            server.pg = address.replace("0.0.0.0", "127.0.0.1");
            line.clear();
            output.read_line(&mut line).unwrap();
        }
        let address = line.split_whitespace().last().expect("server startup line");
        assert!(
            address.starts_with(&format!("http://{}:", bind.unwrap_or("127.0.0.1"))),
            "{line}"
        );
        server.url = address.replace("0.0.0.0", "127.0.0.1");
        server
    }
    fn get(&self, path: &str) -> Value {
        ureq::get(&format!("{}{path}", self.url))
            .timeout(Duration::from_secs(10))
            .call()
            .unwrap()
            .into_json()
            .unwrap()
    }
    fn post(
        &self,
        path: &str,
        args: Value,
        json_reply: bool,
    ) -> Result<ureq::Response, Box<ureq::Error>> {
        let req = ureq::post(&format!("{}{path}", self.url)).timeout(Duration::from_secs(10));
        let req = if json_reply {
            req.set("Accept", "application/json")
        } else {
            req
        };
        req.send_json(args).map_err(Box::new)
    }
}
#[test]
fn history_provider_real_http() {
    let server = Server::start_config(None, false, None, true);
    let output = Command::new("node")
        .arg(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("plugins/signalk-lume-ti/test/history-real.cjs"),
        )
        .arg(&server.url)
        .output()
        .expect("Node is required for the Signal K History integration");
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
#[test]
fn explicit_bind_is_honored_and_ti_errors_have_no_cors() {
    // A non-default loopback address exercises --bind without exposing a test server
    // on every interface (which also raised Windows Firewall prompts on each build).
    let server = Server::start_on(Some("127.0.0.2"));
    assert_eq!(server.get("/ti/status")["width_seconds"], 10);
    for path in [
        "/ti/resolve",
        "/ti/resolve?q=GPS&limit=0",
        "/ti/resolve?q=%GG",
        "/ti/resolve?q=GPS&vessel=missing",
    ] {
        match ureq::get(&format!("{}{path}", server.url)).call() {
            Err(ureq::Error::Status(400, response)) => {
                assert_eq!(response.header("Access-Control-Allow-Origin"), None);
            }
            other => panic!("Expected bad request {path}: {other:?}"),
        }
    }
    let preflight = ureq::request("OPTIONS", &format!("{}/mcp", server.url))
        .set("Origin", "http://untrusted.example")
        .call()
        .unwrap();
    assert_eq!(preflight.header("Access-Control-Allow-Origin"), None);
}
#[test]
fn http_shared_engine_arrow_json_schema_explain_status_and_read_only() {
    let server = Server::start();
    let resolve = ureq::get(&format!("{}/ti/resolve?q=GPS+speed&limit=1", server.url))
        .set("Origin", "http://untrusted.example")
        .call()
        .unwrap();
    assert_eq!(resolve.header("Access-Control-Allow-Origin"), None);
    let resolved: Value = resolve.into_json().unwrap();
    assert_eq!(
        resolved["candidates"][0]["path"],
        "navigation.speedOverGround"
    );
    assert_eq!(resolved["candidates"][0]["last_value"], 4.0);
    assert_eq!(resolved["candidates"][0]["units"], "m/s");
    let encoded =
        server.get("/ti/resolve?q=speed%20over%20ground&vessel=vessels.urn%3Atest%3Ahttp");
    assert_eq!(encoded["candidates"][0]["last_value"], 4.0);
    let schema_response = ureq::get(&format!("{}/ti/schema", server.url))
        .call()
        .unwrap();
    assert_eq!(schema_response.header("Access-Control-Allow-Origin"), None);
    let preflight = ureq::request("OPTIONS", &format!("{}/ti/query", server.url))
        .set("Origin", "http://untrusted.example")
        .set("Access-Control-Request-Method", "POST")
        .call()
        .unwrap();
    assert_eq!(preflight.header("Access-Control-Allow-Origin"), None);
    assert_eq!(preflight.status(), 204);
    let sql = "SELECT \"navigation.speedOverGround\" FROM telemetry ORDER BY ts";
    let response = server.post("/ti/query", json!({"sql":sql}), false).unwrap();
    assert_eq!(response.header("Access-Control-Allow-Origin"), None);
    assert_eq!(
        response.header("Content-Type"),
        Some("application/vnd.apache.arrow.stream")
    );
    assert_eq!(response.header("X-TI-Row-Count"), Some("2"));
    let reader =
        ti_sql::arrow_ipc::reader::StreamReader::try_new(response.into_reader(), None).unwrap();
    assert_eq!(
        reader.schema().field(0).name(),
        "navigation.speedOverGround@mean"
    );
    assert_eq!(
        reader
            .schema()
            .field(0)
            .metadata()
            .get("units")
            .map(String::as_str),
        Some("m/s")
    );
    let batches = reader.collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(batches.iter().map(|b| b.num_rows()).sum::<usize>(), 2);
    let arrow_rows = ti_sql::rows_json(&batches).unwrap();
    let reply: Value = server
        .post("/ti/query", json!({"sql":sql}), true)
        .unwrap()
        .into_json()
        .unwrap();
    assert_eq!(reply["rows"], json!(arrow_rows));
    assert_eq!(reply["format"], "json");
    assert_eq!(reply["row_count"], 2);
    assert_eq!(reply["truncated"], false);
    for key in ["columns", "elapsed_ms", "pushdown", "units", "hint"] {
        assert!(reply.get(key).is_some(), "{key}");
    }
    let limited = server
        .post("/ti/query", json!({"sql":sql,"max_rows":1}), false)
        .unwrap();
    assert_eq!(limited.header("X-TI-Truncated"), Some("true"));
    let reader =
        ti_sql::arrow_ipc::reader::StreamReader::try_new(limited.into_reader(), None).unwrap();
    assert_eq!(reader.map(|b| b.unwrap().num_rows()).sum::<usize>(), 1);
    let empty = server
        .post(
            "/ti/query",
            json!({"sql":"SELECT ts FROM telemetry WHERE false"}),
            false,
        )
        .unwrap();
    let reader =
        ti_sql::arrow_ipc::reader::StreamReader::try_new(empty.into_reader(), None).unwrap();
    assert!(reader
        .schema()
        .field(0)
        .data_type()
        .to_string()
        .contains("Timestamp"));
    assert_eq!(reader.map(|b| b.unwrap().num_rows()).sum::<usize>(), 0);
    let large = server
        .post(
            "/ti/query",
            json!({"sql":"SELECT repeat('é', 40000) AS big"}),
            false,
        )
        .unwrap();
    assert_eq!(large.header("X-TI-Truncated"), Some("true"));
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut large.into_reader(), &mut bytes).unwrap();
    assert!(bytes.len() <= 65536);
    let reader =
        ti_sql::arrow_ipc::reader::StreamReader::try_new(std::io::Cursor::new(bytes), None)
            .unwrap();
    assert_eq!(reader.map(|b| b.unwrap().num_rows()).sum::<usize>(), 0);
    let schema = server.get("/ti/schema");
    assert_eq!(schema["tables"].as_array().unwrap().len(), 5);
    assert_eq!(schema["width_seconds"], 10);
    let explain: Value = server
        .post("/ti/explain", json!({"sql":sql}), true)
        .unwrap()
        .into_json()
        .unwrap();
    assert!(explain["plan"]
        .as_str()
        .unwrap()
        .contains("execution details"));
    let status = server.get("/ti/status");
    assert_eq!(status["width_seconds"], 10);
    assert!(status["shards"].is_object());
    let counters = json!({
        "samples_dropped_late": 1, "samples_dropped_nonfinite": 2,
        "samples_skipped_magnitude": 3, "samples_rejected_source": 4,
        "apply_failures": 5, "apply_retries": 6,
        "samples_rejected_blocked": 7, "ingest_blocked": true
    });
    std::fs::write(
        server.root.join("store/ingest_status.json"),
        serde_json::to_vec(&counters).unwrap(),
    )
    .unwrap();
    let status = server.get("/ti/status");
    for (name, value) in counters.as_object().unwrap() {
        assert_eq!(&status[name], value, "{name}");
    }
    match server.post("/ti/query", json!({"sql":"DELETE FROM telemetry"}), true) {
        Err(error) => match *error {
            ureq::Error::Status(400, response) => {
                assert_eq!(response.header("Access-Control-Allow-Origin"), None);
                let error: Value = response.into_json().unwrap();
                assert!(error["error"].is_string());
            }
            other => panic!("Expected read-only HTTP 400: {other:?}"),
        },
        other => panic!("Expected read-only HTTP 400: {other:?}"),
    }
    match server.post("/ti/query", json!({"sql":sql,"max_rows":0}), false) {
        Err(error) if matches!(*error, ureq::Error::Status(400, _)) => {}
        other => panic!("{other:?}"),
    }
    // Removing disk metadata after startup proves requests reuse the captured session.
    let store = server.root.join("store/catalog");
    let hidden = server.root.join("hidden-catalog");
    std::fs::rename(&store, &hidden).unwrap();
    assert_eq!(server.get("/ti/schema")["width_seconds"], 10);
    let reply: Value = server
        .post(
            "/ti/query",
            json!({"sql":"SELECT count(*) AS n FROM telemetry"}),
            true,
        )
        .unwrap()
        .into_json()
        .unwrap();
    assert_eq!(reply["rows"][0]["n"], 2);
    let mcp:Value=server.post("/mcp",json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"ti_query","arguments":{"sql":"SELECT 42 AS answer"}}}),true).unwrap().into_json().unwrap();
    assert_ne!(mcp["result"]["isError"], true);
    let result: Value =
        serde_json::from_str(mcp["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(result["rows"][0]["answer"], 42);
    let response=server.post("/mcp",json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"ti_resolve","arguments":{"phrase":"GPS speed","limit":1}}}),true).unwrap();
    assert_eq!(response.header("Access-Control-Allow-Origin"), None);
    let mcp: Value = response.into_json().unwrap();
    assert_ne!(mcp["result"]["isError"], true);
    let resolved: Value =
        serde_json::from_str(mcp["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(resolved["candidates"][0]["last_value"], 4.0);
    std::fs::rename(hidden, store).unwrap();
}

#[test]
fn ingest_serve_defaults_to_loopback() {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "ingest-serve-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&root).unwrap();
    let store_root = root.join("store");
    drop(ti_store::Store::open_or_create(&store_root, 10).unwrap());

    let mut command = Command::new(env!("CARGO_BIN_EXE_lume"));
    command.args([
        "ti",
        "ingest",
        "--store",
        store_root.to_str().unwrap(),
        "--signalk",
        "ws://127.0.0.1:59999/stream",
        "--serve",
        "--port",
        "0",
    ]);
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let mut line = String::new();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    while output.read_line(&mut line).unwrap() > 0 {
        if line.contains("server on ") || line.contains("listening on http://") {
            break;
        }
        line.clear();
    }
    assert!(
        line.contains("127.0.0.1"),
        "expected default bind under --serve to be 127.0.0.1 (loopback), got: {line}"
    );
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&root);
}
