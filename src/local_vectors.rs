//! Opt-in resident section vectors stored in a checked binary f32 format
//! (`local-vectors.bin`), with transparent reading and write-time migration of
//! legacy `local-vectors.json` files. Shared `.jsonl` imports stay JSON input.
use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::bm25::Section;
use crate::hybrid::{SearchResult, section_hash};
use crate::search::load_json;

pub const FILE: &str = "local-vectors.bin";
pub const LEGACY_FILE: &str = "local-vectors.json";
const QUERY_CACHE_LIMIT: usize = 1024;
const MAGIC: &[u8; 8] = b"LUMEVEC1";
const FORMAT_VERSION: u16 = 1;
const ENDIAN_MARKER: u32 = 0x0102_0304;
const HEADER_BYTES: usize = 40;

/// LUME_LOCAL_VECTOR_DEPTH accepts a positive count or all (default: 100).
pub fn candidate_depth() -> Result<usize, String> {
    parse_candidate_depth(std::env::var("LUME_LOCAL_VECTOR_DEPTH").ok().as_deref())
}

fn parse_candidate_depth(value: Option<&str>) -> Result<usize, String> {
    match value {
        None => Ok(100),
        Some("all") => Ok(usize::MAX),
        Some(value) => value
            .parse::<usize>()
            .ok()
            .filter(|depth| *depth > 0)
            .ok_or_else(|| "LUME_LOCAL_VECTOR_DEPTH must be a positive count or all".to_string()),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingProfile {
    pub model: String,
    pub dimensions: usize,
    pub document_task: String,
    pub query_task: String,
}

impl EmbeddingProfile {
    pub fn new(model: String, dimensions: usize) -> Result<Self, String> {
        let profile = Self {
            model,
            dimensions,
            document_task: "document".to_string(),
            query_task: "query".to_string(),
        };
        profile.validate()?;
        Ok(profile)
    }

    fn validate(&self) -> Result<(), String> {
        if self.model.is_empty()
            || self.model.len() > 128
            || !self
                .model
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        {
            return Err("Embedding model must be a nonempty model name, not a URL".to_string());
        }
        if self.dimensions == 0 || self.dimensions > 4096 {
            return Err("Embedding dimensions must be between 1 and 4096".to_string());
        }
        if self.document_task != "document" || self.query_task != "query" {
            return Err("Local vectors require document/query task metadata".to_string());
        }
        Ok(())
    }
}

#[derive(Default)]
pub struct ImportPaths {
    pub documents: Option<PathBuf>,
    pub queries: Option<PathBuf>,
    pub query_texts: Option<PathBuf>,
}

#[derive(Clone)]
struct VectorFile {
    version: u32,
    profile: EmbeddingProfile,
    documents: HashMap<String, Vec<f32>>,
    queries: HashMap<String, Vec<f32>>,
}

#[derive(Serialize, Deserialize)]
struct LegacyVectorFile {
    version: u32,
    profile: EmbeddingProfile,
    documents: HashMap<String, Vec<f64>>,
    queries: HashMap<String, Vec<f64>>,
}

struct DocumentVector {
    source: String,
    section_index: usize,
    values: Vec<f32>,
    norm: f64,
}

pub struct LocalVectors {
    pub profile: EmbeddingProfile,
    documents: Vec<DocumentVector>,
    queries: Mutex<HashMap<String, Vec<f32>>>,
}

fn vector_norm(vector: &[f32], dimensions: usize) -> Result<f64, String> {
    if vector.len() != dimensions {
        return Err(format!(
            "Vector dimensions {} != expected {dimensions}",
            vector.len()
        ));
    }
    if vector.iter().any(|value| !value.is_finite()) {
        return Err("Vector contains a non-finite value".to_string());
    }
    let norm = vector
        .iter()
        .map(|&value| (value as f64) * (value as f64))
        .sum::<f64>()
        .sqrt();
    if !norm.is_finite() || norm == 0.0 {
        return Err("Vector norm must be finite and nonzero".to_string());
    }
    Ok(norm)
}

fn convert_f64_row(row: &[f64], dimensions: usize) -> Result<Vec<f32>, String> {
    if row.len() != dimensions {
        return Err(format!(
            "Vector dimensions {} != expected {dimensions}",
            row.len()
        ));
    }
    if row.iter().any(|value| !value.is_finite()) {
        return Err("Vector contains a non-finite value".to_string());
    }
    let values: Vec<f32> = row.iter().map(|&v| v as f32).collect();
    vector_norm(&values, dimensions)?;
    Ok(values)
}

fn from_legacy_file(legacy: LegacyVectorFile) -> Result<VectorFile, String> {
    if legacy.version != 1 {
        return Err("Unsupported local-vector format version".to_string());
    }
    legacy.profile.validate()?;
    let dims = legacy.profile.dimensions;
    let mut documents = HashMap::with_capacity(legacy.documents.len());
    for (id, row) in legacy.documents {
        documents.insert(id, convert_f64_row(&row, dims)?);
    }
    let mut queries = HashMap::with_capacity(legacy.queries.len());
    for (key, row) in legacy.queries {
        queries.insert(key, convert_f64_row(&row, dims)?);
    }
    Ok(VectorFile {
        version: 1,
        profile: legacy.profile,
        documents,
        queries,
    })
}

fn encode_binary(file: &VectorFile) -> Result<Vec<u8>, String> {
    if file.version != 1 {
        return Err("Unsupported local-vector format version".to_string());
    }
    file.profile.validate()?;
    if file.queries.len() > QUERY_CACHE_LIMIT {
        return Err(format!(
            "Preloaded query vectors exceed {QUERY_CACHE_LIMIT} entries"
        ));
    }
    let dims = file.profile.dimensions;
    let doc_count =
        u32::try_from(file.documents.len()).map_err(|_| "Document vector count exceeds u32")?;
    let query_count =
        u32::try_from(file.queries.len()).map_err(|_| "Query vector count exceeds u32")?;
    let mut doc_ids: Vec<&String> = file.documents.keys().collect();
    doc_ids.sort();
    let mut query_ids: Vec<&String> = file.queries.keys().collect();
    query_ids.sort();

    let mut bytes = Vec::new();
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    bytes.extend_from_slice(&0u16.to_le_bytes());
    bytes.extend_from_slice(&ENDIAN_MARKER.to_le_bytes());
    bytes.extend_from_slice(&0u64.to_le_bytes()); // Filled at finish.
    bytes.extend_from_slice(&(dims as u32).to_le_bytes());
    bytes.extend_from_slice(&doc_count.to_le_bytes());
    bytes.extend_from_slice(&query_count.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());

    for field in [
        file.profile.model.as_str(),
        file.profile.document_task.as_str(),
        file.profile.query_task.as_str(),
    ] {
        let len = u16::try_from(field.len()).map_err(|_| "Profile field exceeds u16")?;
        bytes.extend_from_slice(&len.to_le_bytes());
        bytes.extend_from_slice(field.as_bytes());
    }

    for id in &doc_ids {
        if id.is_empty() {
            return Err("Document vector id must not be empty".to_string());
        }
        let len = u32::try_from(id.len()).map_err(|_| "Document vector id exceeds u32")?;
        bytes.extend_from_slice(&len.to_le_bytes());
        bytes.extend_from_slice(id.as_bytes());
    }
    for id in &doc_ids {
        let row = &file.documents[*id];
        vector_norm(row, dims)?;
        for &value in row {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
    }

    for key in &query_ids {
        if key.is_empty() {
            return Err("Query vector key must not be empty".to_string());
        }
        let len = u32::try_from(key.len()).map_err(|_| "Query vector key exceeds u32")?;
        bytes.extend_from_slice(&len.to_le_bytes());
        bytes.extend_from_slice(key.as_bytes());
    }
    for key in &query_ids {
        let row = &file.queries[*key];
        vector_norm(row, dims)?;
        for &value in row {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
    }

    let total = u64::try_from(bytes.len()).map_err(|_| "Local-vector segment exceeds u64")?;
    bytes[16..24].copy_from_slice(&total.to_le_bytes());
    // Validate with the exact same fail-closed decoder before publishing.
    decode_binary(&bytes)?;
    Ok(bytes)
}

struct BinCursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> BinCursor<'a> {
    fn remaining(&self) -> usize {
        self.bytes.len() - self.position
    }
    fn raw(&mut self, len: usize) -> Result<&'a [u8], String> {
        let end = self
            .position
            .checked_add(len)
            .ok_or("Local-vector offset overflow")?;
        let slice = self
            .bytes
            .get(self.position..end)
            .ok_or("Truncated local-vector payload")?;
        self.position = end;
        Ok(slice)
    }
    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_le_bytes(self.raw(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.raw(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.raw(8)?.try_into().unwrap()))
    }
    fn f32(&mut self) -> Result<f32, String> {
        Ok(f32::from_le_bytes(self.raw(4)?.try_into().unwrap()))
    }
    fn text(&mut self, len: usize) -> Result<&'a str, String> {
        std::str::from_utf8(self.raw(len)?).map_err(|_| "Invalid local-vector UTF-8".to_string())
    }
}

fn decode_binary(bytes: &[u8]) -> Result<VectorFile, String> {
    if bytes.len() < HEADER_BYTES || &bytes[..8] != MAGIC {
        return Err("Invalid or truncated local-vector header".to_string());
    }
    let mut cursor = BinCursor { bytes, position: 8 };
    let version = cursor.u16()?;
    let flags = cursor.u16()?;
    if version != FORMAT_VERSION {
        return Err("Unsupported local-vector format version".to_string());
    }
    if flags != 0 {
        return Err("Nonzero reserved local-vector flags".to_string());
    }
    if cursor.u32()? != ENDIAN_MARKER {
        return Err("Unsupported local-vector byte order".to_string());
    }
    let total_bytes = cursor.u64()?;
    if total_bytes != u64::try_from(bytes.len()).map_err(|_| "Local-vector segment exceeds u64")? {
        return Err("Local-vector length mismatch".to_string());
    }
    let dimensions = cursor.u32()? as usize;
    let doc_count = cursor.u32()? as usize;
    let query_count = cursor.u32()? as usize;
    if cursor.u32()? != 0 {
        return Err("Nonzero reserved local-vector header bits".to_string());
    }
    if query_count > QUERY_CACHE_LIMIT {
        return Err(format!(
            "Preloaded query vectors exceed {QUERY_CACHE_LIMIT} entries"
        ));
    }
    let model_len = cursor.u16()? as usize;
    let model = cursor.text(model_len)?.to_string();
    let doc_task_len = cursor.u16()? as usize;
    let document_task = cursor.text(doc_task_len)?.to_string();
    let query_task_len = cursor.u16()? as usize;
    let query_task = cursor.text(query_task_len)?.to_string();
    let profile = EmbeddingProfile {
        model,
        dimensions,
        document_task,
        query_task,
    };
    profile.validate()?;

    let row_bytes = dimensions
        .checked_mul(4)
        .ok_or("Local-vector row byte overflow")?;
    let min_entry_bytes = row_bytes
        .checked_add(5)
        .ok_or("Local-vector entry byte overflow")?;
    let total_entries = doc_count
        .checked_add(query_count)
        .ok_or("Local-vector count overflow")?;
    let min_required = total_entries
        .checked_mul(min_entry_bytes)
        .ok_or("Local-vector payload size overflow")?;
    if min_required > cursor.remaining() {
        return Err("Local-vector count exceeds available payload".to_string());
    }

    let mut doc_ids = Vec::with_capacity(doc_count);
    for _ in 0..doc_count {
        let id_len = cursor.u32()? as usize;
        if id_len == 0 {
            return Err("Empty local-vector document id".to_string());
        }
        doc_ids.push(cursor.text(id_len)?.to_string());
    }
    let mut documents = HashMap::with_capacity(doc_count);
    for id in doc_ids {
        let mut row = Vec::with_capacity(dimensions);
        for _ in 0..dimensions {
            row.push(cursor.f32()?);
        }
        vector_norm(&row, dimensions)?;
        if documents.insert(id, row).is_some() {
            return Err("Duplicate local-vector document id".to_string());
        }
    }

    let mut query_ids = Vec::with_capacity(query_count);
    for _ in 0..query_count {
        let key_len = cursor.u32()? as usize;
        if key_len == 0 {
            return Err("Empty local-vector query key".to_string());
        }
        query_ids.push(cursor.text(key_len)?.to_string());
    }
    let mut queries = HashMap::with_capacity(query_count);
    for key in query_ids {
        let mut row = Vec::with_capacity(dimensions);
        for _ in 0..dimensions {
            row.push(cursor.f32()?);
        }
        vector_norm(&row, dimensions)?;
        if queries.insert(key, row).is_some() {
            return Err("Duplicate local-vector query key".to_string());
        }
    }

    if cursor.remaining() != 0 {
        return Err("Trailing local-vector bytes".to_string());
    }
    Ok(VectorFile {
        version: 1,
        profile,
        documents,
        queries,
    })
}

fn write_binary_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let temp = path.with_extension(format!(
        "bin.tmp.{}.{}",
        std::process::id(),
        crate::uuid_v4()
    ));
    let write_res = (|| -> Result<(), String> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|e| format!("Failed to create {}: {e}", temp.display()))?;
        file.write_all(bytes)
            .map_err(|e| format!("Failed to write {}: {e}", temp.display()))?;
        file.sync_all()
            .map_err(|e| format!("Failed to sync {}: {e}", temp.display()))?;
        Ok(())
    })();
    if let Err(err) = write_res {
        let _ = std::fs::remove_file(&temp);
        return Err(err);
    }
    if let Err(err) = std::fs::rename(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(format!(
            "Failed to replace {} with {}: {err}",
            path.display(),
            temp.display()
        ));
    }
    Ok(())
}

