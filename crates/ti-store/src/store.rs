//! Top-level storage engine implementing `ShardSink` and `ShardSource`.
//!
//! Ties together:
//! - Catalog persistence (`DiskCatalog`)
//! - Shard manifest (`Manifest`)
//! - Write-Ahead Log (`Wal`) per vessel
//! - In-memory open shards (`OpenShard`) with flush and seal
//! - Immutable sealed shards (`SealedShard`)
//! - Group commit and crash-recovery replay

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow_array::RecordBatch;
use ti_contracts::{
    AggOp, AggPartial, BucketIx, BucketRecord, Catalog, Error, FieldValue, Predicate, Result,
    RoaringBitmap, ShardKey, ShardManifestEntry, ShardSink, ShardSource, TextIndex, VesselOrd,
};

use crate::catalog::DiskCatalog;
use crate::manifest::Manifest;
use crate::shard::{OpenShard, SealedShard};
use crate::wal::Wal;

pub struct Store {
    root: PathBuf,
    width_seconds: u64,
    catalog: Arc<DiskCatalog>,
    manifest: Arc<Manifest>,
    wals: BTreeMap<VesselOrd, Wal>,
    open_shards: BTreeMap<ShardKey, OpenShard>,
    sealed_shards: BTreeMap<ShardKey, SealedShard>,
    text_index: Option<Arc<dyn TextIndex>>,
}

