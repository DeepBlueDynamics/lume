use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH, Instant, Duration};
use serde::{Deserialize, Serialize};

use crate::bm25::{Bm25Index, Bm25Params, SearchVariant, Section, SearchHit};
use crate::Tagger;

#[derive(Serialize)]
pub struct IngestPayload<'a> {
    pub text: &'a str,
    pub source: &'a str,
}

/// Partial view of the shivvr `/ingest` response — we only need the count of
/// chunks the server created so progress logging can report it.
#[derive(Deserialize)]
struct IngestResponse {
    #[serde(default)]
    chunks_created: usize,
}

#[derive(Deserialize)]
struct EmbedChunk {
    #[serde(default)]
    embedding: Vec<f64>,
}

#[derive(Deserialize)]
struct EmbedResponse {
    #[serde(default)]
    chunks: Vec<EmbedChunk>,
}

/// Embeds `text` to its 768-d GTR-T5 ("organize") vector. shivvr has no
/// dedicated embed endpoint, so we ingest into a throwaway scratch store and
/// read the embedding straight off the response (the store auto-expires). Used
/// by the inversion-steered generator to score candidates against a target.
pub fn embed_text(text: &str, token: &str) -> Result<Vec<f64>, String> {
    let url = format!("{}/temp/lume-embed-scratch/ingest", get_shivvr_base_url());
    let auth_header = format!("Bearer {}", token);
    let payload = IngestPayload { text, source: "embed" };
    match ureq::post(&url)
        .timeout(SHIVVR_TIMEOUT)
        .set("Authorization", &auth_header)
        .send_json(&payload)
    {
        Ok(res) => {
            let resp: EmbedResponse = res
                .into_json()
                .map_err(|e| format!("Failed to parse embed response: {}", e))?;
            let emb = resp
                .chunks
                .into_iter()
                .find(|c| !c.embedding.is_empty())
                .map(|c| c.embedding)
                .ok_or_else(|| "Embed response contained no embedding".to_string())?;
            if emb.len() != 768 {
                return Err(format!("Expected 768-d GTR-T5 vector, got {}", emb.len()));
            }
            Ok(emb)
        }
        Err(e) => Err(format_shivvr_error(&url, format!("Embed request failed: {}", e))),
    }
}

/// Cosine similarity between two equal-length vectors.
pub fn cosine_similarity(a: &[f64], b: &[f64]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0;
    let mut na = 0.0;
    let mut nb = 0.0;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

/// Per-request timeout for shivvr calls. Without this, `ureq` waits forever if
/// the server stalls, which looks identical to a hang.
const SHIVVR_TIMEOUT: Duration = Duration::from_secs(60);

/// Splits a section into `<= 25000`-char ingest payloads, breaking on paragraph
/// (and, for huge paragraphs, line) boundaries. A single part keeps its plain
/// header; multiple parts get a `[Part N]` suffix.
fn split_into_ingest_parts(title: &str, body: &str) -> Vec<String> {
    const MAX: usize = 25000;
    if body.len() <= MAX {
        return vec![format!("Header: {}\nContent: {}", title, body)];
    }
    let mut parts: Vec<String> = Vec::new();
    let mut current = String::new();
    let flush = |parts: &mut Vec<String>, current: &mut String| {
        if !current.is_empty() {
            let n = parts.len() + 1;
            parts.push(format!("Header: {} [Part {}]\nContent: {}", title, n, current));
            current.clear();
        }
    };
    for para in body.split("\n\n") {
        if current.len() + para.len() > MAX && !current.is_empty() {
            flush(&mut parts, &mut current);
        }
        if para.len() > MAX {
            for line in para.split('\n') {
                if current.len() + line.len() > MAX && !current.is_empty() {
                    flush(&mut parts, &mut current);
                }
                if !current.is_empty() {
                    current.push('\n');
                }
                current.push_str(line);
            }
        } else {
            if !current.is_empty() {
                current.push_str("\n\n");
            }
            current.push_str(para);
        }
    }
    flush(&mut parts, &mut current);
    parts
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SearchResult {
    pub chunk_id: String,
    pub score: f64,
    pub text: String,
    pub source: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct SearchResponse {
    pub query: String,
    pub results: Vec<SearchResult>,
    pub time_ms: usize,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct SessionCache {
    pub corpus_path: String,
    pub corpus_mtime: u64,
    pub corpus_size: u64,
    pub session_id: String,
    pub created_at: u64,
    /// Content hashes of every section already ingested into the session.
    /// Lets a corpus change top up only the new/changed sections instead of
    /// re-ingesting everything. Empty on caches written by older builds,
    /// which forces one full re-ingest to establish the baseline.
    #[serde(default)]
    pub ingested_hashes: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SemanticQueryCache {
    pub corpus_path: String,
    pub corpus_mtime: u64,
    pub corpus_size: u64,
    pub queries: HashMap<String, Vec<SearchResult>>,
}

pub const CACHE_FILE: &str = ".lume-session-cache.json";
pub const SEMANTIC_CACHE_FILE: &str = ".lume-semantic-cache.json";

static CACHE_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Anchors the session/semantic cache files to the index db directory so they
/// follow the index instead of landing in whatever cwd the process runs from.
/// Call once per process before any cache access; later calls are no-ops.
pub fn set_cache_dir(dir: &Path) {
    let _ = CACHE_DIR.set(dir.to_path_buf());
}

fn cache_path(name: &str) -> PathBuf {
    match CACHE_DIR.get() {
        Some(dir) => dir.join(name),
        None => PathBuf::from(name),
    }
}

/// Reads a cache file from the db directory, falling back to the legacy
/// cwd-relative location so caches written by older builds still load. The
/// next write lands in the db directory and removes the legacy copy.
fn read_cache_file(name: &str) -> Option<String> {
    if let Ok(content) = fs::read_to_string(cache_path(name)) {
        return Some(content);
    }
    fs::read_to_string(name).ok()
}

fn write_cache_file(name: &str, content: &str) {
    let path = cache_path(name);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(&path, content);
    if path != Path::new(name) {
        let _ = fs::remove_file(name);
    }
}

fn delete_cache_file(name: &str) {
    let _ = fs::remove_file(cache_path(name));
    let _ = fs::remove_file(name);
}

/// Blended hybrid search result hit.
#[derive(Debug, Clone)]
pub struct HybridHit {
    pub section_index: usize,
    pub bm25_score: f64,
    pub semantic_score: f64,
    pub skg_score: f64,
    pub hybrid_score: f64,
    pub boosted: bool,
}

/// Simple percent encoder to avoid adding external dependencies.
pub fn percent_encode(s: &str) -> String {
    let mut encoded = String::new();
    for b in s.bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(b as char);
            }
            b' ' => {
                encoded.push('+');
            }
            _ => {
                encoded.push_str(&format!("%{:02X}", b));
            }
        }
    }
    encoded
}

pub fn get_corpus_metadata(path: &std::path::Path) -> io::Result<(u64, u64)> {
    if path.is_file() {
        let meta = fs::metadata(path)?;
        let mtime = meta.modified()?
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Ok((meta.len(), mtime))
    } else if path.is_dir() {
        let mut total_size = 0;
        let mut max_mtime = 0;
        let mut files = Vec::new();
        collect_files_recursive(path, &mut files)?;
        for f in files {
            if let Ok(meta) = fs::metadata(f) {
                total_size += meta.len();
                let mtime = meta.modified()
                    .map(|t| t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs())
                    .unwrap_or(0);
                if mtime > max_mtime {
                    max_mtime = mtime;
                }
            }
        }
        Ok((total_size, max_mtime))
    } else {
        Err(io::Error::new(io::ErrorKind::NotFound, "Invalid path"))
    }
}

fn collect_files_recursive(dir: &std::path::Path, files: &mut Vec<std::path::PathBuf>) -> io::Result<()> {
    if dir.is_dir() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                if name.starts_with('.') {
                    continue;
                }
            }
            if path.is_dir() {
                collect_files_recursive(&path, files)?;
            } else if path.is_file() {
                if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                    let ext_lower = ext.to_lowercase();
                    // Mirror the indexable set in main.rs::scan_directory so a
                    // code-only corpus still produces a meaningful fingerprint.
                    if matches!(
                        ext_lower.as_str(),
                        "pdf" | "txt" | "md" | "markdown" | "rs" | "py" | "js" | "ts" | "go" | "c" | "cpp" | "h" | "java" | "sh" | "yml" | "yaml" | "toml" | "html" | "css" | "ini" | "cfg" | "conf"
                    ) {
                        files.push(path);
                    }
                }
            }
        }
    }
    Ok(())
}

