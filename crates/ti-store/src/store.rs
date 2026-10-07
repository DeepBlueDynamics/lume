//! Top-level storage engine implementing `ShardSink` and `ShardSource`.
//!
//! Ties together:
//! - Catalog persistence (`DiskCatalog`)
//! - Shard manifest (`Manifest`)
//! - Write-Ahead Log (`Wal`) per vessel
//! - In-memory open shards (`OpenShard`) with flush and seal
//! - Immutable sealed shards (`SealedShard`)
//! - Group commit and crash-recovery replay

use crate::cache::{self, CacheKey, Cached, FieldCache};
use crate::shard::ShardView;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use arrow_array::RecordBatch;
use ti_contracts::{
    AggOp, AggPartial, BucketIx, BucketRecord, Catalog, Error, FieldValue, Predicate, Result,
    RoaringBitmap, ShardKey, ShardManifestEntry, ShardSink, ShardSource, TextIndex, VesselOrd,
    EPOCH,
};

use crate::catalog::DiskCatalog;
use crate::manifest::Manifest;
use crate::shard::{OpenShard, SealedShard};
use crate::wal::Wal;

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct RetentionState {
    cutoff: Option<i64>,
    dropped_late_records: u64,
    #[serde(default)]
    active_open: Vec<ShardKey>,
}

pub struct Store {
    root: PathBuf,
    width_seconds: u64,
    catalog: Arc<DiskCatalog>,
    manifest: Arc<Manifest>,
    wals: BTreeMap<VesselOrd, Wal>,
    open_shards: BTreeMap<ShardKey, OpenShard>,
    sealed_shards: BTreeMap<ShardKey, SealedShard>,
    text_index: Option<Arc<dyn TextIndex>>,
    retention: RetentionState,
    query_cache: Arc<Mutex<FieldCache>>,
}

impl Store {
    /// Open a store strictly read-only for querying.
    /// Does not open or replay WAL files, does not truncate files, and performs no disk writes.
    pub fn open_readonly(root: &Path, width_seconds: u64) -> Result<Self> {
        Self::open_readonly_with_cache(root, width_seconds, cache::configured_budget(root)?)
    }
    /// Explicit cache budget for read-only performance comparisons; never edits ti.toml.
    pub fn open_readonly_with_cache(
        root: &Path,
        width_seconds: u64,
        cache_bytes: u64,
    ) -> Result<Self> {
        let catalog = Arc::new(DiskCatalog::open_or_create(root)?);
        let manifest = Arc::new(Manifest::open_or_create(root)?);

        let retention_path = root.join("retention.json");
        let retention: RetentionState = if retention_path.exists() {
            serde_json::from_slice(&fs::read(retention_path)?)
                .map_err(|e| Error::Corrupt(format!("retention state: {e}")))?
        } else {
            RetentionState::default()
        };

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
                                        let key = open_shard.data.key;
                                        let end = i128::from(EPOCH)
                                            + (i128::from(key.shard) + 1)
                                                * 65536
                                                * i128::from(width_seconds);
                                        let mut restored = if retention
                                            .cutoff
                                            .is_some_and(|cutoff| end <= i128::from(cutoff))
                                        {
                                            OpenShard::new(key)
                                        } else {
                                            Self::restore_open(
                                                root,
                                                key,
                                                catalog.as_ref(),
                                                &manifest,
                                            )?
                                        };
                                        restored.data.fields.extend(open_shard.data.fields);
                                        restored.data.specs.extend(open_shard.data.specs);
                                        restored.has_data = true;
                                        open_shards.insert(key, restored);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(Self {
            root: root.to_path_buf(),
            width_seconds,
            catalog,
            manifest,
            wals: BTreeMap::new(),
            open_shards,
            sealed_shards: BTreeMap::new(),
            text_index: None,
            retention,
            query_cache: cache::shared(root, cache_bytes)?,
        })
    }

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