fn load_vector_file_from_dir(dir: &Path) -> Result<Option<VectorFile>, String> {
    let bin_path = dir.join(FILE);
    if bin_path.exists() {
        let bytes = std::fs::read(&bin_path)
            .map_err(|e| format!("Failed to read {}: {e}", bin_path.display()))?;
        return decode_binary(&bytes).map(Some);
    }
    let legacy_path = dir.join(LEGACY_FILE);
    if legacy_path.exists() {
        let legacy: LegacyVectorFile = load_json(&legacy_path)?;
        return from_legacy_file(legacy).map(Some);
    }
    Ok(None)
}

fn load_vector_file_or_generation(db: &Path) -> Result<Option<VectorFile>, String> {
    if let Some(file) = load_vector_file_from_dir(db)? {
        return Ok(Some(file));
    }
    if crate::index_binary::snapshot::present(db) {
        let manifest = crate::index_binary::generation::read_manifest(db)?;
        let dir = crate::index_binary::generation::generation_directory(db, &manifest)?;
        return load_vector_file_from_dir(&dir);
    }
    Ok(None)
}

pub(crate) fn migrate_legacy_file(db: &Path, sections: &[Section]) -> Result<Vec<u8>, String> {
    let file = load_vector_file_from_dir(db)?.ok_or("Missing local vectors")?;
    let _ = LocalVectors::from_file(file.clone(), sections)?;
    let bytes = encode_binary(&file)?;
    write_binary_atomic(&db.join(FILE), &bytes)?;
    let legacy = db.join(LEGACY_FILE);
    if legacy.exists() {
        let _ = std::fs::remove_file(legacy);
    }
    Ok(bytes)
}

