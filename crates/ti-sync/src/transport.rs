//! Transport abstraction for shard shipping, with loopback and lossy implementations.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use ti_contracts::{Catalog, Error, Result, ShardManifestEntry, TransferIdentity, VesselOrd};

use crate::chunk::{ChunkAck, UploadChunk, UploadStatus};
use crate::shore::ShoreReceiver;

/// Pluggable network transport boundary for syncing shards.
pub trait Transport: Send + Sync {
    /// Fetch current manifest entries from the shore node.
    fn fetch_manifest(&self) -> Result<Vec<ShardManifestEntry>>;

    /// Resolve a foreign vessel ordinal on the shore node to its URN.
    fn resolve_vessel_urn(&self, ord: VesselOrd) -> Result<String>;

    /// Check upload status of a shard.
    fn upload_status(&self, transfer: &TransferIdentity) -> Result<UploadStatus>;

    /// Send a single verifiable chunk.
    fn send_chunk(&self, chunk: &UploadChunk) -> Result<ChunkAck>;

    /// Commit/finalize an upload on the shore node after all chunks have been transmitted.
    fn commit_upload(&self, transfer: &TransferIdentity) -> Result<ShardManifestEntry>;
}

/// In-process loopback transport connecting directly to a local or test `ShoreReceiver`.
pub struct LoopbackTransport {
    receiver: Arc<ShoreReceiver>,
}

impl LoopbackTransport {
    pub fn new(receiver: Arc<ShoreReceiver>) -> Self {
        Self { receiver }
    }
}

impl Transport for LoopbackTransport {
    fn fetch_manifest(&self) -> Result<Vec<ShardManifestEntry>> {
        let store = self.receiver.store().lock().unwrap();
        Ok(store.manifest().entries())
    }

    fn resolve_vessel_urn(&self, ord: VesselOrd) -> Result<String> {
        let store = self.receiver.store().lock().unwrap();
        store.catalog().vessel_urn(ord)
    }

    fn upload_status(&self, transfer: &TransferIdentity) -> Result<UploadStatus> {
        self.receiver.upload_status(transfer)
    }

    fn send_chunk(&self, chunk: &UploadChunk) -> Result<ChunkAck> {
        self.receiver.receive_chunk(chunk)
    }

    fn commit_upload(&self, transfer: &TransferIdentity) -> Result<ShardManifestEntry> {
        self.receiver.commit_upload(transfer)
    }
}

/// Simulated lossy transport dropping chunks with a configurable loss rate
/// and supporting simulated network outages.
pub struct LossyTransport<T: Transport> {
    inner: T,
    drop_rate: f64,
    offline: AtomicBool,
    prng_state: Mutex<u64>,
    chunks_attempted: AtomicU64,
    chunks_dropped: AtomicU64,
}

impl<T: Transport> LossyTransport<T> {
    pub fn new(inner: T, drop_rate: f64, seed: u64) -> Self {
        Self {
            inner,
            drop_rate: drop_rate.clamp(0.0, 1.0),
            offline: AtomicBool::new(false),
            prng_state: Mutex::new(seed.max(1)),
            chunks_attempted: AtomicU64::new(0),
            chunks_dropped: AtomicU64::new(0),
        }
    }

    /// Toggle simulated outage (e.g. 30-minute Starlink/cellular drop).
    pub fn set_offline(&self, offline: bool) {
        self.offline.store(offline, Ordering::SeqCst);
    }

    pub fn is_offline(&self) -> bool {
        self.offline.load(Ordering::SeqCst)
    }

    pub fn chunks_attempted(&self) -> u64 {
        self.chunks_attempted.load(Ordering::SeqCst)
    }

    pub fn chunks_dropped(&self) -> u64 {
        self.chunks_dropped.load(Ordering::SeqCst)
    }