/// Loads the session cache for `corpus_path` if it exists and hasn't expired,
/// WITHOUT checking the corpus fingerprint. Callers that need an exact match
/// (the old behavior) should compare size/mtime themselves; the incremental
/// ingest path deliberately accepts a stale fingerprint and diffs by hash.
pub fn load_session_cache(corpus_path: &str) -> Option<SessionCache> {
    let content = read_cache_file(CACHE_FILE)?;
    let cache: SessionCache = serde_json::from_str(&content).ok()?;

    if cache.corpus_path != corpus_path {
        return None;
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Ephemeral session expiration limit increased to 7 days (604,800 seconds)
    if now < cache.created_at || now - cache.created_at > 604800 {
        return None;
    }

    Some(cache)
}

pub fn load_cached_session(corpus_path: &str, current_size: u64, current_mtime: u64) -> Option<String> {
    let cache = load_session_cache(corpus_path)?;
    if cache.corpus_size != current_size || cache.corpus_mtime != current_mtime {
        return None;
    }
    Some(cache.session_id)
}

pub fn save_cached_session(corpus_path: &str, size: u64, mtime: u64, session_id: &str, ingested_hashes: Vec<String>) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let cache = SessionCache {
        corpus_path: corpus_path.to_string(),
        corpus_mtime: mtime,
        corpus_size: size,
        session_id: session_id.to_string(),
        created_at: now,
        ingested_hashes,
    };

    if let Ok(content) = serde_json::to_string_pretty(&cache) {
        write_cache_file(CACHE_FILE, &content);
    }
}

pub fn delete_cached_session() {
    delete_cache_file(CACHE_FILE);
}

pub fn load_semantic_cache(corpus_path: &str, current_size: u64, current_mtime: u64) -> SemanticQueryCache {
    if let Some(content) = read_cache_file(SEMANTIC_CACHE_FILE) {
        if let Ok(cache) = serde_json::from_str::<SemanticQueryCache>(&content) {
            if cache.corpus_path == corpus_path && cache.corpus_size == current_size && cache.corpus_mtime == current_mtime {
                return cache;
            }
        }
    }
    SemanticQueryCache {
        corpus_path: corpus_path.to_string(),
        corpus_mtime: current_mtime,
        corpus_size: current_size,
        queries: HashMap::new(),
    }
}

