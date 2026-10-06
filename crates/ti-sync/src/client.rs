//! SyncClient orchestrating manifest diff, chunked uploads, retry and resumption.

use std::sync::{Arc, Mutex};
use ti_contracts::{Error, Result, ShardManifestEntry};
use ti_store::Store;

use crate::chunk::{chunk_package, UploadStatus, DEFAULT_CHUNK_SIZE};
use crate::diff::{diff_manifests_with_resolver, MissingShard};
use crate::package::package_sealed_shard;
use crate::transport::Transport;

/// Summary statistics of a synchronization run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub shards_synced: usize,
    pub bytes_uploaded: u64,
    pub chunks_sent: u64,
    pub retries: u64,
}

/// Boat-side synchronization client uploading missing sealed shards to a shore node.
pub struct SyncClient<T: Transport> {
    store: Arc<Mutex<Store>>,
    transport: T,
    chunk_size: usize,
    max_retries: usize,
    link_budget: Option<u64>,
    idle_priority: bool,
}

impl<T: Transport> SyncClient<T> {
    pub fn new(store: Arc<Mutex<Store>>, transport: T) -> Self {
        Self {
            store,
            transport,
            chunk_size: DEFAULT_CHUNK_SIZE,
            max_retries: 100,
            link_budget: None,
            idle_priority: false,
        }
    }

    pub fn with_chunk_size(mut self, size: usize) -> Self {
        self.chunk_size = size.max(1);
        self
    }

    pub fn with_max_retries(mut self, retries: usize) -> Self {
        self.max_retries = retries;
        self
    }

    pub fn with_link_budget(mut self, budget: Option<u64>) -> Self {
        self.link_budget = budget;
        self
    }

    pub fn with_idle_priority(mut self, idle: bool) -> Self {
        self.idle_priority = idle;
        self
    }

    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// Query the shore node and return all missing or repaired shards.
    pub fn diff(&self) -> Result<Vec<MissingShard>> {
        let store = self.store.lock().unwrap();
        let local_manifest = store.manifest().entries();
        let local_catalog = store.catalog().clone();
        drop(store);

        let shore_manifest = self.transport.fetch_manifest()?;
        diff_manifests_with_resolver(
            &local_manifest,
            local_catalog.as_ref(),
            &shore_manifest,
            |ord| self.transport.resolve_vessel_urn(ord),
        )
    }

    /// Synchronize a single missing shard with resumable chunked upload.
    pub fn sync_shard(
        &self,
        missing: &MissingShard,
        report: &mut SyncReport,
    ) -> Result<ShardManifestEntry> {
        let package = {
            let store = self.store.lock().unwrap();
            let entry = store.manifest().get(missing.local_key).ok_or_else(|| {
                Error::NotFound(format!("local shard not found: {:?}", missing.local_key))
            })?;
            package_sealed_shard(
                store.root(),
                missing.local_key.vessel,
                missing.local_key.shard,
                missing.version,
                store.catalog().as_ref(),
                store.width_seconds(),
                &entry,
            )?
        };

        let chunks = chunk_package(&package, self.chunk_size);

        // Resume from last acknowledged chunk if upload is already in progress
        let mut acknowledged = Vec::new();
        let mut status_retries = 0;
        loop {
            match self.transport.upload_status(&package.transfer) {
                Ok(UploadStatus::Completed { manifest_entry }) => {
                    report.shards_synced += 1;
                    return Ok(manifest_entry);
                }
                Ok(UploadStatus::InProgress {
                    acknowledged_chunks,
                    ..
                }) => {
                    acknowledged = acknowledged_chunks;
                    break;
                }
                Ok(UploadStatus::NotStarted) => {
                    break;
                }
                Err(e) => {
                    status_retries += 1;
                    report.retries += 1;
                    if status_retries >= self.max_retries {
                        return Err(e);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            }
        }

        // Upload missing chunks with retry
        for chunk in &chunks {
            if acknowledged.contains(&chunk.chunk_index) {
                continue;
            }

            if let Some(budget) = self.link_budget {
                if report.bytes_uploaded + (chunk.data.len() as u64) > budget {
                    return Err(Error::Unsupported("link budget exhausted".into()));
                }
            }
            if self.idle_priority {
                std::thread::yield_now();
            }

            let mut chunk_retries = 0;
            loop {
                match self.transport.send_chunk(chunk) {
                    Ok(ack) => {
                        report.chunks_sent += 1;
                        report.bytes_uploaded += chunk.data.len() as u64;
                        if ack.is_complete {
                            break;
                        }
                        break;
                    }
                    Err(e) => {
                        chunk_retries += 1;
                        report.retries += 1;
                        if chunk_retries >= self.max_retries {
                            return Err(e);
                        }
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                }
            }
        }

        // Commit upload on shore
        let mut commit_retries = 0;
        loop {
            match self.transport.commit_upload(&package.transfer) {
                Ok(entry) => {
                    report.shards_synced += 1;
                    return Ok(entry);
                }
                Err(e) => {
                    commit_retries += 1;
                    report.retries += 1;
                    if commit_retries >= self.max_retries {
                        return Err(e);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            }
        }
    }

    /// Synchronize all missing shards from local store to shore.
    pub fn sync_all(&self) -> Result<SyncReport> {
        let missing = self.diff()?;
        let mut report = SyncReport::default();
        for shard in &missing {
            self.sync_shard(shard, &mut report)?;
        }
        Ok(report)
    }
}
