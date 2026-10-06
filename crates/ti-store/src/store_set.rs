//! Multi-store collection for D30 high-resolution and default stores.

use std::collections::BTreeMap;
use std::path::Path;
use ti_contracts::{Result, TiConfig};

use crate::store::Store;

pub struct StoreSet {
    stores: BTreeMap<String, Store>,
}

impl StoreSet {
    /// Open or create all stores defined in `config.resolved_stores()`.
    pub fn open_or_create(config: &TiConfig) -> Result<Self> {
        let mut stores = BTreeMap::new();
        for (name, store_cfg) in config.resolved_stores() {
            let root = store_cfg.resolved_root(&config.store_root, &name);
            let width = store_cfg.width_seconds()?;
            let store = Store::open_or_create(Path::new(&root), width)?;
            stores.insert(name, store);
        }
        Ok(Self { stores })
    }

    pub fn store(&self, name: &str) -> Option<&Store> {
        self.stores.get(name)
    }

    pub fn store_mut(&mut self, name: &str) -> Option<&mut Store> {
        self.stores.get_mut(name)
    }

    pub fn stores(&self) -> &BTreeMap<String, Store> {
        &self.stores
    }

    pub fn stores_mut(&mut self) -> &mut BTreeMap<String, Store> {
        &mut self.stores
    }

    /// Enforce retention across all configured stores.
    /// Drops sealed and open shards in each store older than that store's configured retention.
    /// Returns map of store name -> number of dropped shards.
    pub fn enforce_retention(
        &mut self,
        now_timestamp: i64,
        config: &TiConfig,
    ) -> Result<BTreeMap<String, usize>> {
        let mut results = BTreeMap::new();
        for (name, store_cfg) in config.resolved_stores() {
            if let Some(store) = self.stores.get_mut(&name) {
                if let Some(ret_sec) = store_cfg.retention_seconds()? {
                    let count = store.enforce_retention(now_timestamp, ret_sec)?;
                    results.insert(name, count);
                }
            }
        }
        Ok(results)
    }
}
