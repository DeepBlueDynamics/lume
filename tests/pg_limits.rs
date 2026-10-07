#![cfg(feature = "ti")]

use futures::StreamExt;
use lume::ti_http::TiServer;
use lume::ti_pg;
use serde_json::Value;
use std::{io::Read, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};
use ti_contracts::{Catalog, ShardSink};

struct TestDir {
    root: PathBuf,
    store_root: PathBuf,
}

impl TestDir {
    fn new() -> Self {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
            "pg-limits-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let store_root = root.join("store");
        std::fs::create_dir_all(&store_root).unwrap();
        let mut store = ti_store::Store::open_or_create(&store_root, 10).unwrap();
        let vessel = store
            .catalog()
            .register_vessel(&ti_contracts::VesselSpec {
                urn: "vessels.urn:test:pglimits".into(),
                name: None,
                mmsi: None,
            })
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

async fn connect_pg(addr: SocketAddr) -> (tokio_postgres::Client, tokio::task::JoinHandle<()>) {
    let (client, connection) = tokio_postgres::connect(
        &format!(
            "host={} port={} user=lume dbname=ti sslmode=disable connect_timeout=10",
            addr.ip(),
            addr.port()
        ),
        tokio_postgres::NoTls,
    )
    .await
    .unwrap();
    let task = tokio::spawn(async move {
        let _ = connection.await;
    });
    (client, task)
}

struct TestHttpServer {
    url: String,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for TestHttpServer {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn start_http(server: Arc<TiServer>, bind_ip: &str) -> TestHttpServer {
    let listener = std::net::TcpListener::bind(format!("{bind_ip}:0")).unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{addr}");
    let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
    let s = server.clone();
    let thread = std::thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        while stop_rx.try_recv().is_err() {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let mut buf = vec![0u8; 65536];
                    if let Ok(n) = stream.read(&mut buf) {
                        if n > 0 {
                            let text = String::from_utf8_lossy(&buf[..n]);
                            if let Some(first_line) = text.lines().next() {
                                let parts: Vec<&str> = first_line.split_whitespace().collect();
                                if parts.len() >= 2 {
                                    let method = parts[0];
                                    let path = parts[1];
                                    if let Some(end) = text.find("\r\n\r\n") {
                                        let headers = &text[..end];
                                        let body = &buf[end + 4..n];
                                        let _ = lume::ti_http::handle(
                                            &mut stream,
                                            Some(&s),
                                            method,
                                            path,
                                            headers,
                                            body,
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(_) => break,
            }
        }
    });
    TestHttpServer {
        url,
        stop: Some(stop_tx),
        thread: Some(thread),
    }
}

#[test]
fn test_default_limits_10k_rows_over_pgwire() {
    let dir = TestDir::new();
    let server = Arc::new(TiServer::open(&dir.store_root).unwrap());
    assert_eq!(server.query_limits().pg_max_rows, 100_000);
    assert_eq!(server.query_limits().pg_max_bytes, 16 * 1024 * 1024);

    let listener = ti_pg::start(server.clone(), "127.0.0.1:0".parse().unwrap()).unwrap();
    let runtime = ti_sql::surface_runtime().unwrap();
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(30), async {
            let (client, conn) = connect_pg(listener.address).await;
            eprintln!(
                "[test_default_limits {:?}] query started",
                std::time::Instant::now()
            );
            let rows = client
                .query("SELECT * FROM generate_series(1, 10000)", &[])
                .await
                .unwrap();
            eprintln!(
                "[test_default_limits {:?}] query completed with {} rows",
                std::time::Instant::now(),
                rows.len()
            );
            assert_eq!(rows.len(), 10000);
            assert_eq!(rows[0].get::<_, i64>(0), 1);
            assert_eq!(rows[9999].get::<_, i64>(0), 10000);

            drop(client);
            let _ = conn.await;
        })
        .await
        .expect("test_default_limits_10k_rows_over_pgwire timed out after 30s");
    });
}

#[test]
fn test_configured_cap_returns_54000() {
    let dir = TestDir::new();
    std::fs::write(
        dir.store_root.join("ti.toml"),
        "width_seconds = 10\n\n[query]\npg_max_rows = 100\n",
    )
    .unwrap();
    let server = Arc::new(TiServer::open(&dir.store_root).unwrap());
    assert_eq!(server.query_limits().pg_max_rows, 100);

    // Bind to 127.0.0.2 to test non-default loopback interface
    let listener = ti_pg::start(server.clone(), "127.0.0.2:0".parse().unwrap()).unwrap();
    let runtime = ti_sql::surface_runtime().unwrap();
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(30), async {
            let (client, conn) = connect_pg(listener.address).await;

            // Exactly 100 rows succeeds
            let rows = client
                .query("SELECT * FROM generate_series(1, 100)", &[])
                .await
                .unwrap();
            assert_eq!(rows.len(), 100);

            // 101 rows exceeds configured cap of 100 and returns SQLSTATE 54000
            let err = client
                .query("SELECT * FROM generate_series(1, 101)", &[])
                .await
                .unwrap_err();
            assert_eq!(err.code().map(|c| c.code()), Some("54000"));
            let msg = err.as_db_error().map(|d| d.message()).unwrap_or("");
            assert!(
                msg.contains("aggregate"),
                "expected aggregate hint, got: {msg}"
            );
            assert!(
                msg.contains("100"),
                "expected configured limit in message, got: {msg}"
            );

            drop(client);
            let _ = conn.await;
        })
        .await
        .expect("test_configured_cap_returns_54000 timed out after 30s");
    });
}

#[test]
fn test_http_still_caps_at_500() {
    let dir = TestDir::new();
    let server = Arc::new(TiServer::open(&dir.store_root).unwrap());
    let http = start_http(server.clone(), "127.0.0.1");

    let runtime = ti_sql::surface_runtime().unwrap();
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(30), async {
            // 1. Arrow stream query: default accepts arrow stream
            let response = ureq::post(&format!("{}/ti/query", http.url))
                .send_json(serde_json::json!({
                    "sql": "SELECT * FROM generate_series(1, 1000)",
                    "max_rows": 1000
                }))
                .unwrap();
            assert_eq!(
                response.header("Content-Type"),
                Some("application/vnd.apache.arrow.stream")
            );
            assert_eq!(response.header("X-TI-Row-Count"), Some("500"));
            assert_eq!(response.header("X-TI-Truncated"), Some("true"));
            assert!(response
                .header("X-TI-Hint")
                .unwrap_or("")
                .contains("Aggregate"));

            let reader =
                ti_sql::arrow_ipc::reader::StreamReader::try_new(response.into_reader(), None)
                    .unwrap();
            let total_arrow_rows: usize = reader.map(|b| b.unwrap().num_rows()).sum();
            assert_eq!(total_arrow_rows, 500);

            // 2. JSON query: with Accept: application/json
            let json_resp: Value = ureq::post(&format!("{}/ti/query", http.url))
                .set("Accept", "application/json")
                .send_json(serde_json::json!({
                    "sql": "SELECT * FROM generate_series(1, 1000)",
                    "max_rows": 1000
                }))
                .unwrap()
                .into_json()
                .unwrap();
            assert_eq!(json_resp["row_count"], 500);
            assert_eq!(json_resp["truncated"], true);
            assert_eq!(json_resp["rows"].as_array().unwrap().len(), 500);
        })
        .await
        .expect("test_http_still_caps_at_500 timed out after 30s");
    });
}

