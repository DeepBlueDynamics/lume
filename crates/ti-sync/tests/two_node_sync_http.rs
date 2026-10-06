use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ti_contracts::{
    Agg, BucketRecord, Catalog, FieldKind, FieldSpec, FieldValue, RoaringBitmap, ShardKey,
    ShardSink, ShardSource, VesselSpec,
};
use ti_store::Store;
use ti_sync::{
    hex_decode_32, hex_encode, percent_decode, HttpTransport, LossyTransport, ShoreReceiver,
    SyncClient, SyncReport, UploadChunk,
};

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

fn setup_populated_local_store(root: &std::path::Path) -> Store {
    let mut store = Store::open_or_create(root, 10).unwrap();

    let urn = "vessels.urn:mrn:imo:mmsi:230999999";
    let vessel_ord = store
        .catalog()
        .register_vessel(&VesselSpec {
            urn: urn.into(),
            name: Some("PV-1".into()),
            mmsi: Some("230999999".into()),
        })
        .unwrap();

    let field_speed = store
        .catalog()
        .register_field(&FieldSpec {
            id: 0,
            path: "navigation.speedOverGround".into(),
            agg: Some(Agg::Mean),
            kind: FieldKind::Bsi { scale: 2 },
            units: Some("m/s".into()),
        })
        .unwrap();

    let field_state = store
        .catalog()
        .register_field(&FieldSpec {
            id: 1,
            path: "propulsion.port.state".into(),
            agg: None,
            kind: FieldKind::Set,
            units: None,
        })
        .unwrap();

    let row_motoring = store
        .catalog()
        .register_set_value(field_state, "motoring")
        .unwrap();

    // Data in Shard 0 (buckets 10..20)
    let records_shard0 = vec![
        BucketRecord {
            vessel: vessel_ord,
            bucket: 10,
            field: field_speed,
            value: FieldValue::Int(550), // 5.50 m/s
            rewrite: false,
        },
        BucketRecord {
            vessel: vessel_ord,
            bucket: 10,
            field: field_state,
            value: FieldValue::SetValue(row_motoring),
            rewrite: false,
        },
        BucketRecord {
            vessel: vessel_ord,
            bucket: 15,
            field: field_speed,
            value: FieldValue::Int(620), // 6.20 m/s
            rewrite: false,
        },
    ];

    // Data in Shard 1 (buckets 65536 + 10..20)
    let base_shard1 = 65536;
    let records_shard1 = vec![
        BucketRecord {
            vessel: vessel_ord,
            bucket: base_shard1 + 10,
            field: field_speed,
            value: FieldValue::Int(710), // 7.10 m/s
            rewrite: false,
        },
        BucketRecord {
            vessel: vessel_ord,
            bucket: base_shard1 + 10,
            field: field_state,
            value: FieldValue::SetValue(row_motoring),
            rewrite: false,
        },
    ];

    store.apply(&records_shard0).unwrap();
    store
        .seal(ShardKey {
            vessel: vessel_ord,
            shard: 0,
        })
        .unwrap();

    store.apply(&records_shard1).unwrap();
    store
        .seal(ShardKey {
            vessel: vessel_ord,
            shard: 1,
        })
        .unwrap();

    store
}

