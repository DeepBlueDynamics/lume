//! Shore node receiver and import pipeline.
//!
//! Receives chunks, validates per-chunk BLAKE3 digests, enforces byte idempotence (§84),
//! reassembles packages, verifies canonical content hash (§81) and catalog hash (§82),
//! re-maps vessel ordinals by canonical URN via `remap_vessel`, and atomically installs
//! sealed shards into the shore store manifest.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::sync::{Arc, Mutex};
use ti_contracts::{
    Catalog, Error, Result, ShardKey, ShardManifestEntry, TransferIdentity, VesselSpec,
};
use ti_store::Store;

use crate::chunk::{ChunkAck, UploadChunk, UploadStatus};
use crate::package::unpack_and_verify_shard;

pub const MAX_PENDING_TRANSFERS: usize = 64;
pub const MAX_STAGING_BYTES: u64 = 256 * 1024 * 1024; // 256 MB
pub const MAX_SHARD_BYTES: u64 = 64 * 1024 * 1024; // 64 MB
pub const MAX_VESSEL_URN_LEN: usize = 512;
pub const DEFAULT_SESSION_TTL: std::time::Duration = std::time::Duration::from_secs(3600);

pub fn validate_transfer_urn(urn: &str) -> Result<()> {
    if urn.is_empty() || urn.len() > MAX_VESSEL_URN_LEN {
        return Err(Error::InvalidInput(format!(
            "vessel URN length must be between 1 and {MAX_VESSEL_URN_LEN} bytes"
        )));
    }
    ti_contracts::validate_entity_urn(urn)
}

type TransferKey = (String, u32, u64); // (vessel_urn, shard, version)

struct StagingSession {
    _transfer: TransferIdentity,
    total_bytes: u64,
    total_chunks: u32,
    chunks: BTreeMap<u32, (u64, Vec<u8>)>,
    completed_entry: Option<ShardManifestEntry>,
    last_activity: std::time::Instant,
}

/// Shore-side synchronization receiver attached to a shore `Store`.
pub struct ShoreReceiver {
    store: Arc<Mutex<Store>>,
    staging: Mutex<BTreeMap<TransferKey, StagingSession>>,
    max_pending_transfers: usize,
    max_staging_bytes: u64,
    session_ttl: std::time::Duration,
}

impl ShoreReceiver {
    pub fn new(store: Arc<Mutex<Store>>) -> Self {
        Self {
            store,
            staging: Mutex::new(BTreeMap::new()),
            max_pending_transfers: MAX_PENDING_TRANSFERS,
            max_staging_bytes: MAX_STAGING_BYTES,
            session_ttl: DEFAULT_SESSION_TTL,
        }
    }

    pub fn with_limits(
        mut self,
        max_pending_transfers: usize,
        max_staging_bytes: u64,
        session_ttl: std::time::Duration,
    ) -> Self {
        self.max_pending_transfers = max_pending_transfers;
        self.max_staging_bytes = max_staging_bytes;
        self.session_ttl = session_ttl;
        self
    }

    pub fn pending_transfers_count(&self) -> usize {
        self.staging.lock().unwrap().len()
    }

    pub fn staging_bytes_count(&self) -> u64 {
        self.staging
            .lock()
            .unwrap()
            .values()
            .map(|s| s.chunks.values().map(|(_, d)| d.len() as u64).sum::<u64>())
            .sum()
    }

    fn prune_expired(
        staging: &mut BTreeMap<TransferKey, StagingSession>,
        now: std::time::Instant,
        ttl: std::time::Duration,
    ) {
        staging.retain(|_, s| {
            s.completed_entry.is_some() || now.duration_since(s.last_activity) < ttl
        });
    }

    /// Access the underlying shore store.
    pub fn store(&self) -> &Arc<Mutex<Store>> {
        &self.store
    }

    fn transfer_key(t: &TransferIdentity) -> TransferKey {
        (t.vessel_urn.clone(), t.shard, t.version)
    }