impl LocalVectors {
    pub fn stored_profile(db: &Path) -> Result<Option<EmbeddingProfile>, String> {
        Ok(load_vector_file_or_generation(db)?.map(|file| file.profile))
    }

    pub fn open(db: &Path, sections: &[Section]) -> Result<Option<Self>, String> {
        match load_vector_file_from_dir(db)? {
            Some(file) => Self::from_file(file, sections).map(Some),
            None => Ok(None),
        }
    }

    fn from_file(file: VectorFile, sections: &[Section]) -> Result<Self, String> {
        if file.version != 1 {
            return Err("Unsupported local-vector format version".to_string());
        }
        file.profile.validate()?;
        let mut documents = Vec::with_capacity(sections.len());
        for (section_index, section) in sections.iter().enumerate() {
            let source = section_hash(section);
            let values = file
                .documents
                .get(&source)
                .ok_or_else(|| {
                    format!("Missing local vector for section {section_index}; reindex")
                })?
                .clone();
            let norm = vector_norm(&values, file.profile.dimensions)?;
            documents.push(DocumentVector {
                source,
                section_index,
                values,
                norm,
            });
        }
        if file.queries.len() > QUERY_CACHE_LIMIT {
            return Err(format!(
                "Preloaded query vectors exceed {QUERY_CACHE_LIMIT} entries"
            ));
        }
        for vector in file.queries.values() {
            vector_norm(vector, file.profile.dimensions)?;
        }
        Ok(Self {
            profile: file.profile,
            documents,
            queries: Mutex::new(file.queries),
        })
    }

