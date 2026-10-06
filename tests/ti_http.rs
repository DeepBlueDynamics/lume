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
    fn start() -> Self {
        let root =
            PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("http-{}", std::process::id()));
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
        store
            .seal(ti_contracts::ShardKey { vessel, shard: 0 })
            .unwrap();
        store.shutdown().unwrap();
        drop(store);
        let child = Command::new(env!("CARGO_BIN_EXE_lume"))
            .args(["serve", "--port", "0", "--ti-store"])
            .arg(&store_root)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let mut server = Self {
            child,
            root,
            url: String::new(),
        };
        let mut line = String::new();
        BufReader::new(server.child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let address = line.split_whitespace().last().expect("server startup line");
        assert!(address.starts_with("http://0.0.0.0:"), "{line}");
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
    ) -> Result<ureq::Response, ureq::Error> {
        let req = ureq::post(&format!("{}{path}", self.url)).timeout(Duration::from_secs(10));
        let req = if json_reply {
            req.set("Accept", "application/json")
        } else {
            req
        };
        req.send_json(args)
    }
}
#[test]
fn http_shared_engine_arrow_json_schema_explain_status_and_read_only() {
    let server = Server::start();
    let sql = "SELECT \"navigation.speedOverGround\" FROM telemetry ORDER BY ts";
    let response = server.post("/ti/query", json!({"sql":sql}), false).unwrap();
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
    match server.post("/ti/query", json!({"sql":"DELETE FROM telemetry"}), true) {
        Err(ureq::Error::Status(400, response)) => {
            let error: Value = response.into_json().unwrap();
            assert!(error["error"].is_string());
        }
        other => panic!("Expected read-only HTTP 400: {other:?}"),
    }
    match server.post("/ti/query", json!({"sql":sql,"max_rows":0}), false) {
        Err(ureq::Error::Status(400, _)) => {}
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
    std::fs::rename(hidden, store).unwrap();
}