pub fn save_semantic_cache(cache: &SemanticQueryCache) {
    if let Ok(content) = serde_json::to_string_pretty(cache) {
        write_cache_file(SEMANTIC_CACHE_FILE, &content);
    }
}

fn fnv1a64(parts: &[&str]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for part in parts {
        for &b in part.as_bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        // Field separator so ("ab","c") and ("a","bc") hash differently.
        h ^= 0x1f;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Stable content hash identifying a section across re-indexes. Used as the
/// remote chunk `source` so an existing semantic session can be diffed and
/// topped up incrementally instead of re-ingested wholesale. Line numbers are
/// deliberately excluded: moving a section without changing it must not force
/// a re-embed.
pub fn section_hash(sec: &Section) -> String {
    let filename = sec.filename.as_deref().unwrap_or("");
    format!("{:016x}", fnv1a64(&[filename, &sec.title, &sec.body]))
}

struct IngestTask {
    /// Section content hash — becomes the remote chunk's `source`, which is
    /// how search results map back to local sections.
    source: String,
    part_text: String,
}

fn build_ingest_tasks(sections: &[(String, &Section)]) -> Vec<IngestTask> {
    let mut tasks = Vec::new();
    for (hash, sec) in sections {
        for part_text in split_into_ingest_parts(&sec.title, &sec.body) {
            tasks.push(IngestTask {
                source: hash.clone(),
                part_text,
            });
        }
    }
    tasks
}

/// Pushes the prepared tasks into `sess` with a small worker pool. Returns the
/// number of remote chunks created, or the first error encountered.
fn ingest_tasks_concurrent(sess: &str, tasks: Vec<IngestTask>, token: &str) -> Result<usize, String> {
    let base = get_shivvr_base_url();
    let auth_header = format!("Bearer {}", token);
    let start = Instant::now();
    let total_tasks = tasks.len();
    eprintln!("[🌐] Prepared {} sub-chunks for concurrent ingestion...", total_tasks);

    use std::sync::{Arc, Mutex};
    let task_index = Arc::new(Mutex::new(0usize));
    let tasks = Arc::new(tasks);
    let chunks_total = Arc::new(Mutex::new(0usize));
    let error_occurred = Arc::new(Mutex::new(None::<String>));

    let num_threads = 16;

    std::thread::scope(|s| {
        for _ in 0..num_threads {
            let tasks = Arc::clone(&tasks);
            let task_index = Arc::clone(&task_index);
            let chunks_total = Arc::clone(&chunks_total);
            let error_occurred = Arc::clone(&error_occurred);
            let sess = sess.to_string();
            let base = base.clone();
            let auth_header = auth_header.clone();

            s.spawn(move || {
                loop {
                    if error_occurred.lock().unwrap().is_some() {
                        break;
                    }

                    let current_idx = {
                        let mut idx_lock = task_index.lock().unwrap();
                        let idx = *idx_lock;
                        if idx >= total_tasks {
                            break;
                        }
                        *idx_lock += 1;
                        idx
                    };

                    let task = &tasks[current_idx];
                    let url = format!("{}/temp/{}/ingest", base, sess);
                    let payload = IngestPayload { text: &task.part_text, source: &task.source };

                    match ureq::post(&url)
                        .timeout(SHIVVR_TIMEOUT)
                        .set("Authorization", &auth_header)
                        .send_json(&payload)
                    {
                        Ok(res) => {
                            let status = res.status();
                            if status != 200 && status != 201 {
                                let mut err_lock = error_occurred.lock().unwrap();
                                if err_lock.is_none() {
                                    *err_lock = Some(format!("status {}", status));
                                }
                                break;
                            }
                            let created = res.into_json::<IngestResponse>().map(|r| r.chunks_created.max(1)).unwrap_or(1);

                            let mut total_lock = chunks_total.lock().unwrap();
                            *total_lock += created;

                            if *total_lock % 100 == 0 || current_idx == total_tasks - 1 {
                                let elapsed = start.elapsed().as_secs_f64();
                                let rate = if elapsed > 0.0 { *total_lock as f64 / elapsed } else { 0.0 };
                                eprintln!(
                                    "\x1B[36m  [📤] Progress: {}/{} tasks │ {} total chunks │ {:.0}s ({:.1}/s)\x1B[0m",
                                    current_idx + 1, total_tasks, *total_lock, elapsed, rate
                                );
                            }
                        }
                        Err(e) => {
                            let formatted_err = format_shivvr_error(&url, format!("{}", e));
                            let mut err_lock = error_occurred.lock().unwrap();
                            if err_lock.is_none() {
                                *err_lock = Some(formatted_err);
                            }
                            break;
                        }
                    }
                }
            });
        }
    });

    if let Some(err) = error_occurred.lock().unwrap().clone() {
        return Err(err);
    }
    let total_created = *chunks_total.lock().unwrap();
    Ok(total_created)
}

/// Ingests all sections into a newly initialized shivvr session and caches it.
/// Automatically chunks sections whose bodies are too large to avoid 413 Payload Too Large on the neural store.
pub fn initialize_and_ingest_session(
    target_file: &str,
    sections: &[Section],
    corpus_size: u64,
    corpus_mtime: u64,
    token: &str,
) -> Result<String, String> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let sess = format!("lume-hatcherik-{}", timestamp);
    let total = sections.len();
    let start = Instant::now();

    eprintln!(
        "\x1B[1;36m[🌐] HATCHERIK semantic ingest → {} | session {} | {} sections\x1B[0m",
        get_shivvr_base_url(), sess, total
    );

    let hashed: Vec<(String, &Section)> = sections.iter().map(|s| (section_hash(s), s)).collect();
    let tasks = build_ingest_tasks(&hashed);

    let chunks = match ingest_tasks_concurrent(&sess, tasks, token) {
        Ok(n) => n,
        Err(err) => {
            cleanup_session(&sess, token).ok();
            delete_cached_session();
            return Err(format!("Semantic store ingestion error: {}", err));
        }
    };

    eprintln!(
        "\x1B[1;32m[✅] Semantic ingest complete: {} chunks from {} sections in {:.1}s → session {}\x1B[0m",
        chunks, total, start.elapsed().as_secs_f64(), sess
    );

    let mut hashes: Vec<String> = hashed.into_iter().map(|(h, _)| h).collect();
    hashes.sort();
    hashes.dedup();
    save_cached_session(target_file, corpus_size, corpus_mtime, &sess, hashes);
    Ok(sess)
}

/// Returns a semantic session covering `sections`, ingesting only what's
/// missing: a no-op when the corpus fingerprint matches the cached session,
/// an incremental top-up of new/changed sections when it doesn't, and a full
/// ingest only when no usable session exists (none cached, expired, or a
/// legacy cache without content hashes). Sections deleted from the corpus
/// leave orphan chunks in the remote store; they're filtered at blend time
/// because their hash no longer resolves to a local section.
pub fn ensure_semantic_session(
    target_file: &str,
    sections: &[Section],
    corpus_size: u64,
    corpus_mtime: u64,
    token: &str,
) -> Result<String, String> {
    if let Some(cache) = load_session_cache(target_file) {
        if cache.corpus_size == corpus_size && cache.corpus_mtime == corpus_mtime {
            return Ok(cache.session_id);
        }
        if !cache.ingested_hashes.is_empty() {
            let ingested: HashSet<&str> = cache.ingested_hashes.iter().map(|s| s.as_str()).collect();
            let missing: Vec<(String, &Section)> = sections
                .iter()
                .map(|s| (section_hash(s), s))
                .filter(|(h, _)| !ingested.contains(h.as_str()))
                .collect();

            eprintln!(
                "\x1B[1;36m[🌐] Incremental semantic ingest: {} of {} sections new/changed → session {}\x1B[0m",
                missing.len(), sections.len(), cache.session_id
            );

            if !missing.is_empty() {
                let tasks = build_ingest_tasks(&missing);
                if let Err(err) = ingest_tasks_concurrent(&cache.session_id, tasks, token) {
                    // The session may be half-updated; drop it so the next
                    // attempt starts clean rather than serving partial state.
                    delete_cached_session();
                    return Err(format!("Incremental semantic ingestion error: {}", err));
                }
            }

            let mut hashes = cache.ingested_hashes;
            hashes.extend(missing.into_iter().map(|(h, _)| h));
            hashes.sort();
            hashes.dedup();
            save_cached_session(target_file, corpus_size, corpus_mtime, &cache.session_id, hashes);
            return Ok(cache.session_id);
        }
    }
    initialize_and_ingest_session(target_file, sections, corpus_size, corpus_mtime, token)
}

pub fn cleanup_session(session_id: &str, token: &str) -> Result<(), String> {
    let url = format!("{}/temp/{}", get_shivvr_base_url(), session_id);
    let auth_header = format!("Bearer {}", token);
    match ureq::delete(&url)
        .set("Authorization", &auth_header)
        .call() {
        Ok(_) => Ok(()),
        Err(e) => Err(format_shivvr_error(&url, format!("Failed to delete session: {}", e))),
    }
}

pub fn query_semantic_search(
    session_id: &str,
    query: &str,
    token: &str,
) -> Result<Vec<SearchResult>, String> {
    let encoded_query = percent_encode(query);
    let url = format!("{}/temp/{}/search?q={}&n=60", get_shivvr_base_url(), session_id, encoded_query);
    let auth_header = format!("Bearer {}", token);

    match ureq::get(&url)
        .timeout(SHIVVR_TIMEOUT)
        .set("Authorization", &auth_header)
        .call() {
        Ok(res) => {
            match res.into_json::<SearchResponse>() {
                Ok(resp) => Ok(resp.results),
                Err(e) => Err(format!("Failed to parse semantic search JSON: {}", e)),
            }
        }
        Err(e) => {
            if let ureq::Error::Status(status, _) = e {
                if status == 404 {
                    return Err("SESSION_EXPIRED".to_string());
                }
            }
            Err(format_shivvr_error(&url, format!("Semantic search service error: {}", e)))
        }
    }
}

/// Blends local lexical hits with remote semantic hits and the local SKG
/// (entity co-occurrence) signal:
/// `hybrid = bm25 * (1 + alpha*semantic + beta*skg)` when lexically matched,
/// otherwise it falls back to the available signals. `skg_scores` is normalized
/// to `[0,1]` (see `graph_search::compute_skg_scores`); `beta = 0` reproduces
/// the original lexical+semantic behavior exactly.
pub fn blend_hybrid_scores(
    bm25_hits: &[SearchHit],
    semantic_results: &[SearchResult],
    skg_scores: &HashMap<usize, f64>,
    hash_to_idx: &HashMap<String, usize>,
    alpha: f64,
    beta: f64,
) -> Vec<HybridHit> {
    let mut semantic_map: HashMap<usize, f64> = HashMap::new();
    for res in semantic_results {
        if let Some(ref src) = res.source {
            // Chunk sources are section content hashes (current sessions) or
            // raw section indices (sessions ingested by older builds). A hash
            // that no longer resolves belongs to a section that was deleted
            // or changed — skip it instead of mis-attributing the score.
            let resolved = hash_to_idx
                .get(src.as_str())
                .copied()
                .or_else(|| src.parse::<usize>().ok());
            if let Some(idx) = resolved {
                let entry = semantic_map.entry(idx).or_insert(res.score);
                if res.score > *entry {
                    *entry = res.score;
                }
            }
        }
    }

    // Per-candidate tuple: (bm25, semantic, skg, boosted).
    let mut candidate_indices: HashMap<usize, (f64, f64, f64, bool)> = HashMap::new();
    for hit in bm25_hits {
        candidate_indices.insert(hit.section_index, (hit.score, 0.0, 0.0, false));
    }
    for (idx, sem_s) in &semantic_map {
        if let Some(entry) = candidate_indices.get_mut(idx) {
            entry.1 = *sem_s;
            entry.3 = true;
        } else {
            candidate_indices.insert(*idx, (0.0, *sem_s, 0.0, true));
        }
    }
    if beta > 0.0 {
        for (idx, skg_s) in skg_scores {
            if let Some(entry) = candidate_indices.get_mut(idx) {
                entry.2 = *skg_s;
            } else if *skg_s >= crate::graph_search::SKG_EXPAND_MIN {
                // Strongly-related section with no lexical/semantic match:
                // admit it as a recall-expansion candidate.
                candidate_indices.insert(*idx, (0.0, 0.0, *skg_s, true));
            }
        }
    }

    // Two blend modes:
    //  - multiplicative (default, classic HATCHERIK): bm25 * (1 + α·sem + β·skg).
    //    The boost scales WITH bm25, so a high-BM25 doc gets a bigger absolute
    //    lift — semantic/SKG can rarely overtake a strong lexical leader.
    //  - normalized (LUME_BLEND_NORM=1): bm25/max + α·sem + β·skg. All three
    //    signals live on a comparable [0,1] scale, so a strong semantic/SKG
    //    match can actually move #1. Useful when the answer is a vocabulary
    //    match rather than a keyword match.
    let normalize = std::env::var("LUME_BLEND_NORM")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let bm25_max = candidate_indices
        .values()
        .map(|v| v.0)
        .fold(0.0_f64, f64::max)
        .max(1e-9);

    let mut hybrid_hits: Vec<HybridHit> = Vec::new();
    for (idx, (bm25_score, sem_score, skg_score, boosted)) in candidate_indices {
        let hybrid_score = if normalize {
            (bm25_score / bm25_max) + alpha * sem_score + beta * skg_score
        } else if bm25_score > 0.0 {
            bm25_score * (1.0 + alpha * sem_score + beta * skg_score)
        } else {
            sem_score + beta * skg_score
        };
        hybrid_hits.push(HybridHit {
            section_index: idx,
            bm25_score,
            semantic_score: sem_score,
            skg_score,
            hybrid_score,
            boosted,
        });
    }

    hybrid_hits.sort_by(|a, b| b.hybrid_score.partial_cmp(&a.hybrid_score).unwrap_or(std::cmp::Ordering::Equal));
    hybrid_hits
}

pub fn format_shivvr_error(url: &str, error_msg: String) -> String {
    if url.contains("localhost") || url.contains("127.0.0.1") {
        format!(
            "{} (Hint: If you are running inside a Docker container, localhost refers to the container itself. Try using host.docker.internal or host.docker.local instead, e.g. --shivvr-url http://host.docker.internal:8085)",
            error_msg
        )
    } else {
        error_msg
    }
}

pub fn get_shivvr_base_url() -> String {
    std::env::var("SHIVVR_BASE_URL")
        .unwrap_or_else(|_| "http://localhost:8085".to_string())
        .trim_end_matches('/')
        .to_string()
}

pub fn load_nuts_token() -> Option<String> {
    if let Ok(tok) = std::env::var("NUTS_SERVICES_TOKEN") {
        let tok = tok.trim().to_string();
        if !tok.is_empty() {
            return Some(tok);
        }
    }
    if let Ok(content) = fs::read_to_string(".env") {
        for line in content.lines() {
            let line = line.trim();
            if line.starts_with("NUTS_SERVICES_TOKEN=") {
                let parts: Vec<&str> = line.splitn(2, '=').collect();
                if parts.len() == 2 {
                    let tok = parts[1].trim().trim_matches('"').trim_matches('\'').trim().to_string();
                    if !tok.is_empty() {
                        return Some(tok);
                    }
                }
            }
        }
    }
    // Automatically use a dummy token for local shivvr endpoints
    let url = get_shivvr_base_url();
    if url.contains("localhost") || url.contains("127.0.0.1") || url.contains("host.docker.internal") || url.contains("host.docker.local") {
        return Some("local".to_string());
    }
    None
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HybridHitDetails {
    pub rank: usize,
    pub section_index: usize,
    pub title: String,
    pub filename: Option<String>,
    pub line_number: usize,
    pub body: String,
    pub bm25_score: f64,
    pub semantic_score: f64,
    #[serde(default)]
    pub skg_score: f64,
    pub hybrid_score: f64,
    pub boosted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LexicalHitDetails {
    pub score: f64,
    pub title: String,
    pub filename: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticHitDetails {
    pub score: f64,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HybridSearchResult {
    pub query: String,
    pub is_cached: bool,
    pub semantic_results_count: usize,
    pub bm25_results_count: usize,
    pub sem_elapsed: Duration,
    pub lex_elapsed: Duration,
    pub blend_elapsed: Duration,
    pub alpha: f64,
    pub variant: SearchVariant,
    pub hits: Vec<HybridHitDetails>,
    pub lexical_top_hits: Vec<LexicalHitDetails>,
    pub semantic_top_hits: Vec<SemanticHitDetails>,
}

/// The core hybrid search primitive. Blends fast local BM25 indexing with concept-aware remote vector embeddings.
pub fn execute_hybrid_search(
    index: &Bm25Index,
    tagger: Option<&Tagger>,
    target_file: &str,
    query: &str,
    skg_scores: &HashMap<usize, f64>,
    beta: f64,
) -> Result<HybridSearchResult, String> {
    let token = match load_nuts_token() {
        Some(tok) => tok,
        None => return Err("NUTS_SERVICES_TOKEN not set for hybrid semantic search.".to_string()),
    };

    let path = std::path::Path::new(target_file);
    let (corpus_size, corpus_mtime) = get_corpus_metadata(path)
        .map_err(|e| format!("Failed to read metadata for {}: {}", target_file, e))?;

    let mut semantic_cache = load_semantic_cache(target_file, corpus_size, corpus_mtime);

    let variant = match env::var("VARIANT").as_deref() {
        Ok("plus") => SearchVariant::Plus,
        Ok("l") => SearchVariant::L,
        _ => SearchVariant::Classic,
    };

    let params = Bm25Params {
        k1: env::var("K1").ok().and_then(|s| s.parse().ok()).unwrap_or(1.2),
        b: env::var("B").ok().and_then(|s| s.parse().ok()).unwrap_or(0.75),
        delta: env::var("DELTA").ok().and_then(|s| s.parse().ok()).unwrap_or(1.0),
        title_weight: env::var("TITLE_WEIGHT").ok().and_then(|s| s.parse().ok()).unwrap_or(2.0),
        body_weight: env::var("BODY_WEIGHT").ok().and_then(|s| s.parse().ok()).unwrap_or(1.0),
    };

    let alpha: f64 = env::var("ALPHA")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2.0);

    let query_key = query.trim().to_lowercase();
    let mut is_cached = false;

    let sem_start = Instant::now();
    // Query inversion is a debug aid (it shows what the embedding "hears"),
    // but it costs an extra embed + invert round-trip per search with no
    // effect on ranking — opt in via LUME_QUERY_INVERSION=1.
    let inversion_enabled = env::var("LUME_QUERY_INVERSION")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    if inversion_enabled {
        if let Ok(query_vec) = embed_text(query, &token) {
            if let Ok(inv) = crate::inversion::invert_vector(&query_vec, Some(48), &token) {
                println!("[🔄] Query inverts to: \"{}\" (self-similarity {:.3})", inv.text.trim(), inv.similarity);
            }
        }
    }

    let mut semantic_results = if let Some(cached_res) = semantic_cache.queries.get(&query_key) {
        is_cached = true;
        cached_res.clone()
    } else {
        let mut attempts = 0;
        let results = loop {
            let session_id = ensure_semantic_session(target_file, &index.sections, corpus_size, corpus_mtime, &token)?;

            match query_semantic_search(&session_id, query, &token) {
                Ok(res) => {
                    semantic_cache.queries.insert(query_key.clone(), res.clone());
                    save_semantic_cache(&semantic_cache);
                    break res;
                }
                Err(e) => {
                    if e == "SESSION_EXPIRED" && attempts == 0 {
                        delete_cached_session();
                        attempts += 1;
                        continue;
                    }
                    return Err(format!("Failed to retrieve semantic vector search: {}", e));
                }
            }
        };
        results
    };

    // Hash-sourced chunks (current sessions) resolve through this map at
    // blend time; orphans from deleted/changed sections simply don't resolve
    // and drop out. The index-bound staleness check below only applies to
    // legacy sessions whose sources are raw section indices.
    let hash_to_idx: HashMap<String, usize> = index
        .sections
        .iter()
        .enumerate()
        .map(|(i, s)| (section_hash(s), i))
        .collect();

    // A cached session can outlive a re-index (the corpus fingerprint only
    // sees the live directory, not the index). If any returned chunk points
    // past the current section count, the session holds an older corpus:
    // drop it, re-ingest, and re-query rather than panic downstream.
    let sections_len = index.sections.len();
    let is_stale = |res: &[SearchResult]| {
        res.iter().any(|r| {
            r.source.as_ref()
                .filter(|s| !hash_to_idx.contains_key(s.as_str()))
                .and_then(|s| s.parse::<usize>().ok())
                .map_or(false, |idx| idx >= sections_len)
        })
    };
    if is_stale(&semantic_results) {
        eprintln!("[⚠️] Semantic session is stale (chunk ids exceed corpus) — re-ingesting...");
        delete_cached_session();
        semantic_cache.queries.clear();
        let session_id = initialize_and_ingest_session(target_file, &index.sections, corpus_size, corpus_mtime, &token)?;
        semantic_results = query_semantic_search(&session_id, query, &token)
            .map_err(|e| format!("Failed to retrieve semantic vector search: {}", e))?;
        semantic_cache.queries.insert(query_key.clone(), semantic_results.clone());
        save_semantic_cache(&semantic_cache);
        is_cached = false;
    }
    let sem_elapsed = sem_start.elapsed();

    let lex_start = Instant::now();
    let bm25_hits = index.search(query, variant, &params, tagger);
    let lex_elapsed = lex_start.elapsed();

    let blend_start = Instant::now();
    let hybrid_hits = blend_hybrid_scores(&bm25_hits, &semantic_results, skg_scores, &hash_to_idx, alpha, beta);
    let blend_elapsed = blend_start.elapsed();

    let mut lexical_top_hits = Vec::new();
    for hit in bm25_hits.iter().take(5) {
        let sec = &index.sections[hit.section_index];
        lexical_top_hits.push(LexicalHitDetails {
            score: hit.score,
            title: sec.title.clone(),
            filename: sec.filename.clone(),
        });
    }

    let mut semantic_top_hits = Vec::new();
    for res in semantic_results.iter().take(5) {
        semantic_top_hits.push(SemanticHitDetails {
            score: res.score,
            text: res.text.clone(),
        });
    }

    let mut hits = Vec::new();
    for (rank, hit) in hybrid_hits.iter().enumerate() {
        if hit.section_index >= index.sections.len() {
            continue; // defense in depth: never index past the corpus
        }
        let sec = &index.sections[hit.section_index];
        let mut body = sec.body.clone();

        // If this is part of a split section, prepend the previous part and/or append the next part to avoid boundary cutoffs
        let is_part = sec.title.contains("(Part ") || sec.title.contains("[Part ");
        let mut skg_score = hit.skg_score;
        if is_part {
            let clean_title = |t: &str| {
                if let Some(pos) = t.find(" (Part ") {
                    t[..pos].trim().to_string()
                } else if let Some(pos) = t.find(" [Part ") {
                    t[..pos].trim().to_string()
                } else {
                    t.to_string()
                }
            };
            let target_title = clean_title(&sec.title);

            // Prepend the chunk before if it belongs to the same split section
            if hit.section_index > 0 {
                let prev_sec = &index.sections[hit.section_index - 1];
                if prev_sec.filename == sec.filename && clean_title(&prev_sec.title) == target_title {
                    let mut new_body = prev_sec.body.clone();
                    new_body.push_str("\n\n");
                    new_body.push_str(&body);
                    body = new_body;
                    let prev_skg = skg_scores.get(&(hit.section_index - 1)).copied().unwrap_or(0.0);
                    if prev_skg > skg_score {
                        skg_score = prev_skg;
                    }
                }
            }

            // Append the chunk after if it belongs to the same split section
            if hit.section_index + 1 < index.sections.len() {
                let next_sec = &index.sections[hit.section_index + 1];
                if next_sec.filename == sec.filename && clean_title(&next_sec.title) == target_title {
                    body.push_str("\n\n");
                    body.push_str(&next_sec.body);
                    let next_skg = skg_scores.get(&(hit.section_index + 1)).copied().unwrap_or(0.0);
                    if next_skg > skg_score {
                        skg_score = next_skg;
                    }
                }
            }
        }

        hits.push(HybridHitDetails {
            rank: rank + 1,
            section_index: hit.section_index,
            title: sec.title.clone(),
            filename: sec.filename.clone(),
            line_number: sec.line_number,
            body,
            bm25_score: hit.bm25_score,
            semantic_score: hit.semantic_score,
            skg_score,
            hybrid_score: hit.hybrid_score,
            boosted: hit.boosted,
        });
    }

    Ok(HybridSearchResult {
        query: query.to_string(),
        is_cached,
        semantic_results_count: semantic_results.len(),
        bm25_results_count: bm25_hits.len(),
        sem_elapsed,
        lex_elapsed,
        blend_elapsed,
        alpha,
        variant,
        hits,
        lexical_top_hits,
        semantic_top_hits,
    })
}

impl HybridSearchResult {
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("## 🚀 HATCHERIK Hybrid Search: \"{}\"\n\n", self.query));
        if self.is_cached {
            out.push_str("💡 *Results loaded instantly from offline semantic cache.*\n\n");
        } else {
            let clean_url = get_shivvr_base_url()
                .trim_start_matches("https://")
                .trim_start_matches("http://")
                .to_string();
            out.push_str(&format!("🌐 *Results fetched via {} neural vectors.*\n\n", clean_url));
        }

        out.push_str(&format!("Found **{}** blended results across the corpus:\n\n", self.hits.len()));

        for hit in &self.hits {
            let title_to_show = if let Some(ref filename) = hit.filename {
                format!("{} ➔ {}", filename, hit.title)
            } else {
                hit.title.clone()
            };

            out.push_str(&format!("### Rank {} | Hybrid Score: **{:.4}**\n", hit.rank, hit.hybrid_score));
            out.push_str(&format!("* **Header:** {} (Line {})\n", title_to_show, hit.line_number));
            
            let boost_indicator = if hit.boosted {
                if hit.bm25_score > 0.0 {
                    format!("✨ *Boosted (+{:.1}% from semantic similarity {:.4})*", (hit.semantic_score * self.alpha * 100.0), hit.semantic_score)
                } else {
                    format!("✨ *Semantic-Only Candidate (Similarity {:.4})*", hit.semantic_score)
                }
            } else {
                "✖ *No Semantic Match (unboosted lexical-only)*".to_string()
            };
            out.push_str(&format!("* **Metrics:** BM25: {:.4} | {}\n", hit.bm25_score, boost_indicator));

            let snippet_body = if hit.body.len() > 300 {
                format!("{} ...", &hit.body[..300].trim())
            } else {
                hit.body.trim().to_string()
            };
            out.push_str(&format!("> {}\n\n", snippet_body));
        }

        out
    }

    pub fn print_cli(&self) {
        println!("\x1B[1;34m========================================================================\x1B[0m");
        println!("\x1B[1;34m🔍  QUERY: \"{}\"\x1B[0m", self.query);
        println!("\x1B[1;34m========================================================================\x1B[0m");
        println!("\x1B[1;32mTIMINGS:\x1B[0m");
        if self.is_cached {
            println!("  Remote Semantic Search (ONNX):  \x1B[1;32m[CACHED OFFLINE]\x1B[0m (returned {} docs)", self.semantic_results_count);
        } else {
            println!("  Remote Semantic Search (ONNX):  \x1B[36m{:.2?}\x1B[0m (returned {} docs)", self.sem_elapsed, self.semantic_results_count);
        }
        println!("  Local Lexical BM25 Search:      \x1B[36m{:.2?}\x1B[0m (returned {} docs)", self.lex_elapsed, self.bm25_results_count);
        println!("  HATCHERIK Semantic Boosting:     \x1B[36m{:.2?}\x1B[0m", self.blend_elapsed);
        println!();

        println!("\x1B[1;4m1. PURE LEXICAL BM25 TOP MATCHES:\x1B[0m");
        if self.lexical_top_hits.is_empty() {
            println!("  (No matches)");
        } else {
            for (r, hit) in self.lexical_top_hits.iter().enumerate() {
                let title_to_show = if let Some(ref filename) = hit.filename {
                    format!("{} ➔ {}", filename, hit.title)
                } else {
                    hit.title.clone()
                };
                println!("  [{}] Score: \x1B[35m{:.4}\x1B[0m | \x1B[1m{}\x1B[0m", r + 1, hit.score, title_to_show);
            }
        }
        println!();

        println!("\x1B[1;4m2. PURE SEMANTIC (ONNX) TOP MATCHES:\x1B[0m");
        if self.semantic_top_hits.is_empty() {
            println!("  (No matches)");
        } else {
            for (r, res) in self.semantic_top_hits.iter().enumerate() {
                println!("  [{}] Sim: \x1B[35m{:.4}\x1B[0m | \x1B[1m{}\x1B[0m", r + 1, res.score, res.text.lines().next().unwrap_or(""));
            }
        }
        println!();

        println!("\x1B[1;4;33m3. HATCHERIK SEMANTIC BOOSTED HYBRID TOP MATCHES:\x1B[0m");
        if self.hits.is_empty() {
            println!("  (No matches)");
        } else {
            for hit in self.hits.iter().take(5) {
                let boost_indicator = if hit.boosted {
                    if hit.bm25_score > 0.0 {
                        format!("\x1B[32m✨ Boosted (+{:.1}% from semantic Sim {:.4})\x1B[0m", (hit.semantic_score * self.alpha * 100.0), hit.semantic_score)
                    } else {
                        format!("\x1B[35m✨ Semantic-Only Candidate (Sim {:.4})\x1B[0m", hit.semantic_score)
                    }
                } else {
                    "\x1B[31m✖ No Semantic Match (unboosted)\x1B[0m".to_string()
                };
                
                let title_to_show = if let Some(ref filename) = hit.filename {
                    format!("{} ➔ {}", filename, hit.title)
                } else {
                    hit.title.clone()
                };
                println!("  [{}] Hybrid Score: \x1B[1;33m{:.4}\x1B[0m (BM25: {:.4}) | \x1B[1m{}\x1B[0m", hit.rank, hit.hybrid_score, hit.bm25_score, title_to_show);
                println!("      └─ {}", boost_indicator);
            }
        }
        println!("\x1B[1;34m========================================================================\x1B[0m\n");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section(filename: &str, title: &str, body: &str) -> Section {
        Section {
            title: title.to_string(),
            body: body.to_string(),
            line_number: 1,
            filename: Some(filename.to_string()),
            entities: Vec::new(),
        }
    }

    #[test]
    fn section_hash_is_stable_and_content_sensitive() {
        let a = section("a.rs", "fn foo", "let x = 1;");
        let same = section("a.rs", "fn foo", "let x = 1;");
        let edited = section("a.rs", "fn foo", "let x = 2;");
        let moved = Section { line_number: 99, ..section("a.rs", "fn foo", "let x = 1;") };

        assert_eq!(section_hash(&a), section_hash(&same));
        assert_ne!(section_hash(&a), section_hash(&edited));
        // Moving a section without editing it must not change its identity,
        // or every line shift would force a re-embed.
        assert_eq!(section_hash(&a), section_hash(&moved));
    }

    #[test]
    fn blend_resolves_hash_sources_and_drops_orphans() {
        let sections = vec![
            section("a.rs", "alpha", "first body"),
            section("b.rs", "beta", "second body"),
        ];
        let hash_to_idx: HashMap<String, usize> = sections
            .iter()
            .enumerate()
            .map(|(i, s)| (section_hash(s), i))
            .collect();

        let semantic = vec![
            SearchResult {
                chunk_id: "c1".into(),
                score: 0.9,
                text: "first body".into(),
                source: Some(section_hash(&sections[0])),
            },
            // Orphan: hash of a section that no longer exists locally.
            SearchResult {
                chunk_id: "c2".into(),
                score: 0.8,
                text: "deleted body".into(),
                source: Some("feedfacefeedface".into()),
            },
            // Legacy numeric source still resolves.
            SearchResult {
                chunk_id: "c3".into(),
                score: 0.7,
                text: "second body".into(),
                source: Some("1".into()),
            },
        ];

        let hits = blend_hybrid_scores(&[], &semantic, &HashMap::new(), &hash_to_idx, 1.0, 0.0);
        let mut indices: Vec<usize> = hits.iter().map(|h| h.section_index).collect();
        indices.sort();
        assert_eq!(indices, vec![0, 1], "hash and numeric sources resolve; orphan is dropped");
    }
}