    /// Build incrementally, or import identical shared vectors with no service calls.
    pub fn build(
        db: &Path,
        sections: &[Section],
        profile: EmbeddingProfile,
        imports: &ImportPaths,
        base: &str,
        token: Option<&str>,
    ) -> Result<(), String> {
        profile.validate()?;
        if imports.queries.is_some() != imports.query_texts.is_some() {
            return Err(
                "--embed-queries requires --embed-query-texts (and vice versa)".to_string(),
            );
        }
        let mut file = VectorFile {
            version: 1,
            profile: profile.clone(),
            documents: HashMap::new(),
            queries: HashMap::new(),
        };
        if let Some(previous) = load_vector_file_or_generation(db)? {
            if previous.version == 1 && previous.profile == profile {
                file = previous;
            }
        }
        if let Some(path) = &imports.documents {
            let mut ids = HashMap::new();
            for section in sections {
                let id = section
                    .filename
                    .as_deref()
                    .and_then(|name| Path::new(name).file_stem())
                    .and_then(|name| name.to_str())
                    .ok_or("Shared vector import requires section filenames")?
                    .to_string();
                if ids.insert(id.clone(), section_hash(section)).is_some() {
                    return Err(format!("Shared doc id {id} maps to multiple sections"));
                }
            }
            let expected: HashSet<String> = ids.keys().cloned().collect();
            let vectors = read_shared_vectors(path, &profile, "docs", &expected)?;
            file.documents = ids
                .into_iter()
                .map(|(id, hash)| (hash, vectors[&id].clone()))
                .collect();
        } else {
            let missing: Vec<&Section> = sections
                .iter()
                .filter(|section| !file.documents.contains_key(&section_hash(section)))
                .collect();
            for batch in missing.chunks(32) {
                let texts: Vec<String> = batch
                    .iter()
                    .map(|section| format!("{}\n{}", section.title, section.body))
                    .collect();
                let vectors = embed(&texts, &profile, &profile.document_task, base, token)?;
                for (section, vector) in batch.iter().zip(vectors) {
                    file.documents.insert(section_hash(section), vector);
                }
            }
        }
        let current: HashSet<String> = sections.iter().map(section_hash).collect();
        file.documents.retain(|hash, _| current.contains(hash));
        if let (Some(path), Some(text_path)) = (&imports.queries, &imports.query_texts) {
            let texts = read_query_texts(text_path)?;
            let expected = texts.keys().cloned().collect();
            let vectors = read_shared_vectors(path, &profile, "queries", &expected)?;
            file.queries.clear();
            for (id, text) in texts {
                let key = text.trim().to_string();
                if let Some(previous) = file.queries.insert(key, vectors[&id].clone()) {
                    if previous != vectors[&id] {
                        return Err("Duplicate query text has different vectors".to_string());
                    }
                }
            }
        }
        // Validate coverage before replacing a previously usable vector file.
        let _ = Self::from_file(file.clone(), sections)?;
        let bytes = encode_binary(&file)?;
        write_binary_atomic(&db.join(FILE), &bytes)?;
        let legacy = db.join(LEGACY_FILE);
        if legacy.exists() {
            let _ = std::fs::remove_file(legacy);
        }
        Ok(())
    }

