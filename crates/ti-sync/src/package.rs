//! Shard packaging and canonical verification.
//!
//! Packages sealed `{vessel}/{shard}/{version}` files and catalog snapshot into a
//! tar archive, validating canonical BLAKE3 content digest (§81) and catalog hash (§82).

use std::fs;
use std::path::Path;
use ti_contracts::{
    canonical_shard_input, Catalog, Error, FieldKind, FieldSpec, Result, ShardManifestEntry,
    TransferIdentity, VesselOrd,
};

use crate::tar::{create_tar, parse_tar};

/// Snapshot of catalog metadata and dictionaries necessary to decode fields in a shard.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CatalogSnapshot {
    pub vessel_urn: String,
    pub fields: Vec<FieldSpec>,
    pub dictionary: Vec<DictionaryEntry>,
}

/// A categorical dictionary entry mapping `(field, row)` to string value.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DictionaryEntry {
    pub field: u32,
    pub row: u32,
    pub value: String,
}

/// Compute the canonical BLAKE3 catalog snapshot hash per spec 14 §82.
pub fn compute_catalog_hash(snapshot: &CatalogSnapshot) -> [u8; 32] {
    let mut sorted = snapshot.clone();
    sorted.fields.sort_by_key(|f| f.id);
    sorted
        .dictionary
        .sort_by_key(|d| (d.field, d.row, d.value.clone()));

    let mut hasher = blake3::Hasher::new();
    hasher.update(b"LumeTI/catalog/v1\0");
    let json_bytes = serde_json::to_vec(&sorted).unwrap_or_default();
    hasher.update(&json_bytes);
    *hasher.finalize().as_bytes()
}

/// A packaged sealed shard ready for chunked upload.
#[derive(Debug, Clone)]
pub struct ShardPackage {
    pub transfer: TransferIdentity,
    pub manifest_entry: ShardManifestEntry,
    pub tar_bytes: Vec<u8>,
    pub file_count: usize,
    pub total_file_bytes: u64,
}

/// An unpacked, cryptographically verified shard ready to install into shore storage.
#[derive(Debug, Clone)]
pub struct UnpackedShard {
    pub transfer: TransferIdentity,
    pub manifest_entry: ShardManifestEntry,
    pub catalog_snapshot: CatalogSnapshot,
    pub files: Vec<(u32, Vec<u8>)>,
}

/// Package a sealed shard version on disk into a verified tar archive.
pub fn package_sealed_shard(
    store_root: &Path,
    vessel: VesselOrd,
    shard: u32,
    version: u64,
    catalog: &dyn Catalog,
    width_seconds: u64,
    entry: &ShardManifestEntry,
) -> Result<ShardPackage> {
    let urn = catalog.vessel_urn(vessel)?;

    let shard_dir = store_root
        .join("shards")
        .join(vessel.to_string())
        .join(shard.to_string())
        .join(format!("v{version}"));

    if !shard_dir.is_dir() {
        return Err(Error::NotFound(format!(
            "sealed shard directory not found: {}",
            shard_dir.display()
        )));
    }

    let mut file_pairs = Vec::new();
    let mut total_file_bytes = 0u64;

    for dir_entry in fs::read_dir(&shard_dir)? {
        let dir_entry = dir_entry?;
        let path = dir_entry.path();
        if path.is_file() && path.extension().is_some_and(|e| e == "rbm") {
            let file_stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .ok_or_else(|| Error::Corrupt("invalid rbm filename".into()))?;
            let field_id: u32 = file_stem
                .parse()
                .map_err(|e| Error::Corrupt(format!("invalid field id in filename: {e}")))?;

            let bytes = fs::read(&path)?;
            total_file_bytes += bytes.len() as u64;
            file_pairs.push((field_id, bytes));
        }
    }

    file_pairs.sort_by_key(|(id, _)| *id);

    // Compute canonical BLAKE3 content digest
    let canonical = canonical_shard_input(&file_pairs)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(&canonical);
    let computed_hash = *hasher.finalize().as_bytes();

    if computed_hash != entry.hash {
        return Err(Error::Corrupt(format!(
            "canonical content digest mismatch for shard {shard} v{version}: expected {}, got {}",
            hex_digest(entry.hash),
            hex_digest(computed_hash)
        )));
    }

    // Build catalog snapshot for fields in this shard
    let mut fields = Vec::new();
    let mut dictionary = Vec::new();
    for (field_id, _) in &file_pairs {
        if let Ok(spec) = catalog.field(*field_id) {
            if spec.kind == FieldKind::Set {
                // Collect any dictionary values
                for row_id in 0..1000 {
                    if let Ok(val) = catalog.set_value(*field_id, row_id) {
                        dictionary.push(DictionaryEntry {
                            field: *field_id,
                            row: row_id,
                            value: val,
                        });
                    } else {
                        break;
                    }
                }
            }
            fields.push(spec);
        }
    }

    let catalog_snapshot = CatalogSnapshot {
        vessel_urn: urn.clone(),
        fields,
        dictionary,
    };
    let catalog_hash = compute_catalog_hash(&catalog_snapshot);

    let transfer = TransferIdentity {
        vessel_urn: urn,
        shard,
        version,
        width_seconds,
        from: entry.from,
        to: entry.to,
        hash: entry.hash,
        catalog_hash,
    };
    transfer.validate()?;

    // Build TAR archive
    let transfer_json = serde_json::to_vec_pretty(&transfer)
        .map_err(|e| Error::Corrupt(format!("transfer json: {e}")))?;
    let manifest_json = serde_json::to_vec_pretty(entry)
        .map_err(|e| Error::Corrupt(format!("manifest json: {e}")))?;
    let catalog_json = serde_json::to_vec_pretty(&catalog_snapshot)
        .map_err(|e| Error::Corrupt(format!("catalog json: {e}")))?;

    let mut tar_entries: Vec<(&str, &[u8])> = Vec::new();
    tar_entries.push(("transfer.json", &transfer_json));
    tar_entries.push(("manifest_entry.json", &manifest_json));
    tar_entries.push(("catalog.json", &catalog_json));

    // Store filenames formatted as fields/{id}.rbm
    let file_names: Vec<String> = file_pairs
        .iter()
        .map(|(id, _)| format!("fields/{id}.rbm"))
        .collect();

    for (name, (_, data)) in file_names.iter().zip(&file_pairs) {
        tar_entries.push((name.as_str(), data.as_slice()));
    }

    let tar_bytes = create_tar(&tar_entries)?;

    Ok(ShardPackage {
        transfer,
        manifest_entry: entry.clone(),
        tar_bytes,
        file_count: file_pairs.len(),
        total_file_bytes,
    })
}