impl Store {
    /// Open an existing store or initialize a new one in `root`.
    /// Replays any un-flushed WAL records automatically.
    pub fn open_or_create(root: &Path, width_seconds: u64) -> Result<Self> {
        fs::create_dir_all(root)?;
        let wal_dir = root.join("wal");
        fs::create_dir_all(&wal_dir)?;
        let shards_dir = root.join("shards");
        fs::create_dir_all(&shards_dir)?;

        let catalog = Arc::new(DiskCatalog::open_or_create(root)?);
        let manifest = Arc::new(Manifest::open_or_create(root)?);

        let mut wals = BTreeMap::new();
        let mut open_shards: BTreeMap<ShardKey, OpenShard> = BTreeMap::new();

        // Discover and load any flushed open shards on disk
        let shards_root = root.join("shards");
        if shards_root.exists() {
            if let Ok(entries) = fs::read_dir(&shards_root) {
                for v_entry in entries.flatten() {
                    if let Ok(v_ord) = v_entry.file_name().to_string_lossy().parse::<u32>() {
                        if let Ok(s_entries) = fs::read_dir(v_entry.path()) {
                            for s_entry in s_entries.flatten() {
                                if let Ok(s_no) =
                                    s_entry.file_name().to_string_lossy().parse::<u32>()
                                {
                                    if let Ok(Some(open_shard)) =
                                        OpenShard::load_open(root, v_ord, s_no, catalog.as_ref())
                                    {
                                        open_shards.insert(open_shard.data.key, open_shard);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Scan and open existing WAL files for each known vessel
        let mut vessel_ord = 0u32;
        while let Ok(urn) = catalog.vessel_urn(vessel_ord) {
            let (wal, replayed_batches) = Wal::open_or_create(&wal_dir, vessel_ord, &urn)?;
            wals.insert(vessel_ord, wal);

            for (_seq, batch) in replayed_batches {
                for rec in &batch {
                    let key = ShardKey {
                        vessel: rec.vessel,
                        shard: rec.bucket >> 16,
                    };
                    let shard = open_shards
                        .entry(key)
                        .or_insert_with(|| OpenShard::new(key));
                    if let Ok(spec) = catalog.field(rec.field) {
                        let _ = shard.register_field(spec);
                    }
                    if let FieldValue::SetValue(row_id) = rec.value {
                        if let Ok(val) = catalog.set_value(rec.field, row_id) {
                            let _ = shard.register_set_value(rec.field, row_id, &val);
                        }
                    }
                    let _ = shard.apply(std::slice::from_ref(rec));
                }
            }

            vessel_ord += 1;
        }

        Ok(Self {
            root: root.to_path_buf(),
            width_seconds,
            catalog,
            manifest,
            wals,
            open_shards,
            sealed_shards: BTreeMap::new(),
            text_index: None,
        })
    }

    pub fn with_text_index(mut self, text: Arc<dyn TextIndex>) -> Self {
        self.text_index = Some(text);
        self
    }

    pub fn catalog(&self) -> &Arc<DiskCatalog> {
        &self.catalog
    }

    pub fn manifest(&self) -> &Arc<Manifest> {
        &self.manifest
    }

    pub fn wal(&self, vessel: VesselOrd) -> Option<&Wal> {
        self.wals.get(&vessel)
    }

    /// Periodic tick called by ingest loop or timer to enforce group commit (D16).
    /// Fsyncs any WAL whose interval since last sync exceeds 1 s.
    pub fn tick(&mut self) -> Result<()> {
        for wal in self.wals.values_mut() {
            if wal.needs_sync() {
                wal.sync()?;
            }
        }
        Ok(())
    }

    /// Explicit shutdown: syncs all WALs durably to disk.
    pub fn shutdown(&mut self) -> Result<()> {
        for wal in self.wals.values_mut() {
            wal.sync()?;
        }
        Ok(())
    }

    pub fn open_shard(&self, key: &ShardKey) -> Option<&OpenShard> {
        self.open_shards.get(key)
    }

    pub fn sealed_shard(&self, key: &ShardKey) -> Option<&SealedShard> {
        self.sealed_shards.get(key)
    }

    /// Flush dirty open shards to disk.
    pub fn flush_shards(&mut self) -> Result<()> {
        for (key, shard) in &mut self.open_shards {
            if shard.dirty {
                let urn = self.catalog.vessel_urn(key.vessel)?;
                shard.flush_to(&self.root, &urn, self.width_seconds)?;
            }
        }
        Ok(())
    }

    /// Truncate all WALs after a flush.
    pub fn truncate_wals(&mut self) -> Result<()> {
        for wal in self.wals.values_mut() {
            wal.truncate_after_flush()?;
        }
        Ok(())
    }

    fn ensure_wal(&mut self, vessel: VesselOrd) -> Result<&mut Wal> {
        if !self.wals.contains_key(&vessel) {
            let urn = self.catalog.vessel_urn(vessel)?;
            let (wal, _) = Wal::open_or_create(&self.root.join("wal"), vessel, &urn)?;
            self.wals.insert(vessel, wal);
        }
        Ok(self.wals.get_mut(&vessel).unwrap())
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        for wal in self.wals.values_mut() {
            let _ = wal.sync();
        }
    }
}

impl ShardSink for Store {
    fn apply(&mut self, recs: &[BucketRecord]) -> Result<()> {
        if recs.is_empty() {
            return Ok(());
        }

        ti_contracts::validate_clear_records(recs)?;

        // Ensure fields exist in open shards
        for rec in recs {
            let spec = self.catalog.field(rec.field)?;
            let key = ShardKey {
                vessel: rec.vessel,
                shard: rec.bucket >> 16,
            };
            let shard = self
                .open_shards
                .entry(key)
                .or_insert_with(|| OpenShard::new(key));
            shard.register_field(spec)?;
            if let FieldValue::SetValue(row_id) = rec.value {
                if let Ok(val) = self.catalog.set_value(rec.field, row_id) {
                    let _ = shard.register_set_value(rec.field, row_id, &val);
                }
            }
        }

        // Group by vessel for WAL append
        let mut by_vessel: BTreeMap<VesselOrd, Vec<BucketRecord>> = BTreeMap::new();
        for rec in recs {
            by_vessel.entry(rec.vessel).or_default().push(rec.clone());
        }

        for (vessel, v_recs) in by_vessel {
            let wal = self.ensure_wal(vessel)?;
            wal.append(&v_recs)?;
            if wal.needs_sync() {
                wal.sync()?;
            }
        }

        // Apply to in-memory open shards
        for rec in recs {
            let key = ShardKey {
                vessel: rec.vessel,
                shard: rec.bucket >> 16,
            };
            let shard = self.open_shards.get_mut(&key).unwrap();
            shard.apply(std::slice::from_ref(rec))?;
        }

        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        self.flush_shards()?;
        self.truncate_wals()?;
        Ok(())
    }

    fn seal(&mut self, key: ShardKey) -> Result<ShardManifestEntry> {
        self.flush()?;

        let urn = self.catalog.vessel_urn(key.vessel)?;
        let next_version = match self.manifest.get(key) {
            Some(existing) => existing.version + 1,
            None => 1,
        };

        let mut shard = self
            .open_shards
            .remove(&key)
            .unwrap_or_else(|| OpenShard::new(key));

        let entry = shard.seal_to(&self.root, &urn, next_version, self.width_seconds)?;
        self.manifest.upsert(entry.clone())?;

        // Re-load into sealed_shards
        let sealed = SealedShard::load(
            &self.root,
            key.vessel,
            key.shard,
            entry.version,
            self.catalog.as_ref(),
        )?;
        self.sealed_shards.insert(key, sealed);

        Ok(entry)
    }
}

impl ShardSource for Store {
    fn shards(&self, vessels: Option<&[VesselOrd]>, from: BucketIx, to: BucketIx) -> Vec<ShardKey> {
        let mut keys = self.manifest.prune(vessels, from, to);

        // Also include open shards covering the range
        for key in self.open_shards.keys() {
            if let Some(vs) = vessels {
                if !vs.contains(&key.vessel) {
                    continue;
                }
            }
            let base = key.shard << 16;
            let end = base | 0xffff;
            if !(base > to || end < from) && !keys.contains(key) {
                keys.push(*key);
            }
        }

        keys.sort();
        keys.dedup();
        keys
    }

    fn eval(&self, shard: ShardKey, p: &Predicate) -> Result<RoaringBitmap> {
        if let Some(open) = self.open_shards.get(&shard) {
            return Ok(open.data.eval_masks(p, self.text_index.as_deref())?.truth);
        }

        let entry = self
            .manifest
            .get(shard)
            .ok_or_else(|| Error::NotFound(format!("shard {:?}", shard)))?;
        let sealed = SealedShard::load(
            &self.root,
            shard.vessel,
            shard.shard,
            entry.version,
            self.catalog.as_ref(),
        )?;
        Ok(sealed.data.eval_masks(p, self.text_index.as_deref())?.truth)
    }

    fn read(&self, shard: ShardKey, cols: &RoaringBitmap, fields: &[u32]) -> Result<RecordBatch> {
        let urn = self.catalog.vessel_urn(shard.vessel)?;

        if let Some(open) = self.open_shards.get(&shard) {
            return open.data.materialize(
                &urn,
                cols,
                fields,
                self.width_seconds,
                self.catalog.as_ref(),
            );
        }

        let entry = self
            .manifest
            .get(shard)
            .ok_or_else(|| Error::NotFound(format!("shard {:?}", shard)))?;
        let sealed = SealedShard::load(
            &self.root,
            shard.vessel,
            shard.shard,
            entry.version,
            self.catalog.as_ref(),
        )?;
        sealed.data.materialize(
            &urn,
            cols,
            fields,
            self.width_seconds,
            self.catalog.as_ref(),
        )
    }

    fn agg(
        &self,
        shard: ShardKey,
        cols: &RoaringBitmap,
        field: u32,
        a: AggOp,
    ) -> Result<AggPartial> {
        if let Some(open) = self.open_shards.get(&shard) {
            return open.data.aggregate(cols, field, a);
        }

        let entry = self
            .manifest
            .get(shard)
            .ok_or_else(|| Error::NotFound(format!("shard {:?}", shard)))?;
        let sealed = SealedShard::load(
            &self.root,
            shard.vessel,
            shard.shard,
            entry.version,
            self.catalog.as_ref(),
        )?;
        sealed.data.aggregate(cols, field, a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use ti_contracts::{Agg, CmpOp, FieldKind, FieldSpec, FieldValue, VesselSpec};

    #[test]
    fn test_store_apply_flush_seal_and_query() {
        let dir = tempdir().unwrap();
        let mut store = Store::open_or_create(dir.path(), 10).unwrap();

        // Register vessel
        let v0 = store
            .catalog()
            .register_vessel(&VesselSpec {
                urn: "vessels.urn:mrn:signalk:uuid:boat-1".into(),
                name: Some("Boat 1".into()),
                mmsi: None,
            })
            .unwrap();
        assert_eq!(v0, 0);

        // Register fields
        let f_speed = store
            .catalog()
            .register_field(&FieldSpec {
                id: 0,
                path: "navigation.speedOverGround".into(),
                agg: Some(Agg::Mean),
                kind: FieldKind::Bsi { scale: 3 },
                units: Some("m/s".into()),
            })
            .unwrap();

        let f_state = store
            .catalog()
            .register_field(&FieldSpec {
                id: 1,
                path: "navigation.state".into(),
                agg: None,
                kind: FieldKind::Set,
                units: None,
            })
            .unwrap();

        let row_sailing = store
            .catalog()
            .register_set_value(f_state, "sailing")
            .unwrap();

        let recs = vec![
            BucketRecord {
                vessel: v0,
                bucket: 10,
                field: f_speed,
                value: FieldValue::Int(6000), // 6.000 m/s
                rewrite: false,
            },
            BucketRecord {
                vessel: v0,
                bucket: 10,
                field: f_state,
                value: FieldValue::SetValue(row_sailing),
                rewrite: false,
            },
            BucketRecord {
                vessel: v0,
                bucket: 20,
                field: f_speed,
                value: FieldValue::Int(8000), // 8.000 m/s
                rewrite: false,
            },
        ];

        store.apply(&recs).unwrap();

        let shard_key = ShardKey {
            vessel: v0,
            shard: 0,
        };

        // Query open shard
        let shards = store.shards(None, 0, 100);
        assert_eq!(shards, vec![shard_key]);

        // Evaluate predicate: speed > 7000
        let pred = Predicate::BsiCmp {
            field: f_speed,
            op: CmpOp::Gt,
            lo: 7000,
            hi: None,
        };
        let matching = store.eval(shard_key, &pred).unwrap();
        assert_eq!(matching.iter().collect::<Vec<_>>(), vec![20]);

        // Aggregate: sum of speed
        let agg_sum = store
            .agg(shard_key, &matching, f_speed, AggOp::Sum)
            .unwrap();
        assert_eq!(
            agg_sum,
            AggPartial::Sum {
                sum: 8000,
                count: 1
            }
        );

        // Read batch
        let batch = store
            .read(shard_key, &matching, &[f_speed, f_state])
            .unwrap();
        assert_eq!(batch.num_rows(), 1);

        // Flush and Seal
        let entry = store.seal(shard_key).unwrap();
        assert_eq!(entry.version, 1);
        assert_ne!(entry.hash, [0u8; 32]);

        drop(store);

        // Re-open store and verify sealed query
        let store2 = Store::open_or_create(dir.path(), 10).unwrap();
        let shards2 = store2.shards(None, 0, 100);
        assert_eq!(shards2, vec![shard_key]);

        let matching2 = store2.eval(shard_key, &pred).unwrap();
        assert_eq!(matching2.iter().collect::<Vec<_>>(), vec![20]);
    }

    #[test]
    fn test_timer_group_commit_idle_sync() {
        let dir = tempdir().unwrap();
        let mut store = Store::open_or_create(dir.path(), 10).unwrap();

        let v0 = store
            .catalog()
            .register_vessel(&VesselSpec {
                urn: "vessels.urn:mrn:signalk:uuid:boat-timer".into(),
                name: Some("Boat Timer".into()),
                mmsi: None,
            })
            .unwrap();

        let f_speed = store
            .catalog()
            .register_field(&FieldSpec {
                id: 0,
                path: "navigation.speedOverGround".into(),
                agg: Some(Agg::Mean),
                kind: FieldKind::Bsi { scale: 3 },
                units: Some("m/s".into()),
            })
            .unwrap();

        let rec = vec![BucketRecord {
            vessel: v0,
            bucket: 1,
            field: f_speed,
            value: FieldValue::Int(5000),
            rewrite: false,
        }];

        store.apply(&rec).unwrap();

        let wal = store.wal(v0).expect("wal exists for v0");
        assert!(wal.has_unsynced());
        assert!(!wal.needs_sync());

        // Immediate tick should not sync since < 1s
        store.tick().unwrap();
        assert!(store.wal(v0).unwrap().has_unsynced());

        // Wait over 1s for group commit timer to elapse
        std::thread::sleep(std::time::Duration::from_millis(1050));
        assert!(store.wal(v0).unwrap().needs_sync());

        // Tick should now sync WAL
        store.tick().unwrap();
        let wal = store.wal(v0).unwrap();
        assert!(!wal.has_unsynced());
        assert!(wal.is_synced());
        assert!(!wal.needs_sync());

        // Second write followed by explicit shutdown
        let rec2 = vec![BucketRecord {
            vessel: v0,
            bucket: 2,
            field: f_speed,
            value: FieldValue::Int(7000),
            rewrite: false,
        }];
        store.apply(&rec2).unwrap();
        assert!(store.wal(v0).unwrap().has_unsynced());

        store.shutdown().unwrap();
        assert!(store.wal(v0).unwrap().is_synced());
    }
}