    pub fn search(
        &self,
        query: &str,
        base: &str,
        token: Option<&str>,
        depth: usize,
    ) -> Result<(Vec<SearchResult>, bool), String> {
        let key = query.trim().to_string();
        let cached = self
            .queries
            .lock()
            .map_err(|_| "Query vector cache poisoned")?
            .get(&key)
            .cloned();
        let is_cached = cached.is_some();
        let vector = match cached {
            Some(vector) => vector,
            None => {
                let vector = embed(
                    std::slice::from_ref(&key),
                    &self.profile,
                    &self.profile.query_task,
                    base,
                    token,
                )?
                .pop()
                .ok_or("Embedding service returned no query vector")?;
                let mut cache = self
                    .queries
                    .lock()
                    .map_err(|_| "Query vector cache poisoned")?;
                // Bounded cache; eviction only affects remote work, never scores.
                if cache.len() >= QUERY_CACHE_LIMIT {
                    cache.clear();
                }
                cache.insert(key, vector.clone());
                vector
            }
        };
        let norm = vector_norm(&vector, self.profile.dimensions)?;
        let mut scores: Vec<(usize, f64)> = self
            .documents
            .iter()
            .enumerate()
            .map(|(row, document)| {
                let dot = document
                    .values
                    .iter()
                    .zip(&vector)
                    .map(|(&a, &b)| (a as f64) * (b as f64))
                    .sum::<f64>();
                (row, dot / (document.norm * norm))
            })
            .collect();
        scores.sort_by(|(a, sa), (b, sb)| {
            sb.total_cmp(sa).then_with(|| {
                self.documents[*a]
                    .section_index
                    .cmp(&self.documents[*b].section_index)
            })
        });
        scores.truncate(depth);
        let results = scores
            .into_iter()
            .map(|(row, score)| SearchResult {
                chunk_id: format!("local-{}", self.documents[row].section_index),
                score,
                text: String::new(),
                source: Some(self.documents[row].source.clone()),
            })
            .collect();
        Ok((results, is_cached))
    }
}

#[derive(Deserialize)]
struct SharedVector {
    id: String,
    vector: Vec<f64>,
}

