#![cfg(feature = "ti")]

use serde_json::{json, Value};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use tungstenite::Message;

struct ProcessGuard {
    child: Child,
    root: PathBuf,
}

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn test_ingest_serve_live_queries_grow_without_restart() {
    let base = std::env::var_os("CARGO_TARGET_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let root = base.join(format!("ti-live-serve-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let store_root = root.join("store");
    std::fs::create_dir_all(&store_root).unwrap();
    let store_config = "width_seconds=10\n[signal_k]\nurl='ws://127.0.0.1:29999'\n";
    std::fs::write(store_root.join("ti.toml"), store_config).unwrap();
    let auth_path = root.join("pg-auth.toml");
    // The production Node-derived verifier is independently cross-tested in ti_http.
    std::fs::write(&auth_path, "width_seconds=0\n[[auth.scram_users]]\nusername='grafana'\nverifier='SCRAM-SHA-256$4096:W22ZaJ0SNY7soEsUEjb6gQ==$WG5d8oPm3OtcPnkdi4Uo7BkeZkBFzpcXkuLmtbsT4qY=:wfPLwcE6nTWhTAmQ7tl2KeoiWGPlZqQxSrmfPwDl2dU='\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&auth_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let pg_listener = TcpListener::bind("127.0.0.2:0").unwrap();
    let pg_port = pg_listener.local_addr().unwrap().port();
    drop(pg_listener);

    // 1. Mock Signal K WebSocket server
    let ws_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let ws_port = ws_listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel::<String>();

    let ws_thread = thread::spawn(move || {
        let (stream, _) = ws_listener.accept().unwrap();
        let mut ws = tungstenite::accept(stream).unwrap();

        // Read two subscription messages
        let _ = ws.read().unwrap();
        let _ = ws.read().unwrap();

        // Send hello message with self URN
        let hello = serde_json::json!({
            "name": "signalk-mock",
            "version": "1.0.0",
            "self": "urn:mrn:imo:mmsi:367000000"
        });
        ws.send(Message::Text(hello.to_string())).unwrap();

        // Stream deltas received over channel
        while let Ok(msg) = rx.recv() {
            if ws.send(Message::Text(msg)).is_err() {
                break;
            }
        }
    });

    // 2. Select an available port for the query server
    let serve_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let serve_port = serve_listener.local_addr().unwrap().port();
    drop(serve_listener);

    // 3. Start `lume ti ingest --serve` as a child process against the empty store
    let child = Command::new(env!("CARGO_BIN_EXE_lume"))
        .args([
            "ti",
            "ingest",
            "--signalk",
            &format!("ws://127.0.0.1:{ws_port}"),
            "--store",
            store_root.to_str().unwrap(),
            "--serve",
            "--port",
            &serve_port.to_string(),
            "--bind",
            "127.0.0.1",
            "--pg",
            &pg_port.to_string(),
            "--pg-bind",
            "127.0.0.2",
            "--pg-auth-config",
            auth_path.to_str().unwrap(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn lume ti ingest --serve");

    let mut guard = ProcessGuard {
        child,
        root: root.clone(),
    };

    let query_url = format!("http://127.0.0.1:{serve_port}/ti/query");

    // 4. Wait for server to come up and verify empty store queries return 0
    let mut ready = false;
    for _ in 0..50 {
        if let Ok(res) = ureq::post(&query_url)
            .set("Accept", "application/json")
            .timeout(Duration::from_millis(500))
            .send_json(json!({"sql": "SELECT count(*) FROM telemetry"}))
        {
            if res.status() == 200 {
                ready = true;
                break;
            }
        }
        thread::sleep(Duration::from_millis(100));
    }
    assert!(
        ready,
        "lume ti ingest --serve server failed to become ready"
    );

    ti_sql::surface_runtime().unwrap().block_on(async {
        let mut config = tokio_postgres::Config::new();
        config
            .host("127.0.0.2")
            .port(pg_port)
            .user("grafana")
            .password("pencil")
            .dbname("ti");
        let (client, connection) = config.connect(tokio_postgres::NoTls).await.unwrap();
        let task = tokio::spawn(connection);
        assert_eq!(
            client
                .query_one("SELECT 42::BIGINT", &[])
                .await
                .unwrap()
                .get::<_, i64>(0),
            42
        );
        drop(client);
        task.await.unwrap().unwrap();
    });
    assert_eq!(
        std::fs::read_to_string(store_root.join("ti.toml")).unwrap(),
        store_config
    );

    // Helper to query row count
    let query_count = || -> u64 {
        match ureq::post(&query_url)
            .set("Accept", "application/json")
            .timeout(Duration::from_secs(2))
            .send_json(json!({"sql": "SELECT count(*) FROM telemetry"}))
        {
            Ok(res) => {
                let val: Value = res.into_json().expect("json parse");
                if let Some(rows) = val["rows"].as_array() {
                    if let Some(first_row) = rows.first() {
                        if let Some(obj) = first_row.as_object() {
                            if let Some(num) = obj.values().next().and_then(Value::as_u64) {
                                return num;
                            }
                        }
                    }
                }
                0
            }
            Err(ureq::Error::Status(code, res)) => {
                let body = res.into_string().unwrap_or_default();
                panic!("query failed with {code}: {body}");
            }
            Err(e) => panic!("query request error: {e}"),
        }
    };

    let initial_count = query_count();
    assert_eq!(
        initial_count, 0,
        "Initial row count on empty store must be 0"
    );

    // 5. Send Delta 1 and Delta 2 to close the first bucket
    let now = chrono::Utc::now();
    let d1 = serde_json::json!({
        "context": "vessels.self",
        "updates": [{
            "$source": "n2k.115",
            "timestamp": now.to_rfc3339(),
            "values": [
                { "path": "navigation.speedOverGround", "value": 5.5 }
            ]
        }]
    });
    tx.send(d1.to_string()).unwrap();

    // Delta 2 arrives 45s later (within 300s skew window), watermark advances to now + 15s,
    // closing the bucket for `now` (end time = now + 10s).
    let d2 = serde_json::json!({
        "context": "vessels.self",
        "updates": [{
            "$source": "n2k.115",
            "timestamp": (now + chrono::Duration::seconds(45)).to_rfc3339(),
            "values": [
                { "path": "navigation.speedOverGround", "value": 6.0 }
            ]
        }]
    });
    tx.send(d2.to_string()).unwrap();

    // 6. Poll /ti/query without restarting server; assert count grows to 1
    let mut count_after_first = 0;
    for _ in 0..100 {
        count_after_first = query_count();
        if count_after_first >= 1 {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(
        count_after_first, 1,
        "Query count must grow to 1 after first bucket closes without server restart"
    );

    // Wait at least 5s so debounce window clears before sending delta 3
    thread::sleep(Duration::from_millis(5100));

    // 7. Send Delta 3 at now + 90s, advancing watermark to now + 60s,
    // closing the second bucket (now + 45s, end time = now + 50s).
    let d3 = serde_json::json!({
        "context": "vessels.self",
        "updates": [{
            "$source": "n2k.115",
            "timestamp": (now + chrono::Duration::seconds(90)).to_rfc3339(),
            "values": [
                { "path": "navigation.speedOverGround", "value": 7.0 }
            ]
        }]
    });
    tx.send(d3.to_string()).unwrap();

    // 8. Poll /ti/query without restarting server; assert count grows to 2
    let mut count_after_second = 0;
    for _ in 0..100 {
        count_after_second = query_count();
        if count_after_second >= 2 {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(
        count_after_second, 2,
        "Query count must grow to 2 after second bucket closes without server restart"
    );

    // Also assert that SELECT count(*) query works
    let count_star_res: Value = ureq::post(&query_url)
        .set("Accept", "application/json")
        .timeout(Duration::from_secs(2))
        .send_json(json!({"sql": "SELECT count(*) FROM telemetry"}))
        .expect("count(*) query")
        .into_json()
        .unwrap();
    assert_eq!(count_star_res["row_count"], 1);

    // Cleanup
    drop(tx);
    let _ = ws_thread.join();
    let _ = guard.child.kill();
    let _ = guard.child.wait();
}