        let retention_path = root.join("retention.json");
        let mut retention: RetentionState = if retention_path.exists() {
            serde_json::from_slice(&fs::read(retention_path)?)
                .map_err(|e| Error::Corrupt(format!("retention state: {e}")))?
        } else {
            RetentionState::default()
        };
        let previous_dropped = retention.dropped_late_records;
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
                                        let key = open_shard.data.key;
                                        let end = i128::from(EPOCH)
                                            + (i128::from(key.shard) + 1)
                                                * 65536
                                                * i128::from(width_seconds);
                                        let mut restored = if retention
                                            .cutoff
                                            .is_some_and(|cutoff| end <= i128::from(cutoff))
                                        {
                                            OpenShard::new(key)
                                        } else {
                                            Self::restore_open(
                                                root,
                                                key,
                                                catalog.as_ref(),
                                                &manifest,
                                            )?
                                        };
                                        restored.data.fields.extend(open_shard.data.fields);
                                        restored.data.specs.extend(open_shard.data.specs);
                                        restored.has_data = true;
                                        open_shards.insert(key, restored);
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
                let mut by_shard: BTreeMap<ShardKey, Vec<BucketRecord>> = BTreeMap::new();
                for rec in batch {
                    let replay_key = ShardKey {
                        vessel: rec.vessel,
                        shard: rec.bucket >> 16,
                    };
                    if Self::record_expired(&rec, width_seconds, retention.cutoff)
                        && !retention.active_open.contains(&replay_key)
                    {
                        retention.dropped_late_records += 1;
                        continue;
                    }
                    let key = ShardKey {
                        vessel: rec.vessel,
                        shard: rec.bucket >> 16,
                    };
                    if let std::collections::btree_map::Entry::Vacant(entry) =
                        open_shards.entry(key)
                    {
                        entry.insert(Self::restore_open(root, key, catalog.as_ref(), &manifest)?);
                    }
                    let shard = open_shards.get_mut(&key).expect("restored shard");
                    shard.register_field(catalog.field(rec.field)?)?;
                    if let FieldValue::SetValue(row_id) = rec.value {
                        shard.register_set_value(
                            rec.field,
                            row_id,
                            &catalog.set_value(rec.field, row_id)?,
                        )?;
                    }
                    by_shard.entry(key).or_default().push(rec);
                }
                for (key, records) in by_shard {
                    open_shards
                        .get_mut(&key)
                        .expect("restored shard")
                        .apply(&records)?;
                }
            }

            vessel_ord += 1;
        }

        if retention.dropped_late_records != previous_dropped {
            crate::catalog::atomic_write_json(&root.join("retention.json"), &retention)?;
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
            retention,
            query_cache: cache::shared(root, cache::configured_budget(root)?)?,
        })
    }

    fn record_expired(record: &BucketRecord, width: u64, cutoff: Option<i64>) -> bool {
        let end = i128::from(EPOCH) + (i128::from(record.bucket) + 1) * i128::from(width);
        cutoff.is_some_and(|cutoff| end <= i128::from(cutoff))
    }

    /// Late records rejected after a persisted retention cutoff has been installed.
    pub fn dropped_late_records(&self) -> u64 {
        self.retention.dropped_late_records
    }

    fn restore_open(
        root: &Path,
        key: ShardKey,
        catalog: &dyn Catalog,
        manifest: &Manifest,
    ) -> Result<OpenShard> {
        if let Some(entry) = manifest.get(key) {
            let sealed = SealedShard::load(root, key.vessel, key.shard, entry.version, catalog)?;
            let has_data = !sealed.data.universe().is_empty();
            Ok(OpenShard {
                data: sealed.data,
                dirty: false,
                has_data,
                dirty_fields: Default::default(),
            })
        } else {
            Ok(OpenShard::new(key))
        }
    }

    /// Retained cache charge excludes active query batches and temporary cold decode buffers.
    pub fn query_cache_stats(&self) -> Result<crate::QueryCacheStats> {
        Ok(cache::lock(&self.query_cache)?.stats())
    }
    fn with_sealed_view<R>(
        &self,
        shard: ShardKey,
        needed: &[u32],
        use_view: impl FnOnce(&ShardView<'_>) -> Result<R>,
    ) -> Result<R> {
        let entry = self
            .manifest
            .get(shard)
            .ok_or_else(|| Error::NotFound(format!("shard {shard:?}")))?;
        if cache::lock(&self.query_cache)?.stats().budget_bytes == 0 {
            let sealed = SealedShard::load(
                &self.root,
                shard.vessel,
                shard.shard,
                entry.version,
                self.catalog.as_ref(),
            )?;
            return use_view(&sealed.data.view());
        }
        let universe_key = CacheKey::new(&entry, None);
        let cached_universe = {
            let mut cache = cache::lock(&self.query_cache)?;
            cache.retain_version(&entry);
            cache.get(universe_key)
        };
        let mut decoded = None;
        let universe = if let Some(universe) = cached_universe {
            universe
        } else {
            cache::lock(&self.query_cache)?.full_load();
            let sealed = SealedShard::load(
                &self.root,
                shard.vessel,
                shard.shard,
                entry.version,
                self.catalog.as_ref(),
            )?;
            let universe = Arc::new(Cached::Universe(sealed.data.universe()));
            cache::lock(&self.query_cache)?.insert(universe_key, Arc::clone(&universe));
            decoded = Some(sealed);
            universe
        };
        let mut owners = BTreeMap::new();
        let mut specs = BTreeMap::new();
        for id in needed
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
        {
            let key = CacheKey::new(&entry, Some(id));
            let cached_field = { cache::lock(&self.query_cache)?.get(key) };
            let value = if let Some(value) = cached_field {
                value
            } else {
                let field = if let Some(sealed) = decoded.as_mut() {
                    sealed.data.fields.remove(&id)
                } else {
                    cache::lock(&self.query_cache)?.field_load();
                    SealedShard::load_field(
                        &self.root,
                        shard,
                        entry.version,
                        id,
                        self.catalog.as_ref(),
                    )?
                    .map(|(_, field)| field)
                };
                let value = Arc::new(field.map(Cached::Field).unwrap_or(Cached::Missing));
                cache::lock(&self.query_cache)?.insert(key, Arc::clone(&value));
                value
            };
            if matches!(value.as_ref(), Cached::Field(_)) {
                specs.insert(id, self.catalog.field(id)?);
            }
            owners.insert(id, value);
        }
        drop(decoded);
        let fields = owners
            .iter()
            .filter_map(|(id, value)| match value.as_ref() {
                Cached::Field(field) => Some((*id, field)),
                _ => None,
            })
            .collect();
        let Cached::Universe(bitmap) = universe.as_ref() else {
            return Err(Error::Corrupt("invalid universe cache entry".into()));
        };
        use_view(&ShardView {
            key: shard,
            fields,
            specs: &specs,
            cached_universe: Some(bitmap),
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
        // Stage every dirty shard first so one sync covers the whole flush.
        let mut staged = Vec::new();
        for (key, shard) in &self.open_shards {
            if shard.dirty {
                let urn = self.catalog.vessel_urn(key.vessel)?;
                staged.extend(shard.stage_flush(&self.root, &urn, self.width_seconds)?);
            }
        }
        crate::shard::commit_staged(staged)?;
        for shard in self.open_shards.values_mut() {
            shard.mark_flushed();
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

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn width_seconds(&self) -> u64 {
        self.width_seconds
    }

    /// Drop a single shard completely from memory, manifest, and disk.
    pub fn drop_shard(&mut self, key: ShardKey) -> Result<()> {
        self.open_shards.remove(&key);
        self.sealed_shards.remove(&key);
        cache::lock(&self.query_cache)?.invalidate(key);
        self.manifest.remove(key)?;
        if self.retention.active_open.contains(&key) {
            self.retention.active_open.retain(|active| *active != key);
            crate::catalog::atomic_write_json(&self.root.join("retention.json"), &self.retention)?;
        }

        let shard_dir = self
            .root
            .join("shards")
            .join(key.vessel.to_string())
            .join(key.shard.to_string());
        if shard_dir.exists() {
            fs::remove_dir_all(&shard_dir)?;
        }
        Ok(())
    }

    /// Drop sealed and open shards whose data is strictly older than `retention_seconds`
    /// relative to `now_timestamp`.
    /// Returns the number of dropped shards.
    pub fn enforce_retention(
        &mut self,
        now_timestamp: i64,
        retention_seconds: u64,
    ) -> Result<usize> {
        let cutoff_ts = i64::try_from(i128::from(now_timestamp) - i128::from(retention_seconds))
            .map_err(|_| Error::Overflow("retention cutoff"))?;
        self.retention.cutoff = Some(
            self.retention
                .cutoff
                .map_or(cutoff_ts, |old| old.max(cutoff_ts)),
        );
        self.retention.active_open = self
            .open_shards
            .keys()
            .copied()
            .filter(|key| self.manifest.get(*key).is_none())
            .collect();
        crate::catalog::atomic_write_json(&self.root.join("retention.json"), &self.retention)?;
        let mut dropped = Vec::new();

        // Check sealed shards in manifest only: open shards are active and
        // must never be dropped while still open (to prevent resurrection on WAL replay).
        for entry in self.manifest.entries() {
            let end_ts = EPOCH + ((entry.to as i64 + 1) * (self.width_seconds as i64));
            if end_ts <= cutoff_ts {
                dropped.push(entry.key);
            }
        }

        let count = dropped.len();
        for key in dropped {
            self.drop_shard(key)?;
        }
        Ok(count)
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

        let retained;
        let recs = if self.retention.cutoff.is_some() {
            retained = recs
                .iter()
                .filter(|rec| {
                    !Self::record_expired(rec, self.width_seconds, self.retention.cutoff)
                        || self.retention.active_open.contains(&ShardKey {
                            vessel: rec.vessel,
                            shard: rec.bucket >> 16,
                        })
                })
                .cloned()
                .collect::<Vec<_>>();
            let dropped = recs.len() - retained.len();
            if dropped > 0 {
                self.retention.dropped_late_records += dropped as u64;
                crate::catalog::atomic_write_json(
                    &self.root.join("retention.json"),
                    &self.retention,
                )?;
            }
            retained.as_slice()
        } else {
            recs
        };
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
            if let std::collections::btree_map::Entry::Vacant(entry) = self.open_shards.entry(key) {
                entry.insert(Self::restore_open(
                    &self.root,
                    key,
                    self.catalog.as_ref(),
                    &self.manifest,
                )?);
            }
            let shard = self.open_shards.get_mut(&key).expect("restored shard");
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

        // Apply to in-memory open shards, one call per shard so each shard stages its
        // touched fields once per batch rather than once per record.
        let mut by_shard: BTreeMap<ShardKey, Vec<BucketRecord>> = BTreeMap::new();
        for rec in recs {
            let key = ShardKey {
                vessel: rec.vessel,
                shard: rec.bucket >> 16,
            };
            by_shard.entry(key).or_default().push(rec.clone());
        }
        for (key, shard_recs) in by_shard {
            let shard = self.open_shards.get_mut(&key).unwrap();
            shard.apply(&shard_recs)?;
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

        let mut shard = match self.open_shards.remove(&key) {
            Some(shard) => shard,
            None => Self::restore_open(&self.root, key, self.catalog.as_ref(), &self.manifest)?,
        };

        let entry = shard.seal_to(&self.root, &urn, next_version, self.width_seconds)?;
        self.manifest.upsert(entry.clone())?;
        if self.retention.active_open.contains(&key) {
            self.retention.active_open.retain(|active| *active != key);
            crate::catalog::atomic_write_json(&self.root.join("retention.json"), &self.retention)?;
        }
        // Keep WAL-replayable open staging until the manifest makes the new seal authoritative.
        let open_dir = self
            .root
            .join("shards")
            .join(key.vessel.to_string())
            .join(key.shard.to_string())
            .join("open");
        if open_dir.exists() {
            fs::remove_dir_all(open_dir)?;
        }

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
            if !(base > to || end < from || keys.contains(key)) {
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

        fn collect_fields(p: &Predicate, fields: &mut Vec<u32>) {
            match p {
                Predicate::Present(id)
                | Predicate::SetEq { field: id, .. }
                | Predicate::BsiCmp { field: id, .. }
                | Predicate::GeoCover { field: id, .. } => fields.push(*id),
                Predicate::And(children) | Predicate::Or(children) => {
                    for child in children {
                        collect_fields(child, fields);
                    }
                }
                Predicate::Not(child) => collect_fields(child, fields),
                _ => {}
            }
        }
        let mut fields = Vec::new();
        collect_fields(p, &mut fields);
        self.with_sealed_view(shard, &fields, |view| {
            Ok(view.eval_masks(p, self.text_index.as_deref())?.truth)
        })
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

        self.with_sealed_view(shard, fields, |view| {
            view.materialize(
                &urn,
                cols,
                fields,
                self.width_seconds,
                self.catalog.as_ref(),
            )
        })
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

        let fields = if a == AggOp::CountAll {
            Vec::new()
        } else {
            vec![field]
        };
        self.with_sealed_view(shard, &fields, |view| view.aggregate(cols, field, a))
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
