//! Transport abstraction for shard shipping, with loopback and lossy implementations.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
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
