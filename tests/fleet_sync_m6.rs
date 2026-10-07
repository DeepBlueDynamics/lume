#![cfg(feature = "ti")]

use std::collections::BTreeMap;
use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ti_contracts::{Catalog, ShardKey, ShardSink, TiConfig, VesselSpec};
use ti_store::Store;
use ti_sync::{HttpTransport, SyncClient};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);
impl Scratch {
    fn new(prefix: &str) -> Self {
        let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn shard_keys(root: &Path) -> Vec<ShardKey> {
    let mut keys = Vec::new();
    if let Ok(vessels) = std::fs::read_dir(root.join("shards")) {
        for v in vessels.flatten() {
            let Ok(vessel) = v.file_name().to_string_lossy().parse::<u32>() else {
                continue;
            };
            if let Ok(shards) = std::fs::read_dir(v.path()) {
                for s in shards.flatten() {
                    if let Ok(shard) = s.file_name().to_string_lossy().parse::<u32>() {
                        keys.push(ShardKey { vessel, shard });
                    }
                }
            }
        }
    }
    keys.sort_by_key(|k| (k.vessel, k.shard));
    keys
}

struct TestHttpServer {
    port: u16,
    shutdown: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl TestHttpServer {
    fn start(server: Arc<lume::ti_http::TiServer>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_clone = shutdown.clone();

        let handle = std::thread::spawn(move || {
            listener.set_nonblocking(true).unwrap();
            while !shutdown_clone.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        Self::serve_one(&mut stream, &server);
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            port,
            shutdown,
            handle: Some(handle),
        }
    }

    fn serve_one(stream: &mut TcpStream, server: &lume::ti_http::TiServer) {
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut buffer = Vec::new();
        let mut temp = [0u8; 8192];
        let header_end;

        loop {
            match stream.read(&mut temp) {
                Ok(0) => return,
                Ok(n) => {
                    buffer.extend_from_slice(&temp[..n]);
                    if let Some(pos) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                        header_end = pos;
                        break;
                    }
                }
                Err(_) => return,
            }
        }

        let header_bytes = &buffer[..header_end];
        let header_str = String::from_utf8_lossy(header_bytes);
        let mut lines = header_str.lines();
        let request_line = lines.next().unwrap_or("");
        let parts: Vec<&str> = request_line.split_whitespace().collect();
        if parts.len() < 2 {
            return;
        }
        let method = parts[0];
        let path = parts[1];

        let mut content_length = 0usize;
        for line in lines {
            if let Some((k, v)) = line.split_once(':') {
                if k.trim().eq_ignore_ascii_case("content-length") {
                    content_length = v.trim().parse().unwrap_or(0);
                    break;
                }
            }
        }

        let mut body = buffer[header_end + 4..].to_vec();
        while body.len() < content_length {
            match stream.read(&mut temp) {
                Ok(0) => break,
                Ok(n) => body.extend_from_slice(&temp[..n]),
                Err(_) => break,
            }
        }

        let _ = lume::ti_http::handle(stream, Some(server), method, path, &header_str, &body);
    }
}

impl Drop for TestHttpServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[test]
fn test_m6_fleet_sync_fifty_vessels() {
    let scratch = Scratch::new("m6-fleet");
    let parquet_dir = scratch.0.join("parquet");
    let shore_dir = scratch.0.join("shore");
    let boats_dir = scratch.0.join("boats");

    std::fs::create_dir_all(&parquet_dir).unwrap();
    std::fs::create_dir_all(&shore_dir).unwrap();
    std::fs::create_dir_all(&boats_dir).unwrap();

    let n_vessels: usize = std::env::var("TI_FLEET_VESSELS")
        .ok()
        .and_then(|v| v.parse().ok())
        // The 50-vessel gate runs in release (about 5 s); debug builds take ~20 s per vessel.
        .unwrap_or(if cfg!(debug_assertions) { 5 } else { 50 });
    let seed = 42;
    let start = 1_772_323_200i64; // 2026-03-01 00:00:00 UTC
    let end = start + 600; // 10 minutes (short window per M6 spec)

    println!(
        "Generating {n_vessels} synthetic vessels into {}",
        parquet_dir.display()
    );
    let (raw_files, _, _) = ti_bench::write::write_all_stream_with_config(
        &parquet_dir.to_string_lossy(),
        seed,
        n_vessels,
        start,
        end,
        &ti_bench::gen::GenConfig::default(),
    );
    assert!(raw_files > 0, "must have generated raw parquet files");

    // Configure scales for numeric paths
    let mut cfg = TiConfig::default();
    cfg.width_seconds = 10;
    for p in ti_bench::model::NUMERIC_PATHS {
        if let Some(s) = ti_bench::model::scale_for(p) {
            cfg.path_scales.insert(p.to_string(), s);
        }
    }

    // Set up shore store with ti.toml sync token
    let token = "shore-m6-secret-bearer-token";
    let shore_toml = shore_dir.join("ti.toml");
    std::fs::write(
        &shore_toml,
        format!("width_seconds = 10\n\n[sync]\ntoken = \"{token}\"\n"),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shore_toml, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let shore_store = Store::open_or_create(&shore_dir, 10).unwrap();
    drop(shore_store);

    let shore_server = Arc::new(
        lume::ti_http::TiServer::open(&shore_dir)
            .unwrap()
            .with_sync_token(token),
    );
    let http_server = TestHttpServer::start(shore_server);
    let shore_url = format!("http://127.0.0.1:{}", http_server.port);

    let runtime = ti_sql::surface_runtime().unwrap();
    let mut expected_counts: BTreeMap<String, i64> = BTreeMap::new();
    let mut expected_max_winds: BTreeMap<(String, i64), f64> = BTreeMap::new();

    let total_start = std::time::Instant::now();

    // Ingest each vessel into its own local boat store, query it as oracle, then sync to shore
    for v in 0..n_vessels {
        let v_start = std::time::Instant::now();
        let vessel_urn = ti_bench::model::vessel_urn(v);
        let boat_dir = boats_dir.join(format!("boat_{v}"));
        std::fs::create_dir_all(&boat_dir).unwrap();

        let mut store = Store::open_or_create(&boat_dir, 10).unwrap();
        let _ = store
            .catalog()
            .register_vessel(&VesselSpec {
                urn: vessel_urn.clone(),
                name: Some(ti_bench::model::vessel_name(v)),
                mmsi: Some(format!("367{:06}", v)),
            })
            .unwrap();

        let sanitized = ti_bench::layout::sanitize_context(&vessel_urn);
        let vessel_raw = parquet_dir
            .join("tier=raw")
            .join(format!("context={sanitized}"));

        let catalog = Arc::clone(store.catalog());
        let _ = ti_ingest::parquet::backfill_directory(
            &vessel_raw,
            &vessel_urn,
            None,
            &cfg,
            catalog.as_ref(),
            &mut store,
        )
        .unwrap();

        store.flush().unwrap();
        let keys = shard_keys(&boat_dir);
        for k in &keys {
            store.seal(*k).unwrap();
        }
        drop(store);

        // Query boat store baseline with TiEngine
        let boat_engine = runtime
            .block_on(ti_sql::TiEngine::open(&boat_dir, Some(10), None))
            .unwrap();

        // 1. Boat count
        let q_count = "SELECT count(*) AS count FROM telemetry";
        let batches_count = runtime
            .block_on(boat_engine.session.query(q_count))
            .unwrap();
        let rows_count = ti_sql::rows_json(&batches_count).unwrap();
        assert_eq!(rows_count.len(), 1);
        let boat_cnt = rows_count[0]["count"].as_i64().unwrap();
        expected_counts.insert(vessel_urn.clone(), boat_cnt);

        // 2. Boat max wind per day
        let q_wind = "SELECT date_bin(INTERVAL '1 day', ts) AS day, max(\"environment.wind.speedApparent\") AS max_wind FROM telemetry GROUP BY 1 ORDER BY 1";
        let batches_wind = runtime.block_on(boat_engine.session.query(q_wind)).unwrap();
        let rows_wind = ti_sql::rows_json(&batches_wind).unwrap();
        for row in rows_wind {
            let day_ts = row["day"].as_i64().unwrap_or(0);
            let wind_val = row["max_wind"].as_f64().unwrap();
            expected_max_winds.insert((vessel_urn.clone(), day_ts), wind_val);
        }
        drop(boat_engine);

        // 3. Sync boat store to shore over HTTP transport
        let boat_store = Store::open_or_create(&boat_dir, 10).unwrap();
        let transport = HttpTransport::new(&shore_url, Some(token.to_string()));
        let client = SyncClient::new(Arc::new(std::sync::Mutex::new(boat_store)), transport);

        let report = client.sync_all().unwrap();
        assert_eq!(
            report.shards_synced,
            keys.len(),
            "vessel {v} must have synced all sealed shards"
        );
        let v_elapsed = v_start.elapsed();
        println!(
            "Vessel {v}/{n_vessels} ({vessel_urn}) ingested & synced in {:.2}s (elapsed {:.1}s)",
            v_elapsed.as_secs_f64(),
            total_start.elapsed().as_secs_f64()
        );
    }

    println!(
        "All {n_vessels} vessels synced to shore. Verifying fleet queries against shore store."
    );

    // Query shore store using TiEngine
    let shore_engine = runtime
        .block_on(ti_sql::TiEngine::open(&shore_dir, Some(10), None))
        .unwrap();

    // Query 1: Count per vessel across entire fleet
    let q_fleet_count = "SELECT vessel, count(*) AS count FROM telemetry GROUP BY 1 ORDER BY 1";
    let batches_fleet_count = runtime
        .block_on(shore_engine.session.query(q_fleet_count))
        .unwrap();
    let rows_fleet_count = ti_sql::rows_json(&batches_fleet_count).unwrap();

    assert_eq!(
        rows_fleet_count.len(),
        n_vessels,
        "fleet count query must return exactly {n_vessels} vessels"
    );

    for row in &rows_fleet_count {
        let v_urn = row["vessel"].as_str().unwrap();
        let shore_cnt = row["count"].as_i64().unwrap();
        let expected_cnt = expected_counts
            .get(v_urn)
            .unwrap_or_else(|| panic!("unexpected vessel on shore: {v_urn}"));
        assert_eq!(
            shore_cnt, *expected_cnt,
            "vessel {v_urn} count on shore ({shore_cnt}) must equal boat store count ({expected_cnt})"
        );
    }

    // Query 2: Max wind per vessel per day across entire fleet
    let q_fleet_wind = "SELECT vessel, date_bin(INTERVAL '1 day', ts) AS day, max(\"environment.wind.speedApparent\") AS max_wind FROM telemetry GROUP BY 1, 2 ORDER BY 1, 2";
    let batches_fleet_wind = runtime
        .block_on(shore_engine.session.query(q_fleet_wind))
        .unwrap();
    let rows_fleet_wind = ti_sql::rows_json(&batches_fleet_wind).unwrap();

    assert_eq!(
        rows_fleet_wind.len(),
        n_vessels,
        "fleet max wind query must return {n_vessels} rows (1 per vessel for 1 day)"
    );

    for row in &rows_fleet_wind {
        let v_urn = row["vessel"].as_str().unwrap();
        let day_ts = row["day"].as_i64().unwrap_or(0);
        let shore_max_wind = row["max_wind"].as_f64().unwrap();
        let expected_wind = expected_max_winds
            .get(&(v_urn.to_string(), day_ts))
            .unwrap_or_else(|| panic!("unexpected (vessel, day) on shore: ({v_urn}, {day_ts})"));

        let diff = (shore_max_wind - *expected_wind).abs();
        assert!(
            diff < 1e-4,
            "vessel {v_urn} max wind on shore ({shore_max_wind}) must equal boat ({expected_wind})"
        );
    }

    let total_elapsed = total_start.elapsed();
    println!(
        "M6 item 2 passed: {n_vessels} vessels synced & verified in {:.2}s ({:.2}s/vessel)",
        total_elapsed.as_secs_f64(),
        total_elapsed.as_secs_f64() / (n_vessels as f64)
    );
}

#[test]
fn test_sync_endpoints_disabled_without_token_and_query_works() {
    use std::io::Write;

    let scratch = Scratch::new("sync-no-token");
    let store_dir = scratch.0.join("store");
    std::fs::create_dir_all(&store_dir).unwrap();

    // Create store with ti.toml with NO [sync] section
    std::fs::write(store_dir.join("ti.toml"), "width_seconds = 10\n").unwrap();
    let store = Store::open_or_create(&store_dir, 10).unwrap();
    drop(store);

    let server = Arc::new(lume::ti_http::TiServer::open(&store_dir).unwrap());
    let http_server = TestHttpServer::start(server);
    let port = http_server.port;

    // Helper to send HTTP request and get status code
    let send_req = |req_str: &str| -> (u16, String) {
        let mut stream = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream.write_all(req_str.as_bytes()).unwrap();
        let mut resp = String::new();
        stream.read_to_string(&mut resp).unwrap();
        let status_line = resp.lines().next().unwrap_or("");
        let status: u16 = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        (status, resp)
    };

    // 1. Sync endpoint /ti/manifest without token returns 401
    let (status, resp) =
        send_req("GET /ti/manifest HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    assert_eq!(status, 401, "manifest without token must be 401: {resp}");
    assert!(
        !resp.contains("Follow-up: NUTS auth."),
        "must not contain follow-up notice"
    );

    // 2. Sync endpoint with Bearer token still returns 401 because server has no token configured
    let (status, _) = send_req("GET /ti/manifest HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer secret\r\nConnection: close\r\n\r\n");
    assert_eq!(
        status, 401,
        "manifest must be 401 when sync token is not configured on server"
    );

    // 3. Shard chunk upload without token returns 401
    let (status, _) = send_req("POST /ti/shards/vessels.urn:mrn:boat/0/1/chunks/0 HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    assert_eq!(status, 401, "chunks endpoint must be 401");

    // 4. Invalid URN returns 400 when configured with token
    let server_with_token = Arc::new(
        lume::ti_http::TiServer::open(&store_dir)
            .unwrap()
            .with_sync_token("my-token"),
    );
    let http_with_token = TestHttpServer::start(server_with_token);
    let port2 = http_with_token.port;
    let mut stream = TcpStream::connect(format!("127.0.0.1:{port2}")).unwrap();
    stream.write_all(b"POST /ti/shards/invalid_urn/0/1/chunks/0 HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer my-token\r\nConnection: close\r\n\r\n").unwrap();
    let mut resp = String::new();
    stream.read_to_string(&mut resp).unwrap();
    let status_code: u16 = resp
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    assert_eq!(
        status_code, 400,
        "invalid entity URN must return 400: {resp}"
    );

    // 5. Query endpoint /ti/query without any token still works and returns 200
    let (status_q, resp_q) = send_req("GET /ti/query?sql=SELECT+1+AS+one HTTP/1.1\r\nHost: 127.0.0.1\r\nAccept: application/json\r\nConnection: close\r\n\r\n");
    assert_eq!(
        status_q, 200,
        "/ti/query must return 200 without token: {resp_q}"
    );
    assert!(
        resp_q.contains("\"one\":1") || resp_q.contains("\"one\": 1") || resp_q.contains("[{"),
        "query must execute successfully: {resp_q}"
    );
}

#[test]
fn test_sync_config_token_file_error_and_permissions() {
    let scratch = Scratch::new("token-err-test");
    let store_dir = scratch.0.join("store");
    std::fs::create_dir_all(&store_dir).unwrap();

    // 1. token_file set to non-existent path -> resolved_token returns Err
    let cfg = ti_contracts::TiConfig::from_toml(
        "width_seconds = 10\n[sync]\ntoken_file = \"/path/does/not/exist\"\n",
    )
    .unwrap();
    assert!(cfg.sync.resolved_token().is_err());

    // 2. token_file set to empty file -> resolved_token returns Err
    let empty_file = scratch.0.join("empty_token");
    std::fs::write(&empty_file, "   \n").unwrap();
    let cfg_empty = ti_contracts::TiConfig::from_toml(&format!(
        // TOML literal string: Windows paths contain backslashes.
        "width_seconds = 10\n[sync]\ntoken_file = '{}'\n",
        empty_file.display()
    ))
    .unwrap();
    assert!(cfg_empty.sync.resolved_token().is_err());

    // 3. On unix, inline token with permissive ti.toml is rejected
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let init_store = Store::open_or_create(&store_dir, 10).unwrap();
        drop(init_store);
        let ti_toml = store_dir.join("ti.toml");
        std::fs::write(
            &ti_toml,
            "width_seconds = 10\n[sync]\ntoken = \"my-secret\"\n",
        )
        .unwrap();
        // Set mode 0644 (world-readable)
        std::fs::set_permissions(&ti_toml, std::fs::Permissions::from_mode(0o644)).unwrap();
        let open_res = lume::ti_http::TiServer::open(&store_dir);
        assert!(
            open_res.is_err(),
            "must reject permissive ti.toml with inline token"
        );

        // Set mode 0600 (owner only)
        std::fs::set_permissions(&ti_toml, std::fs::Permissions::from_mode(0o600)).unwrap();
        let open_ok = lume::ti_http::TiServer::open(&store_dir);
        assert!(
            open_ok.is_ok(),
            "must accept mode 0600 ti.toml with inline token"
        );
    }
}