struct MockHttpServer {
    port: u16,
    shutdown: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl MockHttpServer {
    fn start(receiver: Arc<ShoreReceiver>, expected_token: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_clone = shutdown.clone();
        let expected_token = expected_token.to_string();

        let handle = std::thread::spawn(move || {
            listener.set_nonblocking(true).unwrap();
            while !shutdown_clone.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        Self::handle_connection(&mut stream, &receiver, &expected_token);
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

    fn handle_connection(stream: &mut TcpStream, receiver: &ShoreReceiver, expected_token: &str) {
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
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
        let mut auth_bearer = None;
        let mut x_blake3 = None;
        let mut x_total_chunks = None;
        let mut x_offset = None;
        let mut x_total_bytes = None;
        let mut x_shard_hash = None;
        let mut x_catalog_hash = None;
        let mut x_from = None;
        let mut x_to = None;
        let mut x_width_seconds = None;

        for line in lines {
            if let Some((k, v)) = line.split_once(':') {
                let key = k.trim().to_ascii_lowercase();
                let val = v.trim();
                match key.as_str() {
                    "content-length" => content_length = val.parse().unwrap_or(0),
                    "authorization" => {
                        if let Some(tok) = val.strip_prefix("Bearer ") {
                            auth_bearer = Some(tok.trim().to_string());
                        }
                    }
                    "x-blake3" => x_blake3 = Some(val.to_string()),
                    "x-total-chunks" => x_total_chunks = val.parse::<u32>().ok(),
                    "x-offset" => x_offset = val.parse::<u64>().ok(),
                    "x-total-bytes" => x_total_bytes = val.parse::<u64>().ok(),
                    "x-shard-hash" => x_shard_hash = hex_decode_32(val).ok(),
                    "x-catalog-hash" => x_catalog_hash = hex_decode_32(val).ok(),
                    "x-from" => x_from = val.parse::<u32>().ok(),
                    "x-to" => x_to = val.parse::<u32>().ok(),
                    "x-width-seconds" => x_width_seconds = val.parse::<u64>().ok(),
                    _ => {}
                }
            }
        }

        // Read remaining body if not fully received
        let mut body = buffer[header_end + 4..].to_vec();
        while body.len() < content_length {
            match stream.read(&mut temp) {
                Ok(0) => break,
                Ok(n) => body.extend_from_slice(&temp[..n]),
                Err(_) => break,
            }
        }

        fn ct_eq(a: &str, b: &str) -> bool {
            let ha = blake3::hash(a.as_bytes());
            let hb = blake3::hash(b.as_bytes());
            let mut diff = 0u8;
            for (x, y) in ha.as_bytes().iter().zip(hb.as_bytes().iter()) {
                diff |= x ^ y;
            }
            diff == 0
        }

        // 1. Auth check
        if !ct_eq(auth_bearer.as_deref().unwrap_or(""), expected_token) {
            let resp = b"HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"error\":\"Unauthorized: invalid bearer token.\"}";
            let _ = stream.write_all(resp);
            return;
        }

        // 2. Dispatch endpoints
        if path == "/ti/manifest" && method == "GET" {
            let store = receiver.store().lock().unwrap();
            let entries = store.manifest().entries();
            let mut vessels = BTreeMap::new();
            let mut ord = 0u32;
            while let Ok(urn) = store.catalog().vessel_urn(ord) {
                vessels.insert(ord.to_string(), urn);
                ord += 1;
            }
            drop(store);
            let payload = serde_json::json!({
                "entries": entries,
                "vessels": vessels,
            });
            let body = serde_json::to_vec(&payload).unwrap();
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(header.as_bytes());
            let _ = stream.write_all(&body);
            return;
        }

        if let Some(rest) = path.strip_prefix("/ti/shards/") {
            let parts: Vec<&str> = rest.split('/').collect();
            if parts.len() < 4 {
                let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n");
                return;
            }
            let vessel_urn = percent_decode(parts[0]).unwrap();
            let shard: u32 = parts[1].parse().unwrap();
            let version: u64 = parts[2].parse().unwrap();
            let action = parts[3];

            if action == "status" && method == "GET" {
                let transfer = ti_contracts::TransferIdentity {
                    vessel_urn,
                    shard,
                    version,
                    width_seconds: 0,
                    from: 0,
                    to: 0,
                    hash: x_shard_hash.unwrap_or([0u8; 32]),
                    catalog_hash: [0u8; 32],
                };
                match receiver.upload_status(&transfer) {
                    Ok(status) => {
                        let body = serde_json::to_vec(&status).unwrap();
                        let header = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(header.as_bytes());
                        let _ = stream.write_all(&body);
                    }
                    Err(e) => {
                        let _ =
                            stream.write_all(format!("HTTP/1.1 500 Error\r\n\r\n{e}").as_bytes());
                    }
                }
                return;
            }

            if action == "chunks" && method == "POST" {
                let chunk_index: u32 = parts[4].parse().unwrap();
                let chunk = UploadChunk::new(
                    ti_contracts::TransferIdentity {
                        vessel_urn,
                        shard,
                        version,
                        width_seconds: x_width_seconds.unwrap_or(10),
                        from: x_from.unwrap_or(0),
                        to: x_to.unwrap_or(0),
                        hash: x_shard_hash.unwrap_or([0u8; 32]),
                        catalog_hash: x_catalog_hash.unwrap_or([0u8; 32]),
                    },
                    chunk_index,
                    x_total_chunks.unwrap(),
                    x_offset.unwrap(),
                    x_total_bytes.unwrap(),
                    body,
                );

                if let Some(expected_blake3) = x_blake3 {
                    if hex_encode(&chunk.chunk_hash) != expected_blake3 {
                        let _ = stream
                            .write_all(b"HTTP/1.1 400 Bad Request: X-BLAKE3 mismatch\r\n\r\n");
                        return;
                    }
                }

                match receiver.receive_chunk(&chunk) {
                    Ok(ack) => {
                        let body = serde_json::to_vec(&ack).unwrap();
                        let header = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(header.as_bytes());
                        let _ = stream.write_all(&body);
                    }
                    Err(e) => {
                        let _ = stream
                            .write_all(format!("HTTP/1.1 400 Bad Request\r\n\r\n{e}").as_bytes());
                    }
                }
                return;
            }

            if action == "commit" && method == "POST" {
                let transfer: ti_contracts::TransferIdentity =
                    serde_json::from_slice(&body).unwrap();
                match receiver.commit_upload(&transfer) {
                    Ok(entry) => {
                        let body = serde_json::to_vec(&entry).unwrap();
                        let header = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(header.as_bytes());
                        let _ = stream.write_all(&body);
                    }
                    Err(e) => {
                        let _ = stream
                            .write_all(format!("HTTP/1.1 400 Bad Request\r\n\r\n{e}").as_bytes());
                    }
                }
                return;
            }
        }

        let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\n\r\n");
    }
}

impl Drop for MockHttpServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[test]
fn test_two_node_sync_http_lossy_link_and_outage() {
    let local_scratch = Scratch::new("http-sync-boat");
    let shore_scratch = Scratch::new("http-sync-shore");

    let local_store = setup_populated_local_store(&local_scratch.0);
    let local_entries = local_store.manifest().entries();
    assert_eq!(local_entries.len(), 2, "local store has 2 sealed shards");

    let shore_store = Store::open_or_create(&shore_scratch.0, 10).unwrap();
    assert!(
        shore_store.manifest().entries().is_empty(),
        "shore starts empty"
    );

    let shore_arc = Arc::new(Mutex::new(shore_store));
    let receiver = Arc::new(ShoreReceiver::new(shore_arc.clone()));
    let token = "test-sync-bearer-token-12345";
    let server = MockHttpServer::start(receiver, token);

    // HTTP transport configured with shared bearer token
    let http = HttpTransport::new(
        format!("http://127.0.0.1:{}", server.port),
        Some(token.into()),
    );

    // Lossy transport with 20% chunk drop rate
    let lossy = LossyTransport::new(http, 0.20, 42);

    // Sync client using small 512-byte chunks so each shard is split into multiple chunks
    let local_arc = Arc::new(Mutex::new(local_store));
    let client = SyncClient::new(local_arc.clone(), lossy)
        .with_chunk_size(512)
        .with_max_retries(200);

    // 1. Initial diff sees 2 missing shards over HTTP
    let missing = client.diff().unwrap();
    assert_eq!(missing.len(), 2);
    assert_eq!(missing[0].local_key.shard, 0);
    assert_eq!(missing[1].local_key.shard, 1);

    // 2. Sync first shard through the 20% lossy HTTP link
    let mut report = SyncReport::default();
    let entry0 = client.sync_shard(&missing[0], &mut report).unwrap();
    assert_eq!(entry0.key.shard, 0);
    assert_eq!(entry0.hash, local_entries[0].hash);

    // Verify chunks were attempted, some dropped by lossy transport, and retried successfully
    assert!(client.transport().chunks_attempted() > 0);
    assert!(
        client.transport().chunks_dropped() > 0,
        "lossy link must have dropped chunks: attempted {}, dropped {}",
        client.transport().chunks_attempted(),
        client.transport().chunks_dropped()
    );

    // Shore now has exactly 1 shard installed in its manifest
    {
        let shore = shore_arc.lock().unwrap();
        assert_eq!(shore.manifest().entries().len(), 1);
        assert_eq!(shore.manifest().entries()[0].hash, local_entries[0].hash);
    }

    // 3. Simulate a 30-minute outage
    println!("[HTTP Link] Simulating 30-minute outage over HTTP transport");
    client.transport().set_offline(true);
    assert!(client.transport().is_offline());

    // While offline, sync attempts fail cleanly with network error
    let offline_err = client.diff();
    assert!(offline_err.is_err(), "sync calls during outage must fail");

    // 4. Restore link after 30-minute outage
    println!("[HTTP Link] Outage ended: connection restored");
    client.transport().set_offline(false);
    assert!(!client.transport().is_offline());

    // 5. Resume sync: diff now sees only shard 1 missing (resumption!), and finishes
    let diff_after = client.diff().unwrap();
    assert_eq!(
        diff_after.len(),
        1,
        "shard 0 is already on shore; only shard 1 missing"
    );
    assert_eq!(diff_after[0].local_key.shard, 1);

    let full_report = client.sync_all().unwrap();
    assert_eq!(full_report.shards_synced, 1);

    // 6. Verify convergence: shore manifest matches local byte-for-byte
    let shore = shore_arc.lock().unwrap();
    let shore_entries = shore.manifest().entries();
    assert_eq!(shore_entries.len(), 2, "shore now has both shards");

    for (local_e, shore_e) in local_entries.iter().zip(shore_entries.iter()) {
        assert_eq!(shore_e.key.shard, local_e.key.shard);
        assert_eq!(shore_e.version, local_e.version);
        assert_eq!(shore_e.from, local_e.from);
        assert_eq!(shore_e.to, local_e.to);
        assert_eq!(shore_e.bytes, local_e.bytes);
        assert_eq!(
            shore_e.hash, local_e.hash,
            "shard hashes must be byte-identical"
        );
    }

    // Compare raw manifest JSON bytes
    let local_manifest_bytes = std::fs::read(local_scratch.0.join("manifest.json")).unwrap();
    let shore_manifest_bytes = std::fs::read(shore_scratch.0.join("manifest.json")).unwrap();
    assert_eq!(
        local_manifest_bytes, shore_manifest_bytes,
        "manifest.json on shore must be byte-identical to local"
    );

    // Verify shore store can query data from imported shards
    let all_cols = RoaringBitmap::from_sorted_iter(0..1000).unwrap();
    let batch = shore
        .read(
            ShardKey {
                vessel: 0,
                shard: 0,
            },
            &all_cols,
            &[0, 1],
        )
        .unwrap();
    assert!(batch.num_rows() > 0, "shore store can read imported data");

    // 7. Verify unauthorized access is rejected with 401
    let bad_http = HttpTransport::new(
        format!("http://127.0.0.1:{}", server.port),
        Some("wrong-token".into()),
    );
    let bad_client = SyncClient::new(local_arc, bad_http);
    let err = bad_client.diff();
    assert!(err.is_err(), "sync with bad token must fail with 401");
}

#[test]
fn test_transfer_vessel_urn_validation() {
    let scratch = Scratch::new("test-urn-val");
    let store = Store::open_or_create(&scratch.0, 10).unwrap();
    let receiver = ShoreReceiver::new(Arc::new(Mutex::new(store)));

    let mut t_bad = ti_contracts::TransferIdentity {
        vessel_urn: "not-a-valid-urn".into(),
        shard: 0,
        version: 1,
        width_seconds: 10,
        from: 0,
        to: 100,
        hash: [1u8; 32],
        catalog_hash: [0u8; 32],
    };

    let chunk_bad = UploadChunk::new(t_bad.clone(), 0, 1, 0, 10, vec![1, 2, 3]);
    let err = receiver.receive_chunk(&chunk_bad);
    assert!(
        err.is_err(),
        "invalid URN must be rejected in receive_chunk"
    );

    let status_err = receiver.upload_status(&t_bad);
    assert!(
        status_err.is_err(),
        "invalid URN must be rejected in upload_status"
    );

    let commit_err = receiver.commit_upload(&t_bad);
    assert!(
        commit_err.is_err(),
        "invalid URN must be rejected in commit_upload"
    );

    // Over length URN (> 512 bytes)
    t_bad.vessel_urn = format!("vessels.urn:{}", "a".repeat(513));
    let chunk_long = UploadChunk::new(t_bad.clone(), 0, 1, 0, 10, vec![1, 2, 3]);
    assert!(receiver.receive_chunk(&chunk_long).is_err());
}

#[test]
fn test_inflight_transfers_bounded_and_expiry() {
    let scratch = Scratch::new("test-inflight-bound");
    let store = Store::open_or_create(&scratch.0, 10).unwrap();
    // Cap at 2 concurrent transfers, 1024 bytes, 50ms TTL
    let receiver = ShoreReceiver::new(Arc::new(Mutex::new(store))).with_limits(
        2,
        1024,
        Duration::from_millis(50),
    );

    let make_chunk = |urn: &str, shard: u32, size: usize| {
        let t = ti_contracts::TransferIdentity {
            vessel_urn: urn.into(),
            shard,
            version: 1,
            width_seconds: 10,
            from: 0,
            to: 100,
            hash: [1u8; 32],
            catalog_hash: [0u8; 32],
        };
        UploadChunk::new(t, 0, 2, 0, (size * 2) as u64, vec![42u8; size])
    };

    // Transfer 1
    let c1 = make_chunk("vessels.urn:mrn:boat1", 0, 100);
    assert!(receiver.receive_chunk(&c1).is_ok());

    // Transfer 2
    let c2 = make_chunk("vessels.urn:mrn:boat2", 0, 100);
    assert!(receiver.receive_chunk(&c2).is_ok());

    assert_eq!(receiver.pending_transfers_count(), 2);

    // Transfer 3 exceeds max_pending_transfers (2)
    let c3 = make_chunk("vessels.urn:mrn:boat3", 0, 100);
    let err3 = receiver.receive_chunk(&c3);
    assert!(
        err3.is_err(),
        "must reject when max pending transfers reached"
    );

    // Wait for TTL (50ms) to expire sessions
    std::thread::sleep(Duration::from_millis(60));

    // Transfer 3 now succeeds because expired sessions are pruned
    assert!(receiver.receive_chunk(&c3).is_ok());

    // Exceeding byte limit
    let c_big = make_chunk("vessels.urn:mrn:boat4", 0, 2000);
    let err_big = receiver.receive_chunk(&c_big);
    assert!(
        err_big.is_err(),
        "must reject when staging byte limit exceeded"
    );
}