/// Unpack a shard tar archive and cryptographically verify all digests (§81, §82).
pub fn unpack_and_verify_shard(tar_bytes: &[u8]) -> Result<UnpackedShard> {
    let entries = parse_tar(tar_bytes)?;

    let mut transfer: Option<TransferIdentity> = None;
    let mut manifest_entry: Option<ShardManifestEntry> = None;
    let mut catalog_snapshot: Option<CatalogSnapshot> = None;
    let mut file_pairs: Vec<(u32, Vec<u8>)> = Vec::new();

    for (name, data) in entries {
        match name.as_str() {
            "transfer.json" => {
                let t: TransferIdentity = serde_json::from_slice(&data)
                    .map_err(|e| Error::Corrupt(format!("invalid transfer.json: {e}")))?;
                transfer = Some(t);
            }
            "manifest_entry.json" => {
                let m: ShardManifestEntry = serde_json::from_slice(&data)
                    .map_err(|e| Error::Corrupt(format!("invalid manifest_entry.json: {e}")))?;
                manifest_entry = Some(m);
            }
            "catalog.json" => {
                let c: CatalogSnapshot = serde_json::from_slice(&data)
                    .map_err(|e| Error::Corrupt(format!("invalid catalog.json: {e}")))?;
                catalog_snapshot = Some(c);
            }
            path if path.starts_with("fields/") && path.ends_with(".rbm") => {
                let file_name = path.strip_prefix("fields/").unwrap();
                let stem = file_name.strip_suffix(".rbm").unwrap();
                let field_id: u32 = stem
                    .parse()
                    .map_err(|e| Error::Corrupt(format!("invalid field id: {e}")))?;
                file_pairs.push((field_id, data));
            }
            _ => {
                // Ignore unknown metadata files
            }
        }
    }

    let transfer = transfer.ok_or_else(|| Error::Corrupt("missing transfer.json in tar".into()))?;
    let manifest_entry = manifest_entry
        .ok_or_else(|| Error::Corrupt("missing manifest_entry.json in tar".into()))?;
    let catalog_snapshot =
        catalog_snapshot.ok_or_else(|| Error::Corrupt("missing catalog.json in tar".into()))?;

    transfer.validate()?;

    // 1. Verify catalog hash (§82)
    let computed_cat_hash = compute_catalog_hash(&catalog_snapshot);
    if computed_cat_hash != transfer.catalog_hash {
        return Err(Error::Corrupt(format!(
            "catalog hash mismatch: expected {}, got {}",
            hex_digest(transfer.catalog_hash),
            hex_digest(computed_cat_hash)
        )));
    }

    // 2. Verify canonical shard content digest (§81)
    file_pairs.sort_by_key(|(id, _)| *id);
    let canonical = canonical_shard_input(&file_pairs)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(&canonical);
    let computed_hash = *hasher.finalize().as_bytes();

    if computed_hash != transfer.hash {
        return Err(Error::Corrupt(format!(
            "canonical shard content hash mismatch: expected {}, got {}",
            hex_digest(transfer.hash),
            hex_digest(computed_hash)
        )));
    }

    Ok(UnpackedShard {
        transfer,
        manifest_entry,
        catalog_snapshot,
        files: file_pairs,
    })
}

fn hex_digest(digest: [u8; 32]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}
