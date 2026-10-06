//! Durable manifest managing sealed shard versions and digests.
//!
//! Stored at `<root>/manifest.json` with atomic rename.
//! Exposes shard pruning and Arrow catalog table materialization.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use arrow_array::{
    Array, BooleanArray, RecordBatch, StringArray, TimestampSecondArray, UInt32Array, UInt64Array,
};
use ti_contracts::{
    shards_schema, BucketIx, Catalog, Error, Result, ShardKey, ShardManifestEntry, VesselOrd, EPOCH,
};

static TMP_COUNTER: AtomicU64 = AtomicU64::new(1);

fn atomic_write_json<T: serde::Serialize>(path: &Path, data: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let counter = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp_path = path.with_extension(format!("tmp.{}.{}", std::process::id(), counter));
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp_path)?;
        let json_bytes = serde_json::to_vec_pretty(data)
            .map_err(|e| Error::Corrupt(format!("json serialization error: {}", e)))?;
        file.write_all(&json_bytes)?;
        file.flush()?;
        file.sync_all()?;
    }
    fs::rename(&tmp_path, path)?;
    Ok(())
}

fn read_manifest(path: &Path) -> Result<Vec<ShardManifestEntry>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = File::open(path)?;
    let entries: Vec<ShardManifestEntry> = serde_json::from_reader(file)
        .map_err(|e| Error::Corrupt(format!("Failed to parse manifest: {}", e)))?;
    Ok(entries)
}

pub struct Manifest {
    path: PathBuf,
    entries: Mutex<BTreeMap<ShardKey, ShardManifestEntry>>,
}

impl Manifest {
    /// Open existing `manifest.json` or create a new empty one.
    pub fn open_or_create(store_root: &Path) -> Result<Self> {
        let path = store_root.join("manifest.json");
        let list = read_manifest(&path)?;
        let mut map = BTreeMap::new();
        for entry in list {
            map.insert(entry.key, entry);
        }
        Ok(Self {
            path,
            entries: Mutex::new(map),
        })
    }

    /// Read an entry for a shard key.
    pub fn get(&self, key: ShardKey) -> Option<ShardManifestEntry> {
        self.entries.lock().unwrap().get(&key).cloned()
    }

    /// Upsert a shard entry and atomically write to disk.
    pub fn upsert(&self, entry: ShardManifestEntry) -> Result<()> {
        let mut map = self.entries.lock().unwrap();
        map.insert(entry.key, entry);
        let list: Vec<ShardManifestEntry> = map.values().cloned().collect();
        atomic_write_json(&self.path, &list)?;
        Ok(())
    }

    /// Return all manifest entries.
    pub fn entries(&self) -> Vec<ShardManifestEntry> {
        self.entries.lock().unwrap().values().cloned().collect()
    }

    /// Prune candidate sealed shards by vessel and bucket range [from, to].
    pub fn prune(
        &self,
        vessels: Option<&[VesselOrd]>,
        from: BucketIx,
        to: BucketIx,
    ) -> Vec<ShardKey> {
        if from > to {
            return Vec::new();
        }
        let map = self.entries.lock().unwrap();
        map.values()
            .filter(|e| {
                if let Some(vs) = vessels {
                    if !vs.contains(&e.key.vessel) {
                        return false;
                    }
                }
                !(e.from > to || e.to < from)
            })
            .map(|e| e.key)
            .collect()
    }

    /// Materialize the `shards` catalog table as an Arrow RecordBatch.
    pub fn shards_record_batch(
        &self,
        catalog: &dyn Catalog,
        width_seconds: u64,
    ) -> Result<RecordBatch> {
        let entries = self.entries();
        let mut vessel_col = Vec::with_capacity(entries.len());
        let mut shard_no_col = Vec::with_capacity(entries.len());
        let mut ts_from_col = Vec::with_capacity(entries.len());
        let mut ts_to_col = Vec::with_capacity(entries.len());
        let mut sealed_col = Vec::with_capacity(entries.len());
        let mut bytes_col = Vec::with_capacity(entries.len());
        let mut hash_col: Vec<Option<String>> = Vec::with_capacity(entries.len());

        for e in entries {
            let urn = catalog.vessel_urn(e.key.vessel)?;
            let ts_from = EPOCH + (e.from as i64) * (width_seconds as i64);
            // ts_to is exclusive per spec 14 §71
            let ts_to = EPOCH + (e.to as i64 + 1) * (width_seconds as i64);
            let hex_hash = hex::encode(e.hash);

            vessel_col.push(urn);
            shard_no_col.push(e.key.shard);
            ts_from_col.push(ts_from);
            ts_to_col.push(ts_to);
            sealed_col.push(true);
            bytes_col.push(e.bytes);
            hash_col.push(Some(hex_hash));
        }

        let hash_refs: Vec<Option<&str>> = hash_col.iter().map(|h| h.as_deref()).collect();

        let columns: Vec<Arc<dyn Array>> = vec![
            Arc::new(StringArray::from(vessel_col)),
            Arc::new(UInt32Array::from(shard_no_col)),
            Arc::new(TimestampSecondArray::from(ts_from_col).with_timezone("UTC")),
            Arc::new(TimestampSecondArray::from(ts_to_col).with_timezone("UTC")),
            Arc::new(BooleanArray::from(sealed_col)),
            Arc::new(UInt64Array::from(bytes_col)),
            Arc::new(StringArray::from(hash_refs)),
        ];

        RecordBatch::try_new(shards_schema(), columns).map_err(Error::Arrow)
    }
}

mod hex {
    pub fn encode(data: [u8; 32]) -> String {
        const HEX_CHARS: &[u8; 16] = b"0123456789abcdef";
        let mut v = Vec::with_capacity(64);
        for &byte in &data {
            v.push(HEX_CHARS[(byte >> 4) as usize]);
            v.push(HEX_CHARS[(byte & 0xf) as usize]);
        }
        String::from_utf8(v).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_manifest_upsert_reopen_and_prune() {
        let dir = tempdir().unwrap();
        let manifest = Manifest::open_or_create(dir.path()).unwrap();

        let entry1 = ShardManifestEntry {
            key: ShardKey {
                vessel: 0,
                shard: 0,
            },
            version: 1,
            from: 0,
            to: 1000,
            bytes: 4096,
            hash: [1u8; 32],
        };

        let entry2 = ShardManifestEntry {
            key: ShardKey {
                vessel: 0,
                shard: 1,
            },
            version: 1,
            from: 65536,
            to: 66000,
            bytes: 8192,
            hash: [2u8; 32],
        };

        manifest.upsert(entry1.clone()).unwrap();
        manifest.upsert(entry2.clone()).unwrap();

        assert_eq!(manifest.get(entry1.key).unwrap(), entry1);
        assert_eq!(manifest.get(entry2.key).unwrap(), entry2);

        // Pruning tests
        let hit = manifest.prune(Some(&[0]), 500, 500);
        assert_eq!(hit, vec![entry1.key]);

        let hit_both = manifest.prune(None, 0, 70000);
        assert_eq!(hit_both.len(), 2);

        let miss = manifest.prune(Some(&[1]), 0, 100000);
        assert!(miss.is_empty());

        drop(manifest);

        // Re-open from disk
        let manifest2 = Manifest::open_or_create(dir.path()).unwrap();
        assert_eq!(manifest2.entries().len(), 2);
        assert_eq!(manifest2.get(entry1.key).unwrap(), entry1);
    }
}