    /// Query current status of an upload session.
    pub fn upload_status(&self, transfer: &TransferIdentity) -> Result<UploadStatus> {
        validate_transfer_urn(&transfer.vessel_urn)?;
        let key = Self::transfer_key(transfer);

        // 1. Check if already installed in store manifest
        {
            let store = self.store.lock().unwrap();
            let mut v_ord = 0u32;
            while let Ok(urn) = store.catalog().vessel_urn(v_ord) {
                if urn == transfer.vessel_urn {
                    let shard_key = ShardKey {
                        vessel: v_ord,
                        shard: transfer.shard,
                    };
                    if let Some(entry) = store.manifest().get(shard_key) {
                        if entry.version >= transfer.version && entry.hash == transfer.hash {
                            return Ok(UploadStatus::Completed {
                                manifest_entry: entry,
                            });
                        }
                    }
                    break;
                }
                v_ord += 1;
            }
        }

        // 2. Check in-progress staging
        let mut staging = self.staging.lock().unwrap();
        Self::prune_expired(&mut staging, std::time::Instant::now(), self.session_ttl);
        match staging.get(&key) {
            None => Ok(UploadStatus::NotStarted),
            Some(session) => {
                if let Some(entry) = &session.completed_entry {
                    Ok(UploadStatus::Completed {
                        manifest_entry: entry.clone(),
                    })
                } else {
                    let acks: Vec<u32> = session.chunks.keys().copied().collect();
                    let received: u64 = session
                        .chunks
                        .values()
                        .map(|(_, data)| data.len() as u64)
                        .sum();
                    Ok(UploadStatus::InProgress {
                        acknowledged_chunks: acks,
                        bytes_received: received,
                        total_bytes: session.total_bytes,
                    })
                }
            }
        }
    }

    /// Receive and validate a single chunk.
    pub fn receive_chunk(&self, chunk: &UploadChunk) -> Result<ChunkAck> {
        validate_transfer_urn(&chunk.transfer.vessel_urn)?;
        chunk.validate()?;

        if chunk.total_bytes > MAX_SHARD_BYTES {
            return Err(Error::InvalidInput(format!(
                "transfer total_bytes ({}) exceeds maximum limit ({})",
                chunk.total_bytes, MAX_SHARD_BYTES
            )));
        }

        let key = Self::transfer_key(&chunk.transfer);
        let mut staging = self.staging.lock().unwrap();
        let now = std::time::Instant::now();
        Self::prune_expired(&mut staging, now, self.session_ttl);

        if !staging.contains_key(&key) && staging.len() >= self.max_pending_transfers {
            return Err(Error::InvalidInput(format!(
                "exceeded concurrent in-flight transfer limit (max {})",
                self.max_pending_transfers
            )));
        }

        let current_bytes: u64 = staging
            .values()
            .map(|s| s.chunks.values().map(|(_, d)| d.len() as u64).sum::<u64>())
            .sum();
        if current_bytes + chunk.data.len() as u64 > self.max_staging_bytes {
            return Err(Error::InvalidInput(format!(
                "exceeded in-flight staging byte capacity (max {} bytes)",
                self.max_staging_bytes
            )));
        }

        let session = staging.entry(key).or_insert_with(|| StagingSession {
            _transfer: chunk.transfer.clone(),
            total_bytes: chunk.total_bytes,
            total_chunks: chunk.total_chunks,
            chunks: BTreeMap::new(),
            completed_entry: None,
            last_activity: now,
        });
        session.last_activity = now;

        if session.completed_entry.is_some() {
            return Ok(ChunkAck {
                chunk_index: chunk.chunk_index,
                offset: chunk.offset,
                bytes_acknowledged: chunk.data.len() as u64,
                total_bytes: session.total_bytes,
                is_complete: true,
            });
        }

        // Enforce idempotence (§84): duplicate bytes are valid; differing bytes are errors.
        if let Some((existing_offset, existing_data)) = session.chunks.get(&chunk.chunk_index) {
            if *existing_offset != chunk.offset || existing_data != &chunk.data {
                return Err(Error::Corrupt(format!(
                    "conflicting payload received for chunk {} at offset {}",
                    chunk.chunk_index, chunk.offset
                )));
            }
        } else {
            session
                .chunks
                .insert(chunk.chunk_index, (chunk.offset, chunk.data.clone()));
        }

        let is_complete = session.chunks.len() == session.total_chunks as usize;
        let bytes_received: u64 = session
            .chunks
            .values()
            .map(|(_, data)| data.len() as u64)
            .sum();

        Ok(ChunkAck {
            chunk_index: chunk.chunk_index,
            offset: chunk.offset,
            bytes_acknowledged: bytes_received,
            total_bytes: session.total_bytes,
            is_complete,
        })
    }

