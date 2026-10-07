//! Byte-bounded immutable field LRU, shared across same-root query snapshots.
use crate::row::FieldData;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};
use ti_contracts::{Error, Result, RoaringBitmap, ShardKey, ShardManifestEntry};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) struct CacheKey {
    pub shard: ShardKey,
    pub version: u64,
    pub hash: [u8; 32],
    /// None is the derived bucket universe, not a persisted field ID.
    pub field: Option<u32>,
}
impl CacheKey {
    pub fn new(entry: &ShardManifestEntry, field: Option<u32>) -> Self {
        Self {
            shard: entry.key,
            version: entry.version,
            hash: entry.hash,
            field,
        }
    }
}
pub(crate) enum Cached {
    Universe(RoaringBitmap),
    Field(FieldData),
    Missing,
}
impl Cached {
    fn bytes(&self) -> usize {
        // Include Arc, key, LRU indexes, B-tree node slack and enum storage.
        4096usize.saturating_add(match self {
            Self::Universe(bitmap) => bitmap_bytes(bitmap),
            Self::Field(field) => field_bytes(field),
            Self::Missing => 0,
        })
    }
}
fn bitmap_bytes(bitmap: &RoaringBitmap) -> usize {
    if bitmap.is_empty() {
        return std::mem::size_of::<RoaringBitmap>() + 64;
    }
    // Local shard rows have at most one 16-bit container. Allow double the
    // portable allocation plus a full 8-KiB bitset for run-to-bitset expansion.
    bitmap
        .serialized_size()
        .saturating_mul(2)
        .saturating_add(8192 + 256)
}
fn field_bytes(field: &FieldData) -> usize {
    let mut bytes = std::mem::size_of::<FieldData>();
    let mut add = |bitmap: &RoaringBitmap| {
        bytes = bytes.saturating_add(bitmap_bytes(bitmap));
    };
    match field {
        FieldData::Presence(row) => add(row.bitmap()),
        FieldData::Bsi(row) => {
            add(row.exists());
            add(row.sign());
            for bit in row.bits() {
                add(bit);
            }
        }
        FieldData::Count(row) => {
            add(row.exists());
            for bit in row.bits() {
                add(bit);
            }
        }
        FieldData::Set(row) => {
            add(row.presence());
            for bitmap in row.rows().values() {
                add(bitmap);
            }
            bytes = bytes.saturating_add(row.rows().len().saturating_mul(128));
            for word in row.dictionary().keys() {
                bytes = bytes.saturating_add(word.capacity()).saturating_add(160);
            }
        }
        FieldData::Geo(row) => {
            add(row.presence());
            for bitmap in row.rows().values() {
                add(bitmap);
            }
            bytes = bytes.saturating_add(row.rows().len().saturating_mul(128));
        }
    }
    bytes
}
#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct QueryCacheStats {
    pub budget_bytes: usize,
    pub used_bytes: usize,
    pub entries: usize,
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub full_loads: u64,
    pub field_loads: u64,
}
struct Item {
    value: Arc<Cached>,
    bytes: usize,
    stamp: u128,
}
pub(crate) struct FieldCache {
    items: BTreeMap<CacheKey, Item>,
    order: BTreeMap<u128, CacheKey>,
    clock: u128,
    stats: QueryCacheStats,
}
impl FieldCache {
    fn new(budget: usize) -> Self {
        Self {
            items: BTreeMap::new(),
            order: BTreeMap::new(),
            clock: 0,
            stats: QueryCacheStats {
                budget_bytes: budget,
                used_bytes: 0,
                entries: 0,
                hits: 0,
                misses: 0,
                evictions: 0,
                full_loads: 0,
                field_loads: 0,
            },
        }
    }
    fn stamp(&mut self) -> u128 {
        self.clock += 1;
        self.clock
    }
    fn remove(&mut self, key: CacheKey) {
        if let Some(item) = self.items.remove(&key) {
            self.order.remove(&item.stamp);
            self.stats.used_bytes -= item.bytes;
        }
    }
    fn evict(&mut self) {
        if let Some((_, key)) = self.order.first_key_value() {
            let key = *key;
            self.remove(key);
            self.stats.evictions = self.stats.evictions.saturating_add(1);
        }
    }
    pub fn set_budget(&mut self, bytes: usize) {
        self.stats.budget_bytes = bytes;
        while self.stats.used_bytes > bytes {
            self.evict();
        }
    }
    pub fn retain_version(&mut self, entry: &ShardManifestEntry) {
        let stale: Vec<_> = self
            .items
            .keys()
            .filter(|key| {
                key.shard == entry.key && (key.version != entry.version || key.hash != entry.hash)
            })
            .copied()
            .collect();
        for key in stale {
            self.remove(key);
        }
    }
    pub fn invalidate(&mut self, shard: ShardKey) {
        let stale: Vec<_> = self
            .items
            .keys()
            .filter(|key| key.shard == shard)
            .copied()
            .collect();
        for key in stale {
            self.remove(key);
        }
    }
    pub fn get(&mut self, key: CacheKey) -> Option<Arc<Cached>> {
        if let Some(item) = self.items.get(&key) {
            let value = Arc::clone(&item.value);
            let old = item.stamp;
            self.order.remove(&old);
            let stamp = self.stamp();
            self.items
                .get_mut(&key)
                .expect("existing cache entry")
                .stamp = stamp;
            self.order.insert(stamp, key);
            self.stats.hits = self.stats.hits.saturating_add(1);
            Some(value)
        } else {
            self.stats.misses = self.stats.misses.saturating_add(1);
            None
        }
    }
    pub fn insert(&mut self, key: CacheKey, value: Arc<Cached>) {
        let bytes = value.bytes();
        self.remove(key);
        if bytes > self.stats.budget_bytes {
            return;
        }
        while self.stats.used_bytes > self.stats.budget_bytes - bytes {
            self.evict();
        }
        let stamp = self.stamp();
        self.items.insert(
            key,
            Item {
                value,
                bytes,
                stamp,
            },
        );
        self.order.insert(stamp, key);
        self.stats.used_bytes += bytes;
    }
    pub fn full_load(&mut self) {
        self.stats.full_loads = self.stats.full_loads.saturating_add(1);
    }
    pub fn field_load(&mut self) {
        self.stats.field_loads = self.stats.field_loads.saturating_add(1);
    }
    pub fn stats(&self) -> QueryCacheStats {
        QueryCacheStats {
            entries: self.items.len(),
            ..self.stats
        }
    }
}
type Registry = BTreeMap<PathBuf, Weak<Mutex<FieldCache>>>;
static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
pub(crate) fn lock(cache: &Mutex<FieldCache>) -> Result<std::sync::MutexGuard<'_, FieldCache>> {
    cache
        .lock()
        .map_err(|_| Error::Corrupt("sealed-field cache lock poisoned".into()))
}
pub(crate) fn shared(root: &Path, budget: u64) -> Result<Arc<Mutex<FieldCache>>> {
    let root = root.canonicalize()?;
    let budget = usize::try_from(budget)
        .map_err(|_| Error::InvalidInput("sealed_cache_bytes exceeds address space".into()))?;
    let mut registry = REGISTRY
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .map_err(|_| Error::Corrupt("sealed-field cache registry lock poisoned".into()))?;
    registry.retain(|_, cache| cache.strong_count() > 0);
    if let Some(cache) = registry.get(&root).and_then(Weak::upgrade) {
        lock(&cache)?.set_budget(budget);
        return Ok(cache);
    }
    let cache = Arc::new(Mutex::new(FieldCache::new(budget)));
    registry.insert(root, Arc::downgrade(&cache));
    Ok(cache)
}
pub(crate) fn configured_budget(root: &Path) -> Result<u64> {
    let path = root.join("ti.toml");
    let query = if path.exists() {
        ti_contracts::TiConfig::from_toml(&std::fs::read_to_string(path)?)?.query
    } else {
        ti_contracts::QueryLimits::default()
    };
    Ok(query.sealed_cache_bytes)
}