fn read_shared_vectors(
    path: &Path,
    profile: &EmbeddingProfile,
    kind: &str,
    expected: &HashSet<String>,
) -> Result<HashMap<String, Vec<f32>>, String> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("Invalid vector filename")?;
    let prefix = format!("{}-{}-", profile.model, profile.dimensions);
    let suffix = format!("-{kind}.jsonl");
    let dataset = name
        .strip_prefix(&prefix)
        .and_then(|name| name.strip_suffix(&suffix));
    if dataset.is_none_or(str::is_empty) {
        return Err(format!(
            "Vector filename must match model {}, dimensions {}, kind {kind}",
            profile.model, profile.dimensions
        ));
    }
    let file = File::open(path).map_err(|error| format!("Open shared vectors: {error}"))?;
    let mut vectors = HashMap::new();
    for (line, result) in BufReader::new(file).lines().enumerate() {
        let text = result.map_err(|error| format!("Read shared vectors: {error}"))?;
        if text.trim().is_empty() {
            continue;
        }
        let row: SharedVector = serde_json::from_str(&text)
            .map_err(|_| format!("Malformed shared vector at line {}", line + 1))?;
        if row.id.is_empty() {
            return Err("Shared vector id is empty".to_string());
        }
        let values = convert_f64_row(&row.vector, profile.dimensions)?;
        if vectors.insert(row.id.clone(), values).is_some() {
            return Err(format!("Duplicate shared vector id {}", row.id));
        }
    }
    let missing = expected
        .iter()
        .filter(|id| !vectors.contains_key(*id))
        .count();
    if missing != 0 {
        return Err(format!(
            "Shared vector cache is missing {missing} expected IDs"
        ));
    }
    Ok(vectors)
}

fn read_query_texts(path: &Path) -> Result<HashMap<String, String>, String> {
    let file = File::open(path).map_err(|error| format!("Open query texts: {error}"))?;
    let mut texts = HashMap::new();
    for result in BufReader::new(file).lines() {
        let text = result.map_err(|error| format!("Read query texts: {error}"))?;
        if text.trim().is_empty() {
            continue;
        }
        let (id, query) = text
            .split_once('\t')
            .ok_or("Query texts must be id<TAB>query TSV")?;
        if id.is_empty() || query.trim().is_empty() {
            return Err("Empty query id/text".to_string());
        }
        if texts.insert(id.to_string(), query.to_string()).is_some() {
            return Err(format!("Duplicate query text id {id}"));
        }
    }
    Ok(texts)
}

#[derive(Serialize)]
struct EmbedRequest<'a> {
    texts: &'a [String],
    model: &'a str,
    task: &'a str,
    dimensions: usize,
}

#[derive(Deserialize)]
struct EmbedResponse {
    model: String,
    dim: usize,
    vectors: Vec<Vec<f64>>,
}

fn validate_embed_inputs(texts: &[String]) -> Result<(), String> {
    if texts.is_empty() || texts.len() > 256 {
        return Err("Embedding request requires 1 to 256 texts".to_string());
    }
    if let Some(index) = texts.iter().position(|text| text.len() > 32 * 1024) {
        return Err(format!(
            "Embedding text {index} exceeds 32 KiB; split the section before embedding"
        ));
    }
    Ok(())
}