    fn check_offline(&self) -> Result<()> {
        if self.is_offline() {
            Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "simulated link outage",
            )))
        } else {
            Ok(())
        }
    }

    fn should_drop(&self) -> bool {
        let mut s = self.prng_state.lock().unwrap();
        // LCG PRNG
        *s = s
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let val = ((*s >> 33) as f64) / 2147483648.0;
        val < self.drop_rate
    }
}

impl<T: Transport> Transport for LossyTransport<T> {
    fn fetch_manifest(&self) -> Result<Vec<ShardManifestEntry>> {
        self.check_offline()?;
        self.inner.fetch_manifest()
    }

    fn resolve_vessel_urn(&self, ord: VesselOrd) -> Result<String> {
        self.check_offline()?;
        self.inner.resolve_vessel_urn(ord)
    }

    fn upload_status(&self, transfer: &TransferIdentity) -> Result<UploadStatus> {
        self.check_offline()?;
        self.inner.upload_status(transfer)
    }

    fn send_chunk(&self, chunk: &UploadChunk) -> Result<ChunkAck> {
        self.check_offline()?;
        self.chunks_attempted.fetch_add(1, Ordering::SeqCst);

        if self.should_drop() {
            self.chunks_dropped.fetch_add(1, Ordering::SeqCst);
            Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("chunk {} dropped by lossy transport", chunk.chunk_index),
            )))
        } else {
            self.inner.send_chunk(chunk)
        }
    }

    fn commit_upload(&self, transfer: &TransferIdentity) -> Result<ShardManifestEntry> {
        self.check_offline()?;
        self.inner.commit_upload(transfer)
    }
}

/// HTTP-based transport communicating with a remote or loopback shore node.
pub struct HttpTransport {
    base_url: String,
    auth_token: Option<String>,
    vessels_cache: Mutex<BTreeMap<VesselOrd, String>>,
}