#[test]
fn test_rows_arrive_before_query_finishes() {
    let dir = TestDir::new();
    let server = Arc::new(TiServer::open(&dir.store_root).unwrap());
    let listener = ti_pg::start(server.clone(), "127.0.0.1:0".parse().unwrap()).unwrap();
    let runtime = ti_sql::surface_runtime().unwrap();
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(30), async {
            let (client, conn) = connect_pg(listener.address).await;
            let params: [&(dyn tokio_postgres::types::ToSql + Sync); 0] = [];
            eprintln!(
                "[test_rows_arrive {:?}] starting query_raw",
                std::time::Instant::now()
            );
            let stream = client
                .query_raw("SELECT * FROM generate_series(1, 10000)", params)
                .await
                .unwrap();
            futures::pin_mut!(stream);

            // Read the very first row
            let first = stream.next().await.unwrap().unwrap();
            let first_val: i64 = first.get(0);
            assert_eq!(first_val, 1);
            eprintln!(
                "[test_rows_arrive {:?}] first row arrived: batches_yielded={}, query_completed={}",
                std::time::Instant::now(),
                server.pg_batches_yielded(),
                server.pg_query_completed()
            );

            // Verify that the first row arrived before the query completed,
            // and specifically before the last batch is produced.
            assert!(
                !server.pg_query_completed(),
                "first row must arrive before query finishes"
            );
            assert_eq!(
                server.pg_batches_yielded(),
                1,
                "first row must arrive before the last batch is produced"
            );

            // Drain the rest of the stream
            let mut count = 1;
            while let Some(row) = stream.next().await {
                let _ = row.unwrap();
                count += 1;
            }
            assert_eq!(count, 10000);
            eprintln!(
                "[test_rows_arrive {:?}] all rows drained: batches_yielded={}, query_completed={}",
                std::time::Instant::now(),
                server.pg_batches_yielded(),
                server.pg_query_completed()
            );

            // Query completes after stream finishes and multiple batches were yielded
            assert!(
                server.pg_query_completed(),
                "query must complete after draining stream"
            );
            assert_eq!(
                server.pg_batches_yielded(),
                2,
                "all batches must be yielded for 10k rows"
            );

            drop(client);
            let _ = conn.await;
        })
        .await
        .expect("test_rows_arrive_before_query_finishes timed out after 30s");
    });
}

#[test]
fn test_portal_paging_retrieves_all_rows() {
    let dir = TestDir::new();
    let server = Arc::new(TiServer::open(&dir.store_root).unwrap());
    let listener = ti_pg::start(server.clone(), "127.0.0.1:0".parse().unwrap()).unwrap();
    let runtime = ti_sql::surface_runtime().unwrap();
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(30), async {
            let (mut client, conn) = connect_pg(listener.address).await;
            let stmt = client
                .prepare("SELECT * FROM generate_series(1, 1000)")
                .await
                .unwrap();
            let transaction = client.transaction().await.unwrap();
            let portal = transaction.bind(&stmt, &[]).await.unwrap();

            let mut total_rows = 0;
            let mut fetches = 0;
            while total_rows < 1000 {
                let rows = transaction.query_portal(&portal, 100).await.unwrap();
                assert_eq!(rows.len(), 100);
                fetches += 1;
                for row in rows {
                    total_rows += 1;
                    let val: i64 = row.get(0);
                    assert_eq!(val, total_rows);
                }
            }

            assert_eq!(fetches, 10, "expected all 1,000 rows across 10 fetches");
            assert_eq!(total_rows, 1000);

            let empty = transaction.query_portal(&portal, 100).await.unwrap();
            assert_eq!(empty.len(), 0);

            transaction.rollback().await.unwrap();

            drop(client);
            let _ = conn.await;
        })
        .await
        .expect("test_portal_paging_retrieves_all_rows timed out after 30s");
    });
}