    /// Commit and install an upload after all chunks have been received.
    pub fn commit_upload(&self, transfer: &TransferIdentity) -> Result<ShardManifestEntry> {
        validate_transfer_urn(&transfer.vessel_urn)?;
        let key = Self::transfer_key(transfer);

        // Check if already completed in store
        {
            let store = self.store.lock().unwrap();
            let mut v_ord = 0u32;
            while let Ok(urn) = store.catalog().vessel_urn(v_ord) {
                if urn == transfer.vessel_urn {
                    let shard_key = ShardKey {
                        vessel: v_ord,
                        shard: transfer.shard,
                    };
                    if let Some(entry) = store.manifest().get(shard_key) {
                        if entry.version >= transfer.version && entry.hash == transfer.hash {
                            return Ok(entry);
                        }
                    }
                    break;
                }
                v_ord += 1;
            }
        }

        // Reassemble tar bytes from staging
        let tar_bytes = {
            let staging = self.staging.lock().unwrap();
            let session = staging
                .get(&key)
                .ok_or_else(|| Error::NotFound("upload session not found".into()))?;

            if let Some(entry) = &session.completed_entry {
                return Ok(entry.clone());
            }

            if session.chunks.len() < session.total_chunks as usize {
                return Err(Error::InvalidInput(format!(
                    "cannot commit incomplete upload: {}/{} chunks received",
                    session.chunks.len(),
                    session.total_chunks
                )));
            }

            let mut assembled = Vec::with_capacity(session.total_bytes as usize);
            for chunk_idx in 0..session.total_chunks {
                let (_, chunk_data) = session.chunks.get(&chunk_idx).ok_or_else(|| {
                    Error::Corrupt(format!("missing chunk {chunk_idx} during commit"))
                })?;
                assembled.extend_from_slice(chunk_data);
            }
            assembled
        };

        // Cryptographically verify unpackaging (§81, §82)
        let unpacked = unpack_and_verify_shard(&tar_bytes)?;
        validate_transfer_urn(&unpacked.transfer.vessel_urn)?;

        // Install into shore store with vessel remapping
        let entry = {
            let store = self.store.lock().unwrap();

            // Re-map vessel ordinal by canonical URN (§83)
            let shore_vessel = store.catalog().register_vessel(&VesselSpec {
                urn: unpacked.transfer.vessel_urn.clone(),
                name: None,
                mmsi: None,
            })?;

            // Register catalog fields and dictionary entries, remapping local field IDs
            let mut field_map = BTreeMap::new();
            for field in &unpacked.catalog_snapshot.fields {
                let shore_field_id = store.catalog().register_field(field)?;
                field_map.insert(field.id, shore_field_id);
            }
            for dict in &unpacked.catalog_snapshot.dictionary {
                let shore_field_id = field_map.get(&dict.field).copied().unwrap_or(dict.field);
                let _ = store
                    .catalog()
                    .register_set_value(shore_field_id, &dict.value)?;
            }

            // Write versioned shard field files to disk
            let version_dir = store
                .root()
                .join("shards")
                .join(shore_vessel.to_string())
                .join(unpacked.transfer.shard.to_string())
                .join(format!("v{}", unpacked.transfer.version));
            fs::create_dir_all(&version_dir)?;

            let mut total_file_bytes = 0u64;
            for (field_id, data) in &unpacked.files {
                let shore_field_id = field_map.get(field_id).copied().unwrap_or(*field_id);
                let file_path = version_dir.join(format!("{shore_field_id}.rbm"));
                let tmp = version_dir.join(format!("{shore_field_id}.tmp.{}", std::process::id()));
                {
                    let mut f = OpenOptions::new()
                        .write(true)
                        .create(true)
                        .truncate(true)
                        .open(&tmp)?;
                    f.write_all(data)?;
                    f.flush()?;
                    f.sync_all()?;
                }
                fs::rename(&tmp, &file_path)?;
                total_file_bytes += data.len() as u64;
            }
            {
                if let Ok(dir_file) = fs::File::open(&version_dir) {
                    let _ = dir_file.sync_all();
                }
            }

            // Publish atomically to manifest
            let entry = ShardManifestEntry {
                key: ShardKey {
                    vessel: shore_vessel,
                    shard: unpacked.transfer.shard,
                },
                version: unpacked.transfer.version,
                from: unpacked.transfer.from,
                to: unpacked.transfer.to,
                bytes: total_file_bytes,
                hash: unpacked.transfer.hash,
            };
            store.manifest().upsert(entry.clone())?;
            entry
        };

        // Record completion in staging session
        let mut staging = self.staging.lock().unwrap();
        if let Some(session) = staging.get_mut(&key) {
            session.completed_entry = Some(entry.clone());
        }

        Ok(entry)
    }
}
