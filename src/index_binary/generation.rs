//! Immutable ordinary-index generation files and one atomic publication pointer.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub const POINTER: &str = "index.json";
pub const FORMAT_VERSION: u32 = 4;
pub const CORE_FILES: &[&str] = &[
    "terms.tbl",
    "term-text.bin",
    "sections.tbl",
    "text.bin",
    "profiles.bin",
    "forward-title.bin",
    "forward-body.bin",
    "postings.bin",
    "bm25-aux.json",
    "build-state.json",
];
const OPTIONAL_FILES: &[&str] = &[
    "spelling.json",
    "spelling.bin",
    "entity_graph.json",
    "meta.json",
    "local-vectors.json",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Seal {
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub format_version: u32,
    pub generation: String,
    pub sections: u32,
    pub source_files: u32,
    pub corpus_fingerprint: [u64; 2],
    #[serde(deserialize_with = "unique_seals")]
    pub segments: BTreeMap<String, Seal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_overlay: Option<super::overlays::Head>,
}

fn unique_seals<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, Seal>, D::Error> {
    struct Unique;
    impl<'de> serde::de::Visitor<'de> for Unique {
        type Value = BTreeMap<String, Seal>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a segment map without duplicate names")
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(
            self,
            mut map: M,
        ) -> Result<Self::Value, M::Error> {
            let mut result = BTreeMap::new();
            while let Some((name, seal)) = map.next_entry::<String, Seal>()? {
                if result.insert(name.clone(), seal).is_some() {
                    return Err(serde::de::Error::custom(format!(
                        "Duplicate index segment {name}"
                    )));
                }
            }
            Ok(result)
        }
    }
    deserializer.deserialize_map(Unique)
}

impl Manifest {
    pub fn validate(&self) -> Result<(), String> {
        if self.format_version != FORMAT_VERSION || !valid_generation(&self.generation) {
            return Err("Unsupported or invalid ordinary-index generation".into());
        }
        if let Some(head) = &self.entity_overlay {
            head.validate()?;
        }
        if CORE_FILES
            .iter()
            .any(|name| !self.segments.contains_key(*name))
        {
            return Err("Ordinary-index generation is missing a core segment".into());
        }
        if self.segments.contains_key("spelling.json") && self.segments.contains_key("spelling.bin")
        {
            return Err("Ambiguous ordinary-index spelling segments".into());
        }
        for (name, seal) in &self.segments {
            if !CORE_FILES.contains(&name.as_str()) && !OPTIONAL_FILES.contains(&name.as_str()) {
                return Err(format!("Unknown ordinary-index segment {name}"));
            }
            if seal.sha256.len() != 64
                || !seal
                    .sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err(format!("Invalid ordinary-index seal for {name}"));
            }
            usize::try_from(seal.bytes).map_err(|_| "Segment length exceeds usize")?;
        }
        Ok(())
    }
}

