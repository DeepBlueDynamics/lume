//! Manifest diffing between local boat node and remote shore node.
//!
//! Compares local `manifest.json` against shore `manifest.json`, resolving vessel
//! ordinals to canonical Signal K URNs, and identifying missing or repaired (shard, version, hash).

use std::collections::BTreeMap;
use ti_contracts::{BucketIx, Catalog, Result, ShardKey, ShardManifestEntry, VesselOrd};

/// A missing or out-of-date shard that needs to be synchronized to the shore node.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MissingShard {
    /// Canonical vessel URN.
    pub vessel_urn: String,
    /// Local store shard key (with local vessel ordinal).
    pub local_key: ShardKey,
    /// Repair/shard version.
    pub version: u64,
    /// First bucket index.
    pub from: BucketIx,
    /// Last bucket index.
    pub to: BucketIx,
    /// Total persisted shard file bytes on disk.
    pub bytes: u64,
    /// Expected canonical BLAKE3 content digest.
    pub hash: [u8; 32],
}

/// Compare local manifest entries against shore manifest entries, returning the
/// missing (shard, version, hash) list.
pub fn diff_manifests(
    local_manifest: &[ShardManifestEntry],
    local_catalog: &dyn Catalog,
    shore_manifest: &[ShardManifestEntry],
    shore_catalog: &dyn Catalog,
) -> Result<Vec<MissingShard>> {
    diff_manifests_with_resolver(local_manifest, local_catalog, shore_manifest, |v| {
        shore_catalog.vessel_urn(v)
    })
}

/// Compare local manifest entries against shore manifest entries using a closure
/// to resolve shore vessel ordinals to canonical URNs.
pub fn diff_manifests_with_resolver(
    local_manifest: &[ShardManifestEntry],
    local_catalog: &dyn Catalog,
    shore_manifest: &[ShardManifestEntry],
    shore_resolver: impl Fn(VesselOrd) -> Result<String>,
) -> Result<Vec<MissingShard>> {
    // Index shore shards by (canonical URN, shard number)
    let mut shore_index: BTreeMap<(String, u32), &ShardManifestEntry> = BTreeMap::new();
    for shore_entry in shore_manifest {
        if let Ok(urn) = shore_resolver(shore_entry.key.vessel) {
            shore_index.insert((urn, shore_entry.key.shard), shore_entry);
        }
    }

    let mut missing = Vec::new();

    for local_entry in local_manifest {
        let urn = local_catalog.vessel_urn(local_entry.key.vessel)?;
        let key = (urn.clone(), local_entry.key.shard);

        let needs_sync = match shore_index.get(&key) {
            None => true,
            Some(shore_entry) => {
                // Out of date or repaired content hash
                shore_entry.version < local_entry.version || shore_entry.hash != local_entry.hash
            }
        };

        if needs_sync {
            missing.push(MissingShard {
                vessel_urn: urn,
                local_key: local_entry.key,
                version: local_entry.version,
                from: local_entry.from,
                to: local_entry.to,
                bytes: local_entry.bytes,
                hash: local_entry.hash,
            });
        }
    }

    Ok(missing)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ti_contracts::{VesselOrd, VesselSpec};

    struct TestCatalog(Vec<String>);
    impl Catalog for TestCatalog {
        fn register_vessel(&self, _: &VesselSpec) -> Result<VesselOrd> {
            unimplemented!()
        }
        fn vessel_urn(&self, v: VesselOrd) -> Result<String> {
            self.0
                .get(v as usize)
                .cloned()
                .ok_or_else(|| ti_contracts::Error::NotFound("vessel".into()))
        }
        fn register_field(&self, _: &ti_contracts::FieldSpec) -> Result<u32> {
            unimplemented!()
        }
        fn field(&self, _: u32) -> Result<ti_contracts::FieldSpec> {
            unimplemented!()
        }
        fn fields(&self) -> Result<Vec<ti_contracts::FieldSpec>> {
            unimplemented!()
        }
        fn register_set_value(&self, _: u32, _: &str) -> Result<u32> {
            unimplemented!()
        }
        fn set_value(&self, _: u32, _: u32) -> Result<String> {
            unimplemented!()
        }
        fn set_source_priority(&self, _: &ti_contracts::SourcePriority) -> Result<()> {
            unimplemented!()
        }
        fn source_priority(&self, _: &str) -> Result<Option<ti_contracts::SourcePriority>> {
            unimplemented!()
        }
    }

    #[test]
    fn test_diff_manifests() {
        let local_cat = TestCatalog(vec!["vessels.urn:mrn:imo:mmsi:123456789".into()]);
        let shore_cat = TestCatalog(vec![
            "vessels.urn:other".into(),
            "vessels.urn:mrn:imo:mmsi:123456789".into(),
        ]);

        let hash_a = [1u8; 32];
        let hash_b = [2u8; 32];

        let local_entries = vec![
            ShardManifestEntry {
                key: ShardKey {
                    vessel: 0,
                    shard: 1,
                },
                version: 1,
                from: 1 << 16,
                to: (1 << 16) | 0xff,
                bytes: 100,
                hash: hash_a,
            },
            ShardManifestEntry {
                key: ShardKey {
                    vessel: 0,
                    shard: 2,
                },
                version: 2,
                from: 2 << 16,
                to: (2 << 16) | 0xff,
                bytes: 200,
                hash: hash_b,
            },
        ];

        // Shore has shard 1 with same version/hash (at shore vessel 1), but shard 2 is version 1
        let shore_entries = vec![
            ShardManifestEntry {
                key: ShardKey {
                    vessel: 1,
                    shard: 1,
                },
                version: 1,
                from: 1 << 16,
                to: (1 << 16) | 0xff,
                bytes: 100,
                hash: hash_a,
            },
            ShardManifestEntry {
                key: ShardKey {
                    vessel: 1,
                    shard: 2,
                },
                version: 1,
                from: 2 << 16,
                to: (2 << 16) | 0xff,
                bytes: 150,
                hash: hash_a,
            },
        ];

        let missing =
            diff_manifests(&local_entries, &local_cat, &shore_entries, &shore_cat).unwrap();

        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].local_key.shard, 2);
        assert_eq!(missing[0].version, 2);
        assert_eq!(missing[0].hash, hash_b);
    }
}
