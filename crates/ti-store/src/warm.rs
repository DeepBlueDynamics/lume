//! Non-evicting, newest-first preload into the existing sealed-field cache.
use crate::{
    cache::{lock, CacheKey, Cached},
    row::FieldData,
    DiskCatalog, Manifest, QueryCacheControl, SealedShard,
};
use std::{collections::BTreeSet, path::Path, sync::Arc, time::Instant};
use ti_contracts::{Catalog, Error, Result, RoaringBitmap};

#[derive(Clone, Debug, serde::Serialize)]
pub struct CacheWarmReport {
    pub shards: usize,
    pub fields: usize,
    pub bytes: usize,
    pub budget_bytes: usize,
    pub elapsed_ms: f64,
    pub stopped_at_budget: bool,
}

impl QueryCacheControl {
    /// Benchmark/server helper: the cache budget remains the caller's current budget.
    pub fn warm_configured(&self, root: &Path) -> Result<CacheWarmReport> {
        let path = root.join("ti.toml");
        let query = if path.exists() {
            ti_contracts::TiConfig::from_toml(&std::fs::read_to_string(path)?)?.query
        } else {
            ti_contracts::QueryLimits::default()
        };
        self.warm(root, query.warm_budget_bytes, &query.warm_fields)
    }

    /// Decodes outside the cache lock, one field at a time. Never evicts request entries.
    /// The budget bounds newly retained decoded data, not transient decoder allocations.
    pub fn warm(
        &self,
        root: &Path,
        budget: Option<u64>,
        fields: &[String],
    ) -> Result<CacheWarmReport> {
        let start = Instant::now();
        let capacity = self.stats()?.budget_bytes;
        let budget = budget
            .map(|n| usize::try_from(n).unwrap_or(usize::MAX))
            .unwrap_or(capacity)
            .min(capacity);
        let mut report = CacheWarmReport {
            shards: 0,
            fields: 0,
            bytes: 0,
            budget_bytes: budget,
            elapsed_ms: 0.0,
            stopped_at_budget: budget == 0,
        };
        if budget == 0 {
            report.elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
            return Ok(report);
        }
        let catalog = DiskCatalog::open_or_create(root)?;
        let specs = catalog.fields()?;
        let mut selected = BTreeSet::new();
        for name in fields {
            let mut matched = false;
            for spec in &specs {
                let full = spec.agg.as_ref().map(|agg| {
                    let suffix = format!("{agg:?}").to_lowercase();
                    format!("{}@{suffix}", spec.path)
                });
                if name == &spec.path || full.as_ref() == Some(name) {
                    selected.insert(spec.id);
                    matched = true;
                }
            }
            if !matched {
                return Err(Error::InvalidInput(format!(
                    "warm_fields: unknown field {name:?}"
                )));
            }
        }
        let mut entries = Manifest::open_or_create(root)?.entries();
        entries.sort_by(|a, b| b.to.cmp(&a.to).then_with(|| a.key.cmp(&b.key)));
        'shards: for entry in entries {
            let directory = root
                .join("shards")
                .join(entry.key.vessel.to_string())
                .join(entry.key.shard.to_string())
                .join(format!("v{}", entry.version));
            let mut ids = Vec::new();
            for file in std::fs::read_dir(directory)? {
                let path = file?.path();
                if path.extension().is_some_and(|extension| extension == "rbm") {
                    if let Some(id) = path
                        .file_stem()
                        .and_then(|stem| stem.to_str())
                        .and_then(|stem| stem.parse::<u32>().ok())
                    {
                        ids.push(id);
                    }
                }
            }
            ids.sort_unstable();
            let universe_key = CacheKey::new(&entry, None);
            let mut counted = false;
            if !lock(&self.0)?.contains(universe_key) {
                // Even selective warming needs buckets present only in unselected fields.
                let mut universe = RoaringBitmap::new();
                for &id in &ids {
                    if let Some((_, field)) =
                        SealedShard::load_field(root, entry.key, entry.version, id, &catalog)?
                    {
                        universe |= match &field {
                            FieldData::Presence(row) => row.bitmap(),
                            FieldData::Bsi(row) => row.exists(),
                            FieldData::Count(row) => row.exists(),
                            FieldData::Set(row) => row.presence(),
                            FieldData::Geo(row) => row.presence(),
                        };
                    }
                }
                let value = Arc::new(Cached::Universe(universe));
                let bytes = value.bytes();
                match lock(&self.0)?.warm_insert(universe_key, value, bytes, budget - report.bytes)
                {
                    Some(bytes) => {
                        report.bytes += bytes;
                        counted = bytes > 0;
                        report.shards += usize::from(counted);
                    }
                    None => {
                        report.stopped_at_budget = true;
                        break;
                    }
                }
            }
            for id in ids {
                if !fields.is_empty() && !selected.contains(&id) {
                    continue;
                }
                let key = CacheKey::new(&entry, Some(id));
                if lock(&self.0)?.contains(key) {
                    continue;
                }
                if let Some((_, field)) =
                    SealedShard::load_field(root, entry.key, entry.version, id, &catalog)?
                {
                    let value = Arc::new(Cached::Field(field));
                    let bytes = value.bytes();
                    match lock(&self.0)?.warm_insert(key, value, bytes, budget - report.bytes) {
                        Some(bytes) => {
                            report.bytes += bytes;
                            if bytes > 0 {
                                report.fields += 1;
                                if !counted {
                                    report.shards += 1;
                                    counted = true;
                                }
                            }
                        }
                        None => {
                            report.stopped_at_budget = true;
                            break 'shards;
                        }
                    }
                }
            }
        }
        report.elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
        Ok(report)
    }
}