impl HttpTransport {
    pub fn new(base_url: impl Into<String>, auth_token: Option<String>) -> Self {
        let base_url = base_url.into().trim_end_matches('/').to_string();
        Self {
            base_url,
            auth_token,
            vessels_cache: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn with_auth_token(mut self, token: impl Into<String>) -> Self {
        self.auth_token = Some(token.into());
        self
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn parse_url(&self) -> Result<(String, u16, String)> {
        let s = &self.base_url;
        let s = if let Some(rest) = s.strip_prefix("http://") {
            rest
        } else if s.starts_with("https://") {
            return Err(Error::Unsupported(
                "HTTPS transport requires TLS support; use http:// for loopback / local shore endpoints".into(),
            ));
        } else {
            s.as_str()
        };

        let (host_port, prefix) = match s.split_once('/') {
            Some((hp, rest)) => (hp, format!("/{}", rest.trim_end_matches('/'))),
            None => (s, String::new()),
        };

        let (host, port) = match host_port.split_once(':') {
            Some((h, p)) => {
                let port: u16 = p.parse().map_err(|e| {
                    Error::InvalidInput(format!("invalid port in URL {}: {e}", self.base_url))
                })?;
                (h.to_string(), port)
            }
            None => (host_port.to_string(), 80),
        };

        Ok((host, port, prefix))
    }

    fn http_request(
        &self,
        method: &str,
        path: &str,
        extra_headers: &[(&str, &str)],
        body: &[u8],
    ) -> Result<Vec<u8>> {
        let (host, port, prefix) = self.parse_url()?;
        let full_path = if prefix.is_empty() {
            path.to_string()
        } else {
            format!("{prefix}{path}")
        };

        let mut stream = TcpStream::connect((host.as_str(), port))?;
        stream.set_read_timeout(Some(Duration::from_secs(30)))?;
        stream.set_write_timeout(Some(Duration::from_secs(30)))?;

        let mut req = Vec::new();
        write!(
            req,
            "{method} {full_path} HTTP/1.1\r\n\
             Host: {host}:{port}\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n",
            body.len()
        )
        .map_err(|e| Error::Io(std::io::Error::other(e)))?;

        if let Some(token) = &self.auth_token {
            write!(req, "Authorization: Bearer {token}\r\n")
                .map_err(|e| Error::Io(std::io::Error::other(e)))?;
        }

        for (name, val) in extra_headers {
            write!(req, "{name}: {val}\r\n").map_err(|e| Error::Io(std::io::Error::other(e)))?;
        }

        req.extend_from_slice(b"\r\n");
        req.extend_from_slice(body);

        stream.write_all(&req)?;
        stream.flush()?;

        let mut resp_bytes = Vec::new();
        stream.read_to_end(&mut resp_bytes)?;

        let header_end = resp_bytes
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .ok_or_else(|| {
                Error::Corrupt("malformed HTTP response: missing header boundary".into())
            })?;

        let header_str = String::from_utf8_lossy(&resp_bytes[..header_end]);
        let mut status_code = 0u16;
        for (i, line) in header_str.lines().enumerate() {
            if i == 0 {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() < 2 {
                    return Err(Error::Corrupt("malformed HTTP status line".into()));
                }
                status_code = parts[1].parse().map_err(|e| {
                    Error::Corrupt(format!("invalid HTTP status code {}: {e}", parts[1]))
                })?;
                break;
            }
        }

        let resp_body = &resp_bytes[header_end + 4..];

        match status_code {
            200 | 201 | 204 => Ok(resp_body.to_vec()),
            401 => Err(Error::InvalidInput(format!(
                "HTTP 401 Unauthorized: {}",
                String::from_utf8_lossy(resp_body)
            ))),
            404 => Err(Error::NotFound(format!(
                "HTTP 404 Not Found: {}",
                String::from_utf8_lossy(resp_body)
            ))),
            code => Err(Error::InvalidInput(format!(
                "HTTP error {code}: {}",
                String::from_utf8_lossy(resp_body)
            ))),
        }
    }
}

impl Transport for HttpTransport {
    fn fetch_manifest(&self) -> Result<Vec<ShardManifestEntry>> {
        let resp = self.http_request("GET", "/ti/manifest", &[], &[])?;
        #[derive(serde::Deserialize)]
        struct ManifestPayload {
            entries: Vec<ShardManifestEntry>,
            #[serde(default)]
            vessels: BTreeMap<String, String>,
        }
        let payload: ManifestPayload = serde_json::from_slice(&resp)
            .map_err(|e| Error::Corrupt(format!("invalid manifest JSON from shore: {e}")))?;

        let mut cache = self.vessels_cache.lock().unwrap();
        for (ord_str, urn) in payload.vessels {
            if let Ok(ord) = ord_str.parse::<u32>() {
                cache.insert(ord, urn);
            }
        }

        Ok(payload.entries)
    }

    fn resolve_vessel_urn(&self, ord: VesselOrd) -> Result<String> {
        {
            let cache = self.vessels_cache.lock().unwrap();
            if let Some(urn) = cache.get(&ord) {
                return Ok(urn.clone());
            }
        }

        self.fetch_manifest()?;
        let cache = self.vessels_cache.lock().unwrap();
        cache
            .get(&ord)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("vessel ordinal {ord} not known on shore")))
    }

    fn upload_status(&self, transfer: &TransferIdentity) -> Result<UploadStatus> {
        let path = format!(
            "/ti/shards/{}/{}/{}/status",
            percent_encode(&transfer.vessel_urn),
            transfer.shard,
            transfer.version
        );
        let hash_hex = hex_encode(&transfer.hash);
        let headers = [("X-Shard-Hash", hash_hex.as_str())];
        let resp = self.http_request("GET", &path, &headers, &[])?;
        serde_json::from_slice(&resp)
            .map_err(|e| Error::Corrupt(format!("invalid status JSON: {e}")))
    }

    fn send_chunk(&self, chunk: &UploadChunk) -> Result<ChunkAck> {
        let path = format!(
            "/ti/shards/{}/{}/{}/chunks/{}",
            percent_encode(&chunk.transfer.vessel_urn),
            chunk.transfer.shard,
            chunk.transfer.version,
            chunk.chunk_index
        );
        let blake3_hex = hex_encode(&chunk.chunk_hash);
        let shard_hash_hex = hex_encode(&chunk.transfer.hash);
        let catalog_hash_hex = hex_encode(&chunk.transfer.catalog_hash);
        let total_chunks_str = chunk.total_chunks.to_string();
        let offset_str = chunk.offset.to_string();
        let total_bytes_str = chunk.total_bytes.to_string();
        let from_str = chunk.transfer.from.to_string();
        let to_str = chunk.transfer.to.to_string();
        let width_str = chunk.transfer.width_seconds.to_string();

        let headers = [
            ("Content-Type", "application/octet-stream"),
            ("X-BLAKE3", blake3_hex.as_str()),
            ("X-Total-Chunks", total_chunks_str.as_str()),
            ("X-Offset", offset_str.as_str()),
            ("X-Total-Bytes", total_bytes_str.as_str()),
            ("X-Shard-Hash", shard_hash_hex.as_str()),
            ("X-Catalog-Hash", catalog_hash_hex.as_str()),
            ("X-From", from_str.as_str()),
            ("X-To", to_str.as_str()),
            ("X-Width-Seconds", width_str.as_str()),
        ];

        let resp = self.http_request("POST", &path, &headers, &chunk.data)?;
        serde_json::from_slice(&resp)
            .map_err(|e| Error::Corrupt(format!("invalid chunk ack JSON: {e}")))
    }

    fn commit_upload(&self, transfer: &TransferIdentity) -> Result<ShardManifestEntry> {
        let path = format!(
            "/ti/shards/{}/{}/{}/commit",
            percent_encode(&transfer.vessel_urn),
            transfer.shard,
            transfer.version
        );
        let body = serde_json::to_vec(transfer)
            .map_err(|e| Error::InvalidInput(format!("failed to serialize transfer: {e}")))?;
        let shard_hash_hex = hex_encode(&transfer.hash);
        let headers = [
            ("Content-Type", "application/json"),
            ("X-Shard-Hash", shard_hash_hex.as_str()),
        ];
        let resp = self.http_request("POST", &path, &headers, &body)?;
        serde_json::from_slice(&resp)
            .map_err(|e| Error::Corrupt(format!("invalid commit JSON: {e}")))
    }
}

pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' || b == b'~' {
            out.push(b as char);
        } else {
            use std::fmt::Write;
            let _ = write!(out, "%{:02X}", b);
        }
    }
    out
}

pub fn percent_decode(s: &str) -> std::result::Result<String, String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return Err("invalid percent encoding".into());
            }
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).map_err(|e| e.to_string())?;
            let byte = u8::from_str_radix(hex, 16).map_err(|e| e.to_string())?;
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|e| e.to_string())
}

pub fn hex_encode(bytes: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(s, "{:02x}", b);
    }
    s
}

pub fn hex_decode_32(s: &str) -> std::result::Result<[u8; 32], String> {
    if s.len() != 64 {
        return Err(format!("expected 64 hex chars, got {}", s.len()));
    }
    let mut out = [0u8; 32];
    for (i, chunk) in s.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        let hex = std::str::from_utf8(chunk).map_err(|e| e.to_string())?;
        out[i] = u8::from_str_radix(hex, 16).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

/// Constant-time comparison of bearer tokens via blake3 hashes and byte-wise XOR fold.
pub fn constant_time_bearer_eq(client: &str, server: &str) -> bool {
    let ha = blake3::hash(client.as_bytes());
    let hb = blake3::hash(server.as_bytes());
    let mut diff = 0u8;
    for (x, y) in ha.as_bytes().iter().zip(hb.as_bytes().iter()) {
        diff |= x ^ y;
    }
    diff == 0
}