pub(super) fn valid_generation(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, byte)| {
            if [8, 13, 18, 23].contains(&i) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

pub fn sha256(bytes: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    let mut result = String::with_capacity(64);
    for byte in digest.as_ref() {
        use std::fmt::Write;
        write!(&mut result, "{byte:02x}").unwrap();
    }
    result
}

pub(super) fn sync_directory(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        File::open(path)
            .and_then(|file| file.sync_all())
            .map_err(|e| format!("Cannot sync index directory {}: {e}", path.display()))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishStep {
    CreatedGeneration,
    SyncedSegment,
    SyncedGeneration,
    PublishedPointer,
    RetentionSaved,
}

pub fn publish(
    root: &Path,
    mut manifest: Manifest,
    segments: &BTreeMap<String, Vec<u8>>,
    mut checkpoint: impl FnMut(PublishStep) -> Result<(), String>,
) -> Result<Manifest, String> {
    if !valid_generation(&manifest.generation) {
        return Err("Invalid generation name".into());
    }
    manifest.segments = segments
        .iter()
        .map(|(name, bytes)| {
            (
                name.clone(),
                Seal {
                    bytes: bytes.len() as u64,
                    sha256: sha256(bytes),
                },
            )
        })
        .collect();
    manifest.validate()?;
    let generations = root.join("generations");
    fs::create_dir_all(&generations)
        .map_err(|e| format!("Cannot create index generations: {e}"))?;
    let directory = generations.join(&manifest.generation);
    fs::create_dir(&directory)
        .map_err(|e| format!("Cannot create immutable index generation: {e}"))?;
    checkpoint(PublishStep::CreatedGeneration)?;
    for (name, bytes) in segments {
        let path = directory.join(name);
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .map_err(|e| format!("Cannot create index segment {name}: {e}"))?;
        {
            let _span = crate::index_timing::FileSpan::new("v4.publish.write", &path);
            file.write_all(bytes)
                .map_err(|e| format!("Cannot write index segment {name}: {e}"))?;
        }
        {
            let _span = crate::index_timing::FileSpan::new("v4.publish.fsync", &path);
            file.sync_all()
                .map_err(|e| format!("Cannot sync index segment {name}: {e}"))?;
        }
        checkpoint(PublishStep::SyncedSegment)?;
    }
    sync_directory(&directory)?;
    sync_directory(&generations)?;
    checkpoint(PublishStep::SyncedGeneration)?;
    super::gc::before_publish(root)?;
    checkpoint(PublishStep::RetentionSaved)?;
    crate::search::save_json(&root.join(POINTER), &manifest)?;
    checkpoint(PublishStep::PublishedPointer)?;
    super::gc::after_publish(root);
    Ok(manifest)
}

/// Writes and seals one segment at a time; the pointer stays unchanged until
/// every required segment and the generation directory are durable.
pub struct StagedGeneration {
    root: PathBuf,
    directory: PathBuf,
    manifest: Manifest,
    failed: bool,
}

impl StagedGeneration {
    pub fn new(root: &Path, manifest: Manifest) -> Result<Self, String> {
        if manifest.format_version != FORMAT_VERSION
            || !valid_generation(&manifest.generation)
            || !manifest.segments.is_empty()
        {
            return Err("Invalid staged generation".into());
        }
        let generations = root.join("generations");
        fs::create_dir_all(&generations).map_err(|e| e.to_string())?;
        let directory = generations.join(&manifest.generation);
        fs::create_dir(&directory).map_err(|e| e.to_string())?;
        Ok(Self {
            root: root.to_path_buf(),
            directory,
            manifest,
            failed: false,
        })
    }

    pub fn write(
        &mut self,
        name: &str,
        encode: impl FnOnce(&mut dyn Write) -> Result<(), String>,
    ) -> Result<(), String> {
        if self.failed
            || self.manifest.segments.contains_key(name)
            || (!CORE_FILES.contains(&name) && !OPTIONAL_FILES.contains(&name))
        {
            return Err("Invalid, repeated or failed staged segment".into());
        }
        self.failed = true;
        let path = self.directory.join(name);
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .map_err(|e| e.to_string())?;
        let mut writer = std::io::BufWriter::with_capacity(
            64 * 1024,
            SealedWriter {
                file,
                hash: ring::digest::Context::new(&ring::digest::SHA256),
                bytes: 0,
            },
        );
        {
            let _span = crate::index_timing::FileSpan::new("v4.publish.write", &path);
            encode(&mut writer)?;
            writer.flush().map_err(|e| e.to_string())?;
        }
        let writer = writer.into_inner().map_err(|e| e.to_string())?;
        {
            let _span = crate::index_timing::FileSpan::new("v4.publish.fsync", &path);
            writer.file.sync_all().map_err(|e| e.to_string())?;
        }
        let digest = writer.hash.finish();
        let sha256 = digest
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        self.manifest.segments.insert(
            name.into(),
            Seal {
                bytes: writer.bytes,
                sha256,
            },
        );
        self.failed = false;
        Ok(())
    }

    pub fn finish(self) -> Result<Manifest, String> {
        if self.failed {
            return Err("Cannot publish a failed staged generation".into());
        }
        self.manifest.validate()?;
        sync_directory(&self.directory)?;
        sync_directory(&self.root.join("generations"))?;
        super::gc::before_publish(&self.root)?;
        crate::search::save_json(&self.root.join(POINTER), &self.manifest)?;
        super::gc::after_publish(&self.root);
        Ok(self.manifest)
    }
}

struct SealedWriter {
    file: File,
    hash: ring::digest::Context,
    bytes: u64,
}
impl Write for SealedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let written = self.file.write(bytes)?;
        self.bytes = self
            .bytes
            .checked_add(written as u64)
            .ok_or_else(|| std::io::Error::other("Segment length overflow"))?;
        self.hash.update(&bytes[..written]);
        Ok(written)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

pub fn read_manifest(root: &Path) -> Result<Manifest, String> {
    let path = root.join(POINTER);
    let file = File::open(&path).map_err(|e| format!("Cannot open ordinary-index pointer: {e}"))?;
    // The pointer is small metadata, never a corpus-sized JSON file.
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("Cannot read ordinary-index pointer: {e}"))?;
    if bytes.len() > 1024 * 1024 {
        return Err("Ordinary-index pointer exceeds 1 MiB".into());
    }
    let manifest: Manifest = serde_json::from_slice(&bytes)
        .map_err(|e| format!("Invalid ordinary-index pointer: {e}"))?;
    manifest.validate()?;
    Ok(manifest)
}

pub fn generation_directory(root: &Path, manifest: &Manifest) -> Result<PathBuf, String> {
    manifest.validate()?;
    let root = fs::canonicalize(root).map_err(|e| format!("Cannot resolve index root: {e}"))?;
    let directory = fs::canonicalize(root.join("generations").join(&manifest.generation))
        .map_err(|e| format!("Cannot resolve index generation: {e}"))?;
    if !directory.starts_with(&root) {
        return Err("Index generation escapes root".into());
    }
    Ok(directory)
}

pub fn read_segments(
    root: &Path,
    manifest: &Manifest,
) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let directory = generation_directory(root, manifest)?;
    let mut result = BTreeMap::new();
    for (name, seal) in &manifest.segments {
        let path = directory.join(name);
        let metadata = fs::symlink_metadata(&path)
            .map_err(|e| format!("Cannot inspect index segment {name}: {e}"))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() != seal.bytes
        {
            return Err(format!(
                "Invalid ordinary-index segment length or type: {name}"
            ));
        }
        let capacity = usize::try_from(seal.bytes).map_err(|_| "Index segment exceeds usize")?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(capacity)
            .map_err(|_| format!("Cannot allocate index segment {name}"))?;
        let read_span = crate::index_timing::FileSpan::new("v4.open.read", &path);
        let file =
            File::open(&path).map_err(|e| format!("Cannot open index segment {name}: {e}"))?;
        file.take(
            seal.bytes
                .checked_add(1)
                .ok_or("Index segment length overflow")?,
        )
        .read_to_end(&mut bytes)
        .map_err(|e| format!("Cannot read index segment {name}: {e}"))?;
        drop(read_span);
        let checksum_span = crate::index_timing::FileSpan::new("v4.open.checksum", &path);
        if bytes.len() as u64 != seal.bytes || sha256(&bytes) != seal.sha256 {
            return Err(format!("Ordinary-index seal mismatch: {name}"));
        }
        drop(checksum_span);
        result.insert(name.clone(), bytes);
    }
    Ok(result)
}
