//! Immutable section-entity replacement batches, linked from the sealed base.
//! Each publication writes one node and a fixed-size pointer, never a prefix.
use super::generation::{self, Manifest};
use crate::bm25::Section;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

const MAX_NODE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_BATCH_RECORDS: usize = 4096;
const MAX_NODES: u64 = 1_000_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Head {
    pub node: String,
    pub sha256: String,
    pub sequence: u64,
}
impl Head {
    pub fn validate(&self) -> Result<(), String> {
        if !generation::valid_generation(&self.node)
            || !valid_hash(&self.sha256)
            || self.sequence == 0
            || self.sequence > MAX_NODES
        {
            return Err("Invalid entity overlay head".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Replacement {
    pub section: u32,
    pub source_hash: String,
    pub entities: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Node {
    version: u32,
    base_generation: String,
    sequence: u64,
    previous: Option<Head>,
    replacements: Vec<Replacement>,
}

fn validate_directory(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("Invalid entity overlay directory".into());
    }
    Ok(())
}

fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Hash the source identity/text, excluding entities which overlays replace.
pub fn source_hash(section: &Section) -> String {
    let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
    for value in [&section.title, &section.body] {
        hash.update(&(value.len() as u64).to_le_bytes());
        hash.update(value.as_bytes());
    }
    match &section.filename {
        Some(name) => {
            hash.update(&[1]);
            hash.update(&(name.len() as u64).to_le_bytes());
            hash.update(name.as_bytes());
        }
        None => hash.update(&[0]),
    }
    hash.update(&(section.line_number as u64).to_le_bytes());
    hash.finish()
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn validate_records(records: &[Replacement], sections: u32) -> Result<(), String> {
    if records.is_empty() || records.len() > MAX_BATCH_RECORDS {
        return Err("Entity overlay batch is empty or too large".into());
    }
    let mut seen = HashSet::new();
    for record in records {
        if record.section >= sections
            || !valid_hash(&record.source_hash)
            || !seen.insert(record.section)
        {
            return Err("Invalid or duplicate entity overlay section".into());
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishStep {
    NodeSynced,
    DirectorySynced,
    PointerPublished,
}

/// Fault injection occurs before the pointer changes; an orphan is never replayed.
pub fn publish(
    root: &Path,
    records: Vec<Replacement>,
    mut checkpoint: impl FnMut(PublishStep) -> Result<(), String>,
) -> Result<(Manifest, u64), String> {
    let mut manifest = generation::read_manifest(root)?;
    validate_records(&records, manifest.sections)?;
    let sequence = manifest
        .entity_overlay
        .as_ref()
        .map_or(1, |head| head.sequence + 1);
    if sequence > MAX_NODES {
        return Err("Entity overlay chain limit reached; compact the index".into());
    }
    let node = Node {
        version: 1,
        base_generation: manifest.generation.clone(),
        sequence,
        previous: manifest.entity_overlay.clone(),
        replacements: records,
    };
    let bytes = serde_json::to_vec(&node).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_NODE_BYTES {
        return Err("Entity overlay node exceeds 64 MiB".into());
    }
    let directory = generation::generation_directory(root, &manifest)?.join("overlays");
    fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    validate_directory(&directory)?;
    let id = crate::uuid_v4();
    let path = directory.join(format!("{id}.json"));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    file.write_all(&bytes).map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    drop(file);
    checkpoint(PublishStep::NodeSynced)?;
    generation::sync_directory(&directory)?;
    generation::sync_directory(directory.parent().unwrap())?;
    checkpoint(PublishStep::DirectorySynced)?;
    manifest.entity_overlay = Some(Head {
        node: id,
        sha256: generation::sha256(&bytes),
        sequence,
    });
    manifest.validate()?;
    let pointer_bytes = serde_json::to_vec(&manifest)
        .map_err(|e| e.to_string())?
        .len() as u64;
    crate::search::save_json(&root.join(generation::POINTER), &manifest)?;
    checkpoint(PublishStep::PointerPublished)?;
    Ok((manifest, bytes.len() as u64 + pointer_bytes))
}

/// Validate the complete predecessor chain before returning any replacements.
pub fn read(root: &Path, manifest: &Manifest) -> Result<Vec<Replacement>, String> {
    let directory = generation::generation_directory(root, manifest)?.join("overlays");
    if manifest.entity_overlay.is_some() {
        validate_directory(&directory)?;
    }
    let mut head = manifest.entity_overlay.clone();
    let mut batches = Vec::new();
    while let Some(current) = head {
        current.validate()?;
        let path = directory.join(format!("{}.json", current.node));
        let metadata = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() > MAX_NODE_BYTES
        {
            return Err("Invalid entity overlay node type or size".into());
        }
        let mut bytes = Vec::new();
        File::open(path)
            .map_err(|e| e.to_string())?
            .take(MAX_NODE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > MAX_NODE_BYTES || generation::sha256(&bytes) != current.sha256 {
            return Err("Entity overlay node seal mismatch".into());
        }
        let node: Node = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if node.version != 1
            || node.base_generation != manifest.generation
            || node.sequence != current.sequence
            || node.previous.as_ref().map_or(0, |parent| parent.sequence) != current.sequence - 1
        {
            return Err("Entity overlay chain identity or sequence mismatch".into());
        }
        validate_records(&node.replacements, manifest.sections)?;
        head = node.previous;
        batches.push(node.replacements);
    }
    Ok(batches.into_iter().rev().flatten().collect())
}

/// Check all hashes before mutating. Repeated section results replace, never add.
pub fn apply(sections: &mut [Section], records: &[Replacement]) -> Result<(), String> {
    for record in records {
        let section = sections
            .get(record.section as usize)
            .ok_or("Overlay section exceeds base")?;
        if source_hash(section) != record.source_hash {
            return Err("Entity overlay source hash changed".into());
        }
    }
    for record in records {
        sections[record.section as usize].entities = if record.entities.is_empty() {
            vec!["__LUME_PROCESSED__".into()]
        } else {
            record.entities.clone()
        };
    }
    Ok(())
}
