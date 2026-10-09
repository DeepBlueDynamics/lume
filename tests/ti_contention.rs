#![cfg(feature = "ti")]
use chrono::Utc;
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader},
    net::TcpListener,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};
use ti_contracts::{
    Agg, BucketRecord, Catalog, FieldKind, FieldSpec, FieldValue, ShardSink, VesselSpec,
};

struct Servers {
    child: Child,
    root: PathBuf,
    stop: Arc<AtomicBool>,
    ws: Option<thread::JoinHandle<()>>,
}
impl Drop for Servers {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(ws) = self.ws.take() {
            ws.join().unwrap();
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
#[test]
fn ten_seconds_baseline_and_load_against_real_ti_and_fake_signalk() {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("contention-{}", std::process::id()));
    let store_root = root.join("store");
    let mut store = ti_store::Store::open_or_create(&store_root, 10).unwrap();
    let vessel = store
        .catalog()
        .register_vessel(&VesselSpec {
            urn: "vessels.urn:mrn:imo:mmsi:11265".into(),
            name: None,
            mmsi: None,
        })
        .unwrap();
    let background = store
        .catalog()
        .register_vessel(&VesselSpec {
            urn: "vessels.urn:mrn:imo:mmsi:1".into(),
            name: None,
            mmsi: None,
        })
        .unwrap();
    let field = store
        .catalog()
        .register_field(&FieldSpec {
            id: 0,
            path: "electrical.batteries.house.voltage".into(),
            agg: Some(Agg::Mean),
            kind: FieldKind::Bsi { scale: 3 },
            units: Some("V".into()),
        })
        .unwrap();
    let ts = chrono::DateTime::parse_from_rfc3339("2026-10-06T12:00:00Z")
        .unwrap()
        .timestamp();
    let bucket = ti_contracts::bucket_of(ts, 10).unwrap();
    let mut records: Vec<_> = (0..3)
        .map(|offset| BucketRecord {
            vessel,
            bucket: bucket + offset,
            field,
            value: FieldValue::Int(12500),
            rewrite: false,
        })
        .collect();
    records.push(BucketRecord {
        vessel: background,
        bucket,
        field,
        value: FieldValue::Int(12000),
        rewrite: false,
    });
    store.apply(&records).unwrap();
    store.flush_shards().unwrap();
    store.shutdown().unwrap();
    drop(store);
    std::fs::write(
        store_root.join("ti.toml"),
        "width_seconds=10\n[query]\nwarm_on_open=false\n",
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_lume"))
        .args(["serve", "--port", "0", "--ti-store"])
        .arg(&store_root)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    let ti_url = line.split_whitespace().last().unwrap().to_string();
    assert!(ti_url.starts_with("http://127.0.0.1:"));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let signalk = format!(
        "ws://{}/signalk/v1/stream?subscribe=self",
        listener.local_addr().unwrap()
    );
    listener.set_nonblocking(true).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_ws = stop.clone();
    let ws = thread::spawn(move || {
        let stream = loop {
            if stop_ws.load(Ordering::Relaxed) {
                return;
            }
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10))
                }
                Err(error) => panic!("{error}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        // Tungstenite fixes this callback's error type to its large HTTP response.
        #[allow(clippy::result_large_err)]
        let mut ws = tungstenite::accept_hdr(
            stream,
            |request: &tungstenite::handshake::server::Request, response| {
                assert_eq!(request.headers()["Authorization"], "Bearer fixture-secret");
                assert_eq!(request.uri().query(), Some("subscribe=self"));
                Ok(response)
            },
        )
        .unwrap();
        while !stop_ws.load(Ordering::Relaxed) {
            let delta = json!({"context":"vessels.self","updates":[{"$source":"mock",
                "timestamp":Utc::now().to_rfc3339(),
                "values":[{"path":"navigation.speedOverGround","value":2.0}]}]});
            if ws
                .send(tungstenite::Message::Text(delta.to_string()))
                .is_err()
            {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
    });
    let servers = Servers {
        child,
        root: root.clone(),
        stop,
        ws: Some(ws),
    };
    let token = root.join("token");
    std::fs::write(&token, "fixture-secret\n").unwrap();
    let out = root.join("result.json");
    let empty = ti_bench::contention::run(&[
        "--ti-url".into(),
        ti_url.clone(),
        "--queries".into(),
        "q4-002".into(),
        "--window".into(),
        "2026-10-01/2026-10-02".into(),
        "--out".into(),
        out.to_string_lossy().into(),
    ])
    .unwrap_err();
    assert!(empty.starts_with("EMPTY_RESULT:"), "{empty}");
    assert!(!out.exists());
    ti_bench::contention::run(&[
        "--ti-url".into(),
        ti_url,
        "--signalk".into(),
        signalk,
        "--duration".into(),
        "8".into(),
        "--baseline-secs".into(),
        "2".into(),
        "--queries".into(),
        "q4-002,q8-001".into(),
        "--concurrency".into(),
        "2".into(),
        "--token-file".into(),
        token.to_string_lossy().into(),
        "--out".into(),
        out.to_string_lossy().into(),
    ])
    .unwrap();
    let report: Value = serde_json::from_slice(&std::fs::read(out).unwrap()).unwrap();
    assert_eq!(
        report["resolved"]["vessel"],
        "vessels.urn:mrn:imo:mmsi:11265"
    );
    assert_eq!(
        report["resolved"]["window"]["end"],
        "2026-10-06T12:00:30+00:00"
    );
    assert_eq!(report["resolved"]["preflight"]["q4-002"]["rows"], 1);
    assert_eq!(report["baseline_seconds"], 2);
    assert_eq!(report["load_seconds"], 8);
    assert_eq!(report["concurrency"], 2);
    assert!(report["elapsed_seconds"].as_f64().unwrap() >= 10.0);
    for phase in ["baseline", "with_load"] {
        assert!(report[phase]["values"].as_u64().unwrap() > 10);
        let paths = report[phase]["paths"].as_object().unwrap();
        let p = &paths["vessels.self|mock|navigation.speedOverGround"];
        assert!(p["gaps_ms"]["p50"].as_f64().unwrap() > 0.0);
        assert!(p["timestamp_latency_ms"]["n"].as_u64().unwrap() > 0);
    }
    for q in report["queries"].as_array().unwrap() {
        assert!(q["attempts"].as_u64().unwrap() > 1);
        assert!(q["latency_ms"]["p95"].as_f64().unwrap() > 0.0);
        // Only an in-flight request clipped at the measurement deadline may time out.
        assert!(q["errors"].as_u64().unwrap() <= 2, "{q}");
    }
    assert!(!report.to_string().contains("fixture-secret"));
    assert!(!report["comparison"].as_object().unwrap().is_empty());
    drop(servers);
}