fn embed(
    texts: &[String],
    profile: &EmbeddingProfile,
    task: &str,
    base: &str,
    token: Option<&str>,
) -> Result<Vec<Vec<f32>>, String> {
    validate_embed_inputs(texts)?;
    let payload = EmbedRequest {
        texts,
        model: &profile.model,
        task,
        dimensions: profile.dimensions,
    };
    let request = ureq::post(&format!("{}/embed", base.trim_end_matches('/')))
        .timeout(Duration::from_secs(60));
    let request = match token {
        Some(token) => request.set("Authorization", &format!("Bearer {token}")),
        None => request,
    };
    // Never include headers, token or response body in a failure.
    let response = request
        .send_json(&payload)
        .map_err(|_| "Embedding request failed")?;
    let response: EmbedResponse = response
        .into_json()
        .map_err(|_| "Malformed embedding response")?;
    if response.model != profile.model
        || response.dim != profile.dimensions
        || response.vectors.len() != texts.len()
    {
        return Err("Embedding response model/dimensions/count mismatch".to_string());
    }
    response
        .vectors
        .into_iter()
        .map(|vector| convert_f64_row(&vector, profile.dimensions))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bm25::Bm25Index;
    use crate::search::{LoadedIndex, SearchMode, SearchOptions, search};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("lume-local-vectors-{}", crate::uuid_v4()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn write(&self, name: &str, text: &str) -> PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, text).unwrap();
            path
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn profile() -> EmbeddingProfile {
        EmbeddingProfile::new("embeddinggemma-2".to_string(), 2).unwrap()
    }
    fn section(id: &str, body: &str) -> Section {
        Section {
            title: id.to_string(),
            body: body.to_string(),
            line_number: 1,
            filename: Some(format!("{id}.txt")),
            entities: vec![],
        }
    }

    #[test]
    fn shared_import_rejects_model_dimensions_duplicates_and_missing_ids() {
        let fixture = Fixture::new();
        let expected = HashSet::from(["a".to_string()]);
        for (name, text, message) in [
            (
                "gtr-t5-base-2-test-docs.jsonl",
                r#"{"id":"a","vector":[1,0]}"#,
                "model",
            ),
            (
                "embeddinggemma-2-3-test-docs.jsonl",
                r#"{"id":"a","vector":[1,0]}"#,
                "dimensions",
            ),
            (
                "embeddinggemma-2-2-test-docs.jsonl",
                r#"{"id":"a","vector":[1]}"#,
                "dimensions",
            ),
            (
                "embeddinggemma-2-2-test-docs.jsonl",
                "{\"id\":\"a\",\"vector\":[1,0]}\n{\"id\":\"a\",\"vector\":[0,1]}",
                "Duplicate",
            ),
            (
                "embeddinggemma-2-2-test-docs.jsonl",
                r#"{"id":"b","vector":[1,0]}"#,
                "missing",
            ),
            (
                "embeddinggemma-2-2-test-docs.jsonl",
                r#"{"id":"a","vector":[1e309,0]}"#,
                "Malformed",
            ),
        ] {
            let path = fixture.write(name, text);
            let error = read_shared_vectors(&path, &profile(), "docs", &expected).unwrap_err();
            assert!(error.contains(message), "{error}");
        }
        assert!(
            vector_norm(&[f32::NAN, 0.0], 2)
                .unwrap_err()
                .contains("non-finite")
        );
        assert!(
            vector_norm(&[f32::INFINITY, 0.0], 2)
                .unwrap_err()
                .contains("non-finite")
        );
        assert!(vector_norm(&[0.0, 0.0], 2).is_err());
    }

    #[test]
    fn exact_cosine_uses_original_values_and_stable_ties() {
        let sections = vec![
            section("a", "captain"),
            section("b", "captain"),
            section("c", "captain"),
        ];
        let file = VectorFile {
            version: 1,
            profile: profile(),
            documents: sections
                .iter()
                .zip([vec![3.0f32, 0.0], vec![1.0, 1.0], vec![-2.0, 0.0]])
                .map(|(section, vector)| (section_hash(section), vector))
                .collect(),
            queries: HashMap::from([("captain".to_string(), vec![2.0f32, 0.0])]),
        };
        let vectors = LocalVectors::from_file(file, &sections).unwrap();
        let (results, cached) = vectors
            .search("captain", "http://127.0.0.1:9", None, 3)
            .unwrap();
        assert!(cached);
        assert_eq!(results[0].score.to_bits(), 1.0f64.to_bits());
        let expected = 2.0 / (2.0f64.sqrt() * 2.0);
        assert_eq!(results[1].score.to_bits(), expected.to_bits());
        assert_eq!(results[2].score.to_bits(), (-1.0f64).to_bits());
        assert_eq!(vectors.documents[0].values, vec![3.0f32, 0.0]);
    }

    #[test]
    fn binary_codec_round_trips_and_fails_closed_on_corruption() {
        let sections = [section("a", "captain"), section("b", "engine")];
        let file = VectorFile {
            version: 1,
            profile: profile(),
            documents: HashMap::from([
                (section_hash(&sections[0]), vec![0.75f32, -0.25]),
                (section_hash(&sections[1]), vec![-0.5f32, 0.5]),
            ]),
            queries: HashMap::from([("captain".to_string(), vec![1.0f32, 0.0])]),
        };
        let bytes = encode_binary(&file).unwrap();
        let decoded = decode_binary(&bytes).unwrap();
        assert_eq!(decoded.profile, file.profile);
        assert_eq!(decoded.documents, file.documents);
        assert_eq!(decoded.queries, file.queries);

        for len in 0..bytes.len() {
            assert!(decode_binary(&bytes[..len]).is_err(), "truncated len={len}");
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(decode_binary(&trailing).is_err());

        for offset in [0, 8, 10, 12, 16, 24, 28, 32, 36] {
            let mut bad = bytes.clone();
            bad[offset] ^= 1;
            assert!(
                decode_binary(&bad).is_err(),
                "corrupt header offset={offset}"
            );
        }
    }

    #[test]
    fn legacy_json_file_reads_and_migrates_to_binary_on_next_write() {
        let fixture = Fixture::new();
        let sections = vec![
            section("a", "captain sailed"),
            section("b", "engine failed"),
        ];
        let legacy = LegacyVectorFile {
            version: 1,
            profile: profile(),
            documents: HashMap::from([
                (section_hash(&sections[0]), vec![1.0, 0.0]),
                (section_hash(&sections[1]), vec![0.0, 1.0]),
            ]),
            queries: HashMap::from([("captain".to_string(), vec![1.0, 0.0])]),
        };
        crate::search::save_json(&fixture.0.join(LEGACY_FILE), &legacy).unwrap();
        assert!(fixture.0.join(LEGACY_FILE).exists());
        assert!(!fixture.0.join(FILE).exists());

        assert_eq!(
            LocalVectors::stored_profile(&fixture.0).unwrap(),
            Some(profile())
        );
        let opened = LocalVectors::open(&fixture.0, &sections).unwrap().unwrap();
        let (hits, cached) = opened
            .search("captain", "http://127.0.0.1:9", None, 2)
            .unwrap();
        assert!(cached);
        assert_eq!(hits[0].chunk_id, "local-0");
        assert_eq!(hits[0].score.to_bits(), 1.0f64.to_bits());

        LocalVectors::build(
            &fixture.0,
            &sections,
            profile(),
            &ImportPaths::default(),
            "http://127.0.0.1:9",
            None,
        )
        .unwrap();
        assert!(fixture.0.join(FILE).exists());
        assert!(!fixture.0.join(LEGACY_FILE).exists());
        let migrated = LocalVectors::open(&fixture.0, &sections).unwrap().unwrap();
        let (hits_after, cached_after) = migrated
            .search("captain", "http://127.0.0.1:9", None, 2)
            .unwrap();
        assert!(cached_after);
        assert_eq!(hits_after[0].chunk_id, "local-0");
        assert_eq!(hits_after[0].score.to_bits(), 1.0f64.to_bits());
    }

    #[test]
    fn imported_documents_and_preloaded_query_search_make_zero_shivvr_calls() {
        let fixture = Fixture::new();
        let sections = vec![
            section("a", "captain sailed"),
            section("b", "engine failed"),
        ];
        let imports = ImportPaths {
            documents: Some(fixture.write(
                "embeddinggemma-2-2-test-docs.jsonl",
                "{\"id\":\"a\",\"vector\":[1,0]}\n{\"id\":\"b\",\"vector\":[0,1]}",
            )),
            queries: Some(fixture.write(
                "embeddinggemma-2-2-test-queries.jsonl",
                r#"{"id":"q","vector":[1,0]}"#,
            )),
            query_texts: Some(fixture.write("queries.tsv", "q\tcaptain\n")),
        };
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        LocalVectors::build(&fixture.0, &sections, profile(), &imports, &base, None).unwrap();
        let mut index = LoadedIndex::from_parts(Bm25Index::build(sections, None));
        index.local_vectors = LocalVectors::open(&fixture.0, &index.bm25.sections).unwrap();
        let options = SearchOptions {
            mode: SearchMode::HybridStrict,
            alpha: 1.0,
            graph_beta: 0.0,
            blend_mode: crate::search::BlendMode::Normalized,
            shivvr_url: Some(base),
            query_inversion: true,
            ..Default::default()
        };
        let result = search(&index, "captain", &options).unwrap();
        assert_eq!(result.executed_mode, SearchMode::HybridStrict);
        assert_eq!(result.hits[0].title, "a");
        assert_eq!(result.hits[0].semantic_score, Some(1.0));
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn local_header_rejects_wrong_tasks_and_missing_section_vectors() {
        let sections = vec![section("a", "captain")];
        let mut file = VectorFile {
            version: 1,
            profile: profile(),
            documents: HashMap::new(),
            queries: HashMap::new(),
        };
        assert!(
            LocalVectors::from_file(file, &sections)
                .err()
                .unwrap()
                .contains("Missing")
        );
        file = VectorFile {
            version: 1,
            profile: profile(),
            documents: HashMap::new(),
            queries: HashMap::new(),
        };
        file.profile.document_task = "query".to_string();
        assert!(
            LocalVectors::from_file(file, &sections)
                .err()
                .unwrap()
                .contains("task")
        );
    }

    #[test]
    fn candidate_depth_accepts_all_and_positive_counts() {
        assert_eq!(parse_candidate_depth(None).unwrap(), 100);
        assert_eq!(parse_candidate_depth(Some("all")).unwrap(), usize::MAX);
        assert_eq!(parse_candidate_depth(Some("500")).unwrap(), 500);
        assert!(parse_candidate_depth(Some("0")).is_err());
        assert!(parse_candidate_depth(Some("invalid")).is_err());
    }

    #[test]
    fn embed_limits_reject_oversize_inputs_before_network_io() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        assert!(validate_embed_inputs(&vec!["x".to_string(); 256]).is_ok());
        assert!(validate_embed_inputs(&["x".repeat(32 * 1024)]).is_ok());
        for texts in [
            vec![],
            vec!["x".to_string(); 257],
            vec!["é".repeat(16 * 1024 + 1)],
        ] {
            assert!(embed(&texts, &profile(), "document", &base, None).is_err());
        }
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn absent_local_vectors_leave_legacy_indexes_unchanged() {
        let fixture = Fixture::new();
        assert!(
            LocalVectors::open(&fixture.0, &[section("a", "captain")])
                .unwrap()
                .is_none()
        );
    }
}
