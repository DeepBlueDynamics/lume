#![cfg(feature = "ti")]

use std::{
    io::{Read, Write},
    net::TcpStream,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};
use ti_contracts::{Catalog, ShardSink};

struct StoreDir(PathBuf);
impl Drop for StoreDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn store() -> StoreDir {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "pg-frames-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let mut store = ti_store::Store::open_or_create(&root, 10).unwrap();
    let vessel = store
        .catalog()
        .register_vessel(&ti_contracts::VesselSpec {
            urn: "vessels.urn:test:frames".into(),
            name: None,
            mmsi: None,
        })
        .unwrap();
    store
        .seal(ti_contracts::ShardKey { vessel, shard: 0 })
        .unwrap();
    store.shutdown().unwrap();
    std::fs::write(root.join("ti.toml"), "width_seconds = 10\n").unwrap();
    StoreDir(root)
}
fn connect(address: std::net::SocketAddr) -> TcpStream {
    let stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
}
fn message(stream: &mut TcpStream) -> (u8, Vec<u8>) {
    let mut header = [0; 5];
    stream.read_exact(&mut header).unwrap();
    let length = u32::from_be_bytes(header[1..].try_into().unwrap()) as usize;
    assert!((4..65536).contains(&length), "unbounded server response");
    let mut body = vec![0; length - 4];
    stream.read_exact(&mut body).unwrap();
    (header[0], body)
}
fn startup(stream: &mut TcpStream) {
    let mut body = 196608u32.to_be_bytes().to_vec();
    body.extend_from_slice(b"user\0lume\0database\0ti\0\0");
    stream
        .write_all(&((body.len() + 4) as u32).to_be_bytes())
        .unwrap();
    stream.write_all(&body).unwrap();
    loop {
        let (kind, _) = message(stream);
        assert_ne!(kind, b'E');
        if kind == b'Z' {
            break;
        }
    }
}
fn fatal_and_closed(stream: &mut TcpStream) {
    let (kind, body) = message(stream);
    assert_eq!(kind, b'E');
    assert!(body.windows(7).any(|v| v == b"SFATAL\0"));
    assert!(body.windows(7).any(|v| v == b"C08P01\0"));
    let mut byte = [0];
    assert_eq!(stream.read(&mut byte).unwrap(), 0);
}
fn send_query(stream: &mut TcpStream, sql: &str) {
    stream.write_all(b"Q").unwrap();
    stream
        .write_all(&((sql.len() + 5) as u32).to_be_bytes())
        .unwrap();
    stream.write_all(sql.as_bytes()).unwrap();
    stream.write_all(&[0]).unwrap();
}
fn query(stream: &mut TcpStream, sql: &str) {
    send_query(stream, sql);
    let mut rows = 0;
    loop {
        let (kind, body) = message(stream);
        assert_ne!(kind, b'E', "{}", String::from_utf8_lossy(&body));
        if kind == b'D' {
            rows += 1;
        }
        if kind == b'Z' {
            break;
        }
    }
    assert_eq!(rows, 1);
}

#[test]
fn oversized_frames_fail_before_body_and_listener_recovers() {
    let dir = store();
    let server = Arc::new(lume::ti_http::TiServer::open(&dir.0).unwrap());
    let listener = lume::ti_pg::start(server, "127.0.0.1:0".parse().unwrap()).unwrap();
    for length in [0x80000000u32, 0x7fffffff] {
        let mut stream = connect(listener.address);
        stream.write_all(&length.to_be_bytes()).unwrap();
        stream.write_all(&196608u32.to_be_bytes()).unwrap();
        fatal_and_closed(&mut stream);
    }
    for length in [2 * 1024 * 1024u32, 0x80000000, 0x7fffffff] {
        for tag in [b'Q', b'P', b'B'] {
            let mut stream = connect(listener.address);
            startup(&mut stream);
            stream.write_all(&[tag]).unwrap();
            stream.write_all(&length.to_be_bytes()).unwrap();
            // Only a few body bytes, with no half-close: rejection cannot wait
            // for the promised body or use the idle timeout.
            stream.write_all(b"x\0").unwrap();
            fatal_and_closed(&mut stream);
        }
    }
    let mut healthy = connect(listener.address);
    startup(&mut healthy);
    let sql = format!("SELECT 1 /*{}*/", " ".repeat(512 * 1024 - 13));
    assert_eq!(sql.len(), 512 * 1024);
    send_query(&mut healthy, &sql);
    let (kind, body) = message(&mut healthy);
    assert_eq!(kind, b'E');
    assert!(body.windows(7).any(|v| v == b"SERROR\0"));
    assert!(body.windows(7).any(|v| v == b"C22000\0"));
    assert!(String::from_utf8_lossy(&body).contains("SQL exceeds 64 KiB"));
    assert_eq!(message(&mut healthy).0, b'Z');
    query(&mut healthy, "SELECT 1");
    drop(healthy);
    let mut next = connect(listener.address);
    startup(&mut next);
    query(&mut next, "SELECT 2");
}