/// Explicit controls for reproducible cold/warm query measurements.
#[derive(Clone)]
pub struct QueryCacheControl(Arc<Mutex<FieldCache>>);
impl QueryCacheControl {
    /// Attach to the same cache used by opened Store snapshots.
    pub fn open(root: &Path) -> Result<Self> {
        Ok(Self(shared(root, configured_budget(root)?)?))
    }
    pub fn set_budget(&self, bytes: u64) -> Result<()> {
        let bytes = usize::try_from(bytes)
            .map_err(|_| Error::InvalidInput("sealed_cache_bytes exceeds address space".into()))?;
        lock(&self.0)?.set_budget(bytes);
        Ok(())
    }
    /// Clear decoded entries only; never touches shard files or manifest state.
    pub fn clear(&self) -> Result<()> {
        let mut cache = lock(&self.0)?;
        cache.items.clear();
        cache.order.clear();
        cache.stats.used_bytes = 0;
        Ok(())
    }
    pub fn stats(&self) -> Result<QueryCacheStats> {
        Ok(lock(&self.0)?.stats())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn entry(vessel: u32, version: u64) -> ShardManifestEntry {
        ShardManifestEntry {
            key: ShardKey { vessel, shard: 0 },
            version,
            from: 0,
            to: 65535,
            bytes: 0,
            hash: [version as u8; 32],
        }
    }
    #[test]
    fn lru_hits_refresh_recency_and_budget_is_never_exceeded() {
        let mut cache = FieldCache::new(8192);
        let a = CacheKey::new(&entry(0, 1), Some(1));
        let b = CacheKey::new(&entry(0, 1), Some(2));
        let c = CacheKey::new(&entry(0, 1), Some(3));
        let shared = Arc::new(Cached::Missing);
        cache.insert(a, shared.clone());
        cache.insert(b, Arc::new(Cached::Missing));
        assert!(Arc::ptr_eq(&cache.get(a).unwrap(), &shared));
        cache.insert(c, Arc::new(Cached::Missing));
        assert!(cache.get(b).is_none());
        assert!(cache.get(a).is_some());
        assert_eq!(cache.stats().used_bytes, 8192);
        assert_eq!(cache.stats().evictions, 1);
        cache.set_budget(4095);
        assert_eq!(cache.stats().used_bytes, 0);
        cache.insert(a, Arc::new(Cached::Missing));
        assert_eq!(cache.stats().entries, 0);
    }
    #[test]
    fn version_and_hash_changes_remove_only_that_vessels_shard() {
        let mut cache = FieldCache::new(16384);
        for vessel in [0, 1] {
            cache.insert(
                CacheKey::new(&entry(vessel, 1), Some(9)),
                Arc::new(Cached::Missing),
            );
        }
        cache.retain_version(&entry(0, 2));
        assert_eq!(cache.stats().entries, 1);
        assert!(cache.get(CacheKey::new(&entry(1, 1), Some(9))).is_some());
        let mut altered = entry(1, 1);
        altered.hash = [99; 32];
        cache.retain_version(&altered);
        assert_eq!(cache.stats().entries, 0);
    }
    #[test]
    fn same_root_snapshots_share_cache_and_oversized_entries_are_not_retained() {
        let dir = tempfile::tempdir().unwrap();
        let a = shared(dir.path(), 1024).unwrap();
        let b = shared(dir.path(), 1024).unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        let value = Arc::new(Cached::Universe([1, 2, 3].into_iter().collect()));
        lock(&a)
            .unwrap()
            .insert(CacheKey::new(&entry(0, 1), None), value);
        assert_eq!(lock(&a).unwrap().stats().used_bytes, 0);
        let other = tempfile::tempdir().unwrap();
        assert!(!Arc::ptr_eq(&a, &shared(other.path(), 1024).unwrap()));
        shared(dir.path(), 0).unwrap();
        assert_eq!(lock(&a).unwrap().stats().budget_bytes, 0);
    }
}
