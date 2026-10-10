use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::bm25::{filter_query_stopwords, Bm25Index, Bm25Params, SearchVariant, Section};
use crate::semantic_mesh::EntityGraph;
use crate::spelling::SpellIndex;
use crate::tokenize;
use crate::Entry;
use crate::Tagger;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SearchMode {
    LexicalOnly,
    HybridOrFallback,
    HybridStrict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlendMode {
    Multiplicative,
    Normalized,
}

#[derive(Debug, Clone)]
pub struct SearchOptions {
    pub limit: usize,
    pub spell_check: bool,
    pub mode: SearchMode,
    pub alpha: f32,
    pub graph_beta: f64,
    pub use_relatedness: bool,
    pub bm25_params: Bm25Params,
    pub bm25_variant: SearchVariant,
    pub blend_mode: BlendMode,
    pub shivvr_url: Option<String>,
    pub auth_token: Option<String>,
    pub query_inversion: bool,
    pub max_snippet_chars: usize,
    pub facets: Vec<crate::meta::FacetRequest>,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            limit: 10,
            spell_check: false,
            mode: SearchMode::HybridOrFallback,
            alpha: 0.5,
            graph_beta: 0.4,
            use_relatedness: true,
            bm25_params: Bm25Params::default(),
            bm25_variant: SearchVariant::Classic,
            blend_mode: BlendMode::Multiplicative,
            shivvr_url: None,
            auth_token: None,
            query_inversion: false,
            max_snippet_chars: 6000,
            facets: Vec::new(),
        }
    }
}

pub const FORMAT_VERSION_UNSTEMMED: u32 = 1;
pub const FORMAT_VERSION_STEMMED: u32 = 2;
pub const FORMAT_VERSION_META: u32 = 3;
pub const CURRENT_FORMAT_VERSION: u32 = 3;

fn default_format_version() -> u32 {
    1
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct IndexState {
    #[serde(default = "default_format_version")]
    pub format_version: u32,
    pub target_dir: String,
    pub db_dir: String,
    pub semantic_enabled: bool,
    pub ollama_entities: bool,
    pub ollama_model: String,
    pub ollama_url: String,
    pub tag_dict_path: Option<String>,
    pub semantic_session_id: Option<String>,
    pub cached_files: HashMap<String, (u64, Vec<Section>)>,
    #[serde(default)]
    pub stemmed: bool,
    #[serde(default)]
    pub keep_hyphens: bool,
}

pub fn check_state_compatibility(state: &IndexState) -> Result<(), String> {
    if state.format_version > CURRENT_FORMAT_VERSION {
        return Err(format!(
            "Index format version {} is newer than supported version {}; rebuild with a newer lume or reindex with 'lume index -f'.",
            state.format_version,
            CURRENT_FORMAT_VERSION
        ));
    }
    if state.keep_hyphens {
        return Err(
            "Index was built with legacy keep_hyphens=true, which is no longer supported; please reindex with 'lume index -f'."
                .to_string(),
        );
    }
    Ok(())
}

pub struct LoadedIndex {
    pub state: Option<IndexState>,
    pub bm25: Bm25Index,
    pub spelling: Option<SpellIndex>,
    pub entity_graph: Option<EntityGraph>,
    pub tagger: Option<Tagger>,
    pub cache_dir: Option<PathBuf>,
    pub meta: Option<crate::meta::MetaIndex>,
    /// Fingerprint of this immutable index snapshot, not the live source tree.
    pub corpus_fingerprint: OnceLock<(u64, u64)>,
}

impl std::fmt::Debug for LoadedIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoadedIndex")
            .field("state", &self.state)
            .field("bm25", &self.bm25)
            .field("spelling", &self.spelling)
            .field("entity_graph", &self.entity_graph)
            .field("tagger", &self.tagger.as_ref().map(|_| "<Tagger>"))
            .field("cache_dir", &self.cache_dir)
            .field("meta", &self.meta)
            .finish()
    }
}

/// Optional configuration checks when loading an index from disk.
#[derive(Debug, Clone, Copy, Default)]
pub struct OpenEnvChecks {
    pub check_stem: Option<bool>,
    pub check_keep_hyphens: Option<bool>,
}

impl OpenEnvChecks {
    pub fn from_env() -> Self {
        Self {
            check_stem: std::env::var("LUME_STEM")
                .ok()
                .map(|v| v != "0" && v.to_lowercase() != "false"),
            check_keep_hyphens: None,
        }
    }
}

impl LoadedIndex {
    pub fn open(db_dir: impl AsRef<Path>) -> Result<Self, String> {
        Self::open_with_checks(db_dir, OpenEnvChecks::from_env())
    }

    pub fn open_with_checks(
        db_dir: impl AsRef<Path>,
        checks: OpenEnvChecks,
    ) -> Result<Self, String> {
        let db_path = db_dir.as_ref();
        let state_path = db_path.join("state.json");
        if !state_path.exists() {
            return Err(format!(
                "Index state file not found at {}. Index the directory first.",
                state_path.display()
            ));
        }
        let state: IndexState = load_json(&state_path)?;
        check_state_compatibility(&state)?;

        if let Some(env_stemmed) = checks.check_stem {
            if env_stemmed != state.stemmed {
                return Err(format!(
                    "Stemming configuration mismatch: index was built with stemmed={}, but LUME_STEM={} was requested",
                    state.stemmed,
                    if env_stemmed { "1" } else { "0" }
                ));
            }
        }

        let bm25_path = db_path.join("bm25.json");
        if !bm25_path.exists() {
            return Err(format!(
                "Index bm25.json not found at {}. Index the directory first.",
                bm25_path.display()
            ));
        }
        let mut bm25: Bm25Index = load_json(&bm25_path)?;
        bm25.stemmed = state.stemmed;
        bm25.keep_hyphens = false;
        let spelling: Option<SpellIndex> = load_json(&db_path.join("spelling.json")).ok();
        let entity_graph: Option<EntityGraph> = load_json(&db_path.join("entity_graph.json")).ok();

        let mut tagger = None;
        if let Some(ref tag_dict) = state.tag_dict_path {
            let p = Path::new(tag_dict);
            if p.exists() {
                tagger = load_tagger_csv(p).ok();
            }
        }

        let meta = if state.format_version >= 3 {
            let meta_path = db_path.join("meta.json");
            if !meta_path.exists() {
                return Err(format!(
                    "Index format version {} requires meta.json at {}, but it was not found; please reindex with 'lume index -f'.",
                    state.format_version,
                    meta_path.display()
                ));
            }
            let meta_idx = crate::meta::MetaIndex::open(&meta_path)?;
            if meta_idx.num_sections != bm25.sections.len() {
                return Err(format!(
                    "Index metadata section count mismatch: meta.json has {} sections but bm25.json has {}; please reindex with 'lume index -f'.",
                    meta_idx.num_sections,
                    bm25.sections.len()
                ));
            }
            Some(meta_idx)
        } else {
            None
        };

        let corpus_fingerprint = OnceLock::from(crate::hybrid::index_fingerprint(&bm25.sections));
        Ok(Self {
            corpus_fingerprint,
            state: Some(state),
            bm25,
            spelling,
            entity_graph,
            tagger,
            cache_dir: Some(db_path.to_path_buf()),
            meta,
        })
    }

    pub fn from_parts(bm25: Bm25Index) -> Self {
        let corpus_fingerprint = OnceLock::from(crate::hybrid::index_fingerprint(&bm25.sections));
        Self {
            corpus_fingerprint,
            state: None,
            bm25,
            spelling: None,
            entity_graph: None,
            tagger: None,
            cache_dir: None,
            meta: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResultHit {
    pub rank: usize,
    pub section_index: usize,
    pub score: f64,
    pub bm25_score: f64,
    pub semantic_score: Option<f64>,
    pub skg_score: Option<f64>,
    pub title: String,
    pub filename: Option<String>,
    pub line_number: usize,
    pub body: String,
    pub snippet: String,
    pub entities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResults {
    pub query: String,
    pub corrected_query: Option<String>,
    pub executed_mode: SearchMode,
    pub alpha: f32,
    pub graph_beta: f64,
    pub hits: Vec<SearchResultHit>,
    pub total_sections: usize,
    #[serde(default)]
    pub found: usize,
    #[serde(default)]
    pub facets: Option<crate::meta::Facets>,
    pub skg_seeds: Vec<String>,
    pub skg_neighbors: Vec<(String, f64)>,
    pub warnings: Vec<String>,
}

pub fn save_json<T: Serialize>(path: &Path, val: &T) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let file_name = path.file_name().ok_or_else(|| {
        format!(
            "Failed to create file {}: path has no file name",
            path.display()
        )
    })?;
    let seq = {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        NEXT.fetch_add(1, Ordering::Relaxed)
    };
    let mut tmp_name = file_name.to_os_string();
    tmp_name.push(format!(".{}.{}.tmp", std::process::id(), seq));
    let tmp_path = parent.join(tmp_name);

    let written = (|| {
        let file = File::create(&tmp_path)
            .map_err(|e| format!("Failed to create file {}: {}", tmp_path.display(), e))?;
        let mut writer = io::BufWriter::new(file);
        serde_json::to_writer_pretty(&mut writer, val)
            .map_err(|e| format!("Failed to write JSON to {}: {}", path.display(), e))?;
        writer
            .flush()
            .map_err(|e| format!("Failed to flush {}: {}", tmp_path.display(), e))?;
        let file = writer
            .into_inner()
            .map_err(|e| format!("Failed to flush {}: {}", tmp_path.display(), e))?;
        file.sync_all()
            .map_err(|e| format!("Failed to sync {}: {}", tmp_path.display(), e))?;
        drop(file);
        std::fs::rename(&tmp_path, path).map_err(|e| {
            format!(
                "Failed to rename {} to {}: {}",
                tmp_path.display(),
                path.display(),
                e
            )
        })?;
        Ok(())
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp_path);
        return written;
    }

    // A same-directory rename is atomic. The directory entry itself is durable
    // only after the parent directory is synced. Windows replaces via rename;
    // it has no directory fsync.
    #[cfg(unix)]
    {
        let dir = File::open(parent)
            .map_err(|e| format!("Failed to open directory {}: {}", parent.display(), e))?;
        dir.sync_all()
            .map_err(|e| format!("Failed to sync directory {}: {}", parent.display(), e))?;
    }
    Ok(())
}

pub fn load_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let file =
        File::open(path).map_err(|e| format!("Failed to open file {}: {}", path.display(), e))?;
    let reader = io::BufReader::new(file);
    let val = serde_json::from_reader(reader)
        .map_err(|e| format!("Failed to parse JSON from {}: {}", path.display(), e))?;
    Ok(val)
}

pub fn load_tagger_csv(path: &Path) -> io::Result<Tagger> {
    let kind = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("entity")
        .to_string();
    let text = std::fs::read_to_string(path)?;
    let mut lines = text.lines();
    let header_line = match lines.next() {
        Some(h) => h,
        None => return Err(io::Error::new(io::ErrorKind::InvalidData, "Empty CSV file")),
    };
    let headers = crate::parse_csv_line(header_line);
    let action_col = headers
        .iter()
        .position(|h| h.trim().eq_ignore_ascii_case("action"));
    let is_regex_col = headers
        .iter()
        .position(|h| h.trim().eq_ignore_ascii_case("is_regex"));

    let mut entries = Vec::new();
    for (i, raw) in lines.enumerate() {
        let cells = crate::parse_csv_line(raw);
        let phrase = cells.first().map(|s| s.trim()).unwrap_or("");
        if phrase.is_empty() {
            continue;
        }
        let output_override = action_col
            .and_then(|idx| cells.get(idx))
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let is_regex_val = is_regex_col
            .and_then(|idx| cells.get(idx))
            .map(|s| s.trim().eq_ignore_ascii_case("true"))
            .unwrap_or(false);

        let mut entry =
            Entry::new(phrase, kind.clone(), format!("csv-{}", i)).with_regex(is_regex_val);
        if let Some(out) = output_override {
            entry = entry.with_output(out);
        }
        entries.push(entry);
    }
    Tagger::build(entries)
}

pub fn correct_query(spelling: &SpellIndex, query: &str) -> String {
    let mut words = Vec::new();
    for word in query.split_whitespace() {
        let clean: String = word
            .chars()
            .filter(|c| c.is_alphanumeric())
            .collect::<String>()
            .to_lowercase();
        if clean.is_empty() {
            words.push(word.to_string());
            continue;
        }
        if spelling.vocab_set.contains(&clean) {
            words.push(word.to_string());
        } else {
            let suggestions = spelling.correct_word(&clean, 1);
            if let Some((best, _)) = suggestions.first() {
                words.push(best.clone());
            } else {
                words.push(word.to_string());
            }
        }
    }
    words.join(" ")
}

pub fn best_snippet(body: &str, query: &str) -> String {
    best_snippet_with_cap(body, query, 6000)
}

pub fn best_snippet_with_cap(body: &str, query: &str, max_chars: usize) -> String {
    if body.chars().count() <= max_chars {
        return body.trim().to_string();
    }

    use std::collections::HashSet;
    let parsed = crate::bm25::parse_query(query);
    let pos_query = if parsed.not_terms.is_empty() {
        query.to_string()
    } else {
        parsed.positive_terms.join(" ")
    };
    let q_tokens = filter_query_stopwords(tokenize(&pos_query));
    let qset: HashSet<Vec<u8>> = q_tokens.into_iter().map(|t| t.bytes).collect();

    let lines: Vec<&str> = body.lines().collect();
    if lines.is_empty() {
        return String::new();
    }

    let mut best_idx = 0usize;
    let mut best_score = 0usize;
    for (i, line) in lines.iter().enumerate() {
        let mut seen: HashSet<&Vec<u8>> = HashSet::new();
        for t in tokenize(line) {
            if let Some(k) = qset.get(&t.bytes) {
                seen.insert(k);
            }
        }
        if seen.len() > best_score {
            best_score = seen.len();
            best_idx = i;
        }
    }

    if best_score == 0 {
        return lines
            .iter()
            .filter(|l| !l.trim().is_empty())
            .take(15)
            .copied()
            .collect::<Vec<_>>()
            .join("\n");
    }

    let start = best_idx.saturating_sub(35);
    let end = (best_idx + 45).min(lines.len());
    let snippet = lines[start..end].join("\n");
    let trimmed = snippet.trim();
    if trimmed.chars().count() > max_chars {
        let capped: String = trimmed.chars().take(max_chars).collect();
        format!("{}…", capped)
    } else {
        trimmed.to_string()
    }
}

pub fn search(
    index: &LoadedIndex,
    query: &str,
    opts: &SearchOptions,
) -> Result<SearchResults, String> {
    let mut warnings = Vec::new();

    if !index.bm25.stemmed {
        warnings.push("Notice: this index was built without stemming; run 'lume index -f' to reindex with default stemming.".to_string());
    }

    // 0. Extract field filters before spell correction and NOT parsing
    let (text_query, filters) = if let Some(ref meta) = index.meta {
        if query.contains(':') {
            crate::meta::extract_filters(query, Some(meta))
        } else {
            (query.to_string(), Vec::new())
        }
    } else {
        (query.to_string(), Vec::new())
    };

    let allow = if filters.is_empty() {
        None
    } else if let Some(ref meta) = index.meta {
        crate::meta::evaluate_filters(meta, &filters)
    } else {
        None
    };

    // Filter-only query: if text_query has no text terms, return filtered sections in id order with score 0
    if text_query.trim().is_empty() && !filters.is_empty() {
        let matching_ids: Vec<u32> = allow.clone().map(|bm| bm.iter()).unwrap_or_default();
        let found = matching_ids.len();
        let facets = if !opts.facets.is_empty() {
            let match_bm = allow.unwrap_or_default();
            Some(crate::meta::compute_facets(
                index.meta.as_ref(),
                &index.bm25,
                &match_bm,
                &matching_ids,
                &opts.facets,
            ))
        } else {
            None
        };
        let limit = opts.limit.min(matching_ids.len());
        let mut hits = Vec::new();
        for (i, &sec_id) in matching_ids[..limit].iter().enumerate() {
            if let Some(sec) = index.bm25.sections.get(sec_id as usize) {
                let snippet = sec.body.lines().take(5).collect::<Vec<_>>().join("\n");
                hits.push(SearchResultHit {
                    rank: i + 1,
                    section_index: sec_id as usize,
                    score: 0.0,
                    bm25_score: 0.0,
                    semantic_score: None,
                    skg_score: None,
                    title: sec.title.clone(),
                    filename: sec.filename.clone(),
                    line_number: sec.line_number,
                    body: sec.body.clone(),
                    snippet,
                    entities: sec
                        .entities
                        .iter()
                        .filter(|&e| e != "__LUME_PROCESSED__")
                        .cloned()
                        .collect(),
                });
            }
        }
        return Ok(SearchResults {
            query: query.to_string(),
            corrected_query: None,
            executed_mode: SearchMode::LexicalOnly,
            alpha: opts.alpha,
            graph_beta: opts.graph_beta,
            hits,
            total_sections: index.bm25.sections.len(),
            found,
            facets,
            skg_seeds: Vec::new(),
            skg_neighbors: Vec::new(),
            warnings,
        });
    }

    // 1. Spell correction
    let (corrected_query_opt, effective_query) = if opts.spell_check {
        if let Some(ref spelling) = index.spelling {
            let corrected = correct_query(spelling, &text_query);
            if corrected != text_query {
                (Some(corrected.clone()), corrected)
            } else {
                (None, text_query.to_string())
            }
        } else {
            (None, text_query.to_string())
        }
    } else {
        (None, text_query.to_string())
    };

    // 1b. Parse NOT terms and validate excluded terms
    let parsed_query = crate::bm25::parse_query(&effective_query);
    for not_term in &parsed_query.not_terms {
        for tok in crate::tokenize_with_options(not_term, index.bm25.stemmed, false) {
            if crate::bm25::is_stopword(&tok.bytes) {
                warnings.push(format!(
                    "Notice: excluded term '{}' is a stopword and was ignored.",
                    String::from_utf8_lossy(&tok.bytes)
                ));
            }
        }
    }
    if !parsed_query.not_terms.is_empty() && parsed_query.positive_terms.is_empty() {
        warnings
            .push("Notice: query contains only excluded terms; returning 0 results.".to_string());
    }

    // 2. SKG graph walk
    let beta = opts.graph_beta;
    let mut skg_seeds = Vec::new();
    let mut skg_neighbors = Vec::new();
    let mut skg_scores = HashMap::new();

    if beta > 0.0 {
        if let Some(ref graph) = index.entity_graph {
            let skg_params = crate::graph_search::SkgBoostParams {
                beta,
                use_relatedness: opts.use_relatedness,
                ..Default::default()
            };
            let skg_query = if parsed_query.not_terms.is_empty() {
                effective_query.clone()
            } else {
                parsed_query.positive_terms.join(" ")
            };
            let walk = crate::graph_search::compute_skg_scores(
                &index.bm25,
                graph,
                &skg_query,
                &skg_params,
            );
            let label = |k: &str| {
                index
                    .bm25
                    .entity_labels
                    .get(k)
                    .cloned()
                    .unwrap_or_else(|| k.to_string())
            };
            skg_seeds = walk.seeds.iter().map(|s| label(s)).collect();
            skg_neighbors = walk.expanded.iter().map(|(k, w)| (label(k), *w)).collect();
            skg_scores = walk.scores;
        } else if let Some(ref db_path) = index.cache_dir {
            let graph_path = db_path.join("entity_graph.json");
            if !graph_path.exists() {
                warnings.push("\x1B[35m[SKG] No entity_graph.json found; graph boost disabled (re-run `lume index`).\x1B[0m".to_string());
            } else {
                warnings.push(
                    "\x1B[35m[SKG] Failed to load entity_graph.json; graph boost disabled.\x1B[0m"
                        .to_string(),
                );
            }
        }
    }

    // 3. Mode decision: Hybrid vs Lexical
    let want_hybrid = match opts.mode {
        SearchMode::LexicalOnly => false,
        SearchMode::HybridOrFallback => opts.alpha > 0.0,
        SearchMode::HybridStrict => {
            if opts.alpha <= 0.0 {
                return Err("HybridStrict requires alpha > 0.0".to_string());
            }
            true
        }
    };

    if want_hybrid {
        let token_opt = opts
            .auth_token
            .clone()
            .or_else(|| {
                crate::hybrid::load_nuts_token_at(&crate::hybrid::resolve_shivvr_base_url(
                    opts.shivvr_url.as_deref(),
                ))
            });
        let has_session = index
            .state
            .as_ref()
            .and_then(|s| s.semantic_session_id.as_ref())
            .is_some();

        if !has_session {
            if opts.mode == SearchMode::HybridStrict {
                return Err("Semantic search unavailable for this index (no semantic session — index with -s).".to_string());
            }
            warnings.push("[⚠️] Semantic search unavailable for this index (no semantic session — index with -s); falling back to lexical BM25.".to_string());
        } else if token_opt.is_none() {
            if opts.mode == SearchMode::HybridStrict {
                return Err("Semantic search unavailable (no NUTS_SERVICES_TOKEN and shivvr endpoint is not local).".to_string());
            }
            warnings.push("[⚠️] Semantic search unavailable (no NUTS_SERVICES_TOKEN and shivvr endpoint is not local); falling back to lexical BM25.".to_string());
        } else {
            // Attempt hybrid search
            let target_dir = index
                .state
                .as_ref()
                .map(|s| s.target_dir.as_str())
                .unwrap_or("");
            let hybrid_res = crate::hybrid::execute_hybrid_search(
                &index.bm25,
                index.tagger.as_ref(),
                target_dir,
                *index.corpus_fingerprint.get_or_init(|| {
                    crate::hybrid::index_fingerprint(&index.bm25.sections)
                }),
                &effective_query,
                &skg_scores,
                beta,
                opts.alpha as f64,
                index.cache_dir.as_deref(),
                &opts.bm25_params,
                opts.bm25_variant,
                opts.blend_mode,
                opts.shivvr_url.as_deref(),
                token_opt.as_deref(),
                opts.query_inversion,
            );

            match hybrid_res {
                Ok(mut h_results) => {
                    if let Some(ref allow_bm) = allow {
                        h_results
                            .hits
                            .retain(|h| allow_bm.contains(h.section_index as u32));
                    }
                    let found = h_results.hits.len();
                    let facets = if !opts.facets.is_empty() {
                        let mut match_ids: Vec<u32> = h_results
                            .hits
                            .iter()
                            .map(|h| h.section_index as u32)
                            .collect();
                        match_ids.sort();
                        let match_bm = crate::fast_retrieval::MiniRoaring::from_sorted(&match_ids);
                        Some(crate::meta::compute_facets(
                            index.meta.as_ref(),
                            &index.bm25,
                            &match_bm,
                            &match_ids,
                            &opts.facets,
                        ))
                    } else {
                        None
                    };
                    h_results.hits.truncate(opts.limit);
                    let mut hits = Vec::new();
                    for h in h_results.hits {
                        let snippet = best_snippet_with_cap(
                            &h.body,
                            &effective_query,
                            opts.max_snippet_chars,
                        );
                        hits.push(SearchResultHit {
                            rank: h.rank,
                            section_index: h.section_index,
                            score: h.hybrid_score,
                            bm25_score: h.bm25_score,
                            semantic_score: Some(h.semantic_score),
                            skg_score: Some(h.skg_score),
                            title: h.title,
                            filename: h.filename,
                            line_number: h.line_number,
                            body: h.body,
                            snippet,
                            entities: Vec::new(),
                        });
                    }

                    return Ok(SearchResults {
                        query: query.to_string(),
                        corrected_query: corrected_query_opt,
                        executed_mode: opts.mode,
                        alpha: opts.alpha,
                        graph_beta: beta,
                        hits,
                        total_sections: index.bm25.sections.len(),
                        found,
                        facets,
                        skg_seeds,
                        skg_neighbors,
                        warnings,
                    });
                }
                Err(err) => {
                    if opts.mode == SearchMode::HybridStrict {
                        return Err(format!("Semantic hybrid search failed: {}", err));
                    }
                    warnings.push(format!("Warning: Semantic hybrid search failed ({}), falling back to lexical BM25.", err));
                }
            }
        }
    }

    // 4. Lexical BM25 path
    let (lexical_params, lexical_variant) = (opts.bm25_params.clone(), opts.bm25_variant);

    // Graph and hybrid stages can reorder lexical hits. Only bound the
    // lexical collector once those stages are absent.
    let mut bm25_hits = if beta == 0.0 || skg_scores.is_empty() {
        index.bm25.search_top_k_filtered(
            &effective_query,
            lexical_variant,
            &lexical_params,
            index.tagger.as_ref(),
            opts.limit,
            allow.as_ref(),
        )
    } else {
        let mut hits = index.bm25.search(
            &effective_query,
            lexical_variant,
            &lexical_params,
            index.tagger.as_ref(),
        );
        if let Some(ref allow_bm) = allow {
            hits.retain(|h| allow_bm.contains(h.section_index as u32));
        }
        hits
    };
    crate::graph_search::apply_skg_boost(&mut bm25_hits, &skg_scores, beta);
    bm25_hits.truncate(opts.limit);

    // Only compute exhaustive candidates/facets if facets were requested.
    let (found, facets) = if !opts.facets.is_empty() {
        let candidate_bm =
            index
                .bm25
                .candidates(&effective_query, index.tagger.as_ref(), allow.as_ref());
        let match_ids = candidate_bm.iter();
        let computed = crate::meta::compute_facets(
            index.meta.as_ref(),
            &index.bm25,
            &candidate_bm,
            &match_ids,
            &opts.facets,
        );
        (candidate_bm.len(), Some(computed))
    } else {
        (bm25_hits.len(), None)
    };

    let mut hits = Vec::new();
    for (i, hit) in bm25_hits.iter().enumerate() {
        if let Some(sec) = index.bm25.sections.get(hit.section_index) {
            let filename = sec.filename.clone();
            let skg_score = skg_scores.get(&hit.section_index).copied();
            let snippet =
                best_snippet_with_cap(&sec.body, &effective_query, opts.max_snippet_chars);
            let entities = sec
                .entities
                .iter()
                .filter(|&e| e != "__LUME_PROCESSED__")
                .cloned()
                .collect();
            hits.push(SearchResultHit {
                rank: i + 1,
                section_index: hit.section_index,
                score: hit.score,
                bm25_score: hit.score,
                semantic_score: None,
                skg_score,
                title: sec.title.clone(),
                filename,
                line_number: sec.line_number,
                body: sec.body.clone(),
                snippet,
                entities,
            });
        }
    }

    Ok(SearchResults {
        query: query.to_string(),
        corrected_query: corrected_query_opt,
        executed_mode: SearchMode::LexicalOnly,
        alpha: opts.alpha,
        graph_beta: beta,
        hits,
        total_sections: index.bm25.sections.len(),
        found,
        facets,
        skg_seeds,
        skg_neighbors,
        warnings,
    })
}

pub fn format_cli_output(
    results: &SearchResults,
    index: &LoadedIndex,
    db_dir: &str,
) -> (String, String) {
    let mut stdout = String::new();
    let mut stderr = String::new();

    // 1. Header: Searching corpus: ...
    let target_dir = index
        .state
        .as_ref()
        .map(|s| s.target_dir.as_str())
        .unwrap_or("unknown");
    stdout.push_str(&format!(
        "Searching corpus: {} ({} sections, db: {})\n",
        target_dir, results.total_sections, db_dir
    ));

    // 2. Spell correction notice
    if let Some(ref corrected) = results.corrected_query {
        stdout.push_str(&format!("Corrected query to: {}\n", corrected));
    }

    // 3. SKG walk
    if results.graph_beta > 0.0 {
        if results.skg_seeds.is_empty() {
            stdout.push_str("[SKG walk] no query entities resolved — no graph boost\n");
        } else {
            let seeds_str = results.skg_seeds.join(", ");
            let neighbors_str = if results.skg_neighbors.is_empty() {
                "(none)".to_string()
            } else {
                results
                    .skg_neighbors
                    .iter()
                    .take(8)
                    .map(|(l, w)| format!("{} ({:.2})", l, w))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            stdout.push_str(&format!(
                "[SKG walk] seeds: {} → neighbors: {}\n",
                seeds_str, neighbors_str
            ));
        }
    }

    // 4. Warnings
    for w in &results.warnings {
        if w.starts_with("[⚠️]") {
            stdout.push_str(w);
            stdout.push('\n');
        } else {
            stderr.push_str(w);
            stderr.push('\n');
        }
    }

    // 5. Execution line & hits
    if results.executed_mode == SearchMode::LexicalOnly {
        stdout.push_str(&format!(
            "Executing lexical BM25 search (graph={})...\n",
            results.graph_beta
        ));
        if results.hits.is_empty() {
            stdout.push_str("No hits found.\n");
        } else {
            for hit in &results.hits {
                let filename = hit.filename.as_deref().unwrap_or("unknown");
                let skg_tag = match hit.skg_score {
                    Some(s) if s > 0.0 => format!(" [SKG: {:.2}]", s),
                    _ => String::new(),
                };
                stdout.push_str(&format!(
                    "[{}] Score: {:.4}{} | {} (File: {}, Line: {})\n",
                    hit.rank, hit.score, skg_tag, hit.title, filename, hit.line_number
                ));
                if !hit.entities.is_empty() {
                    stdout.push_str(&format!("  Entities: {:?}\n", hit.entities));
                }
                stdout.push_str(&format!("{}\n\n", hit.snippet));
            }
        }
    } else {
        stdout.push_str(&format!(
            "Executing hybrid search (alpha={}, graph={})...\n",
            results.alpha, results.graph_beta
        ));
        if results.hits.is_empty() {
            stdout.push_str("No hybrid hits found.\n");
        } else {
            for hit in &results.hits {
                let filename = hit.filename.as_deref().unwrap_or("unknown");
                stdout.push_str(&format!(
                    "[{}] Hybrid Score: {:.4} (BM25: {:.4}, Semantic: {:.4}, SKG: {:.2}) | {} (File: {}, Line: {})\n",
                    hit.rank,
                    hit.score,
                    hit.bm25_score,
                    hit.semantic_score.unwrap_or(0.0),
                    hit.skg_score.unwrap_or(0.0),
                    hit.title,
                    filename,
                    hit.line_number
                ));
                stdout.push_str(&format!("{}\n\n", hit.snippet));
            }
        }
    }

    // 6. Facets (if requested)
    if let Some(ref facets) = results.facets {
        stdout.push_str("Facets:\n");
        for (name, facet) in facets {
            match facet {
                crate::meta::FacetResult::Field { buckets, missing } => {
                    stdout.push_str(&format!("  {}:\n", name));
                    for b in buckets {
                        stdout.push_str(&format!("    {}: {}\n", b.val, b.count));
                    }
                    if *missing > 0 {
                        stdout.push_str(&format!("    (missing): {}\n", missing));
                    }
                }
                crate::meta::FacetResult::Range {
                    buckets,
                    before,
                    after,
                    missing,
                } => {
                    stdout.push_str(&format!("  {}:\n", name));
                    if *before > 0 {
                        stdout.push_str(&format!("    before: {}\n", before));
                    }
                    for b in buckets {
                        stdout.push_str(&format!("    [{}, {}): {}\n", b.from, b.to, b.count));
                    }
                    if *after > 0 {
                        stdout.push_str(&format!("    after: {}\n", after));
                    }
                    if *missing > 0 {
                        stdout.push_str(&format!("    (missing): {}\n", missing));
                    }
                }
                crate::meta::FacetResult::Query { count } => {
                    stdout.push_str(&format!("  {} (query): {}\n", name, count));
                }
            }
        }
    }

    (stdout, stderr)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_test_bm25() -> Bm25Index {
        let sections = vec![
            Section {
                title: "Chapter 1".to_string(),
                body: "The brave captain sailed across the stormy sea.".to_string(),
                line_number: 1,
                filename: Some("story.txt".to_string()),
                entities: vec!["captain".to_string()],
            },
            Section {
                title: "Chapter 2".to_string(),
                body: "Deep beneath the waves lay hidden ancient treasure.".to_string(),
                line_number: 25,
                filename: Some("story.txt".to_string()),
                entities: vec!["treasure".to_string()],
            },
        ];
        Bm25Index::build(sections, None)
    }

    #[test]
    fn resident_hybrid_search_does_not_walk_the_source_tree() {
        let dir = std::env::temp_dir().join(format!(
            "lume-hybrid-no-walk-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let missing_source = dir.join("source-no-longer-mounted");
        let target = missing_source.to_string_lossy().into_owned();
        let mut index = LoadedIndex::from_parts(build_test_bm25());
        index.state = Some(IndexState {
            format_version: CURRENT_FORMAT_VERSION,
            target_dir: target.clone(),
            db_dir: dir.to_string_lossy().into_owned(),
            semantic_enabled: true,
            ollama_entities: false,
            ollama_model: "test".to_string(),
            ollama_url: "http://localhost:11434".to_string(),
            tag_dict_path: None,
            semantic_session_id: Some("cached-session".to_string()),
            cached_files: HashMap::new(),
            stemmed: false,
            keep_hyphens: false,
        });
        index.cache_dir = Some(dir.clone());
        let (size, fingerprint) = *index.corpus_fingerprint.get().unwrap();
        let cache = crate::hybrid::SemanticQueryCache {
            server_url: None,
            corpus_path: target.clone(),
            corpus_size: size,
            corpus_mtime: fingerprint,
            queries: HashMap::from([(
                "captain".to_string(),
                vec![crate::hybrid::SearchResult {
                    chunk_id: "cached-chunk".to_string(),
                    score: 0.8,
                    text: "The brave captain".to_string(),
                    source: Some(crate::hybrid::section_hash(&index.bm25.sections[0])),
                }],
            )]),
        };
        crate::hybrid::save_semantic_cache_with_dir(&cache, Some(&dir));
        let opts = SearchOptions {
            mode: SearchMode::HybridStrict,
            graph_beta: 0.0,
            auth_token: Some("test-token".to_string()),
            ..Default::default()
        };
        for _ in 0..2 {
            let results = search(&index, "captain", &opts).unwrap();
            assert_eq!(results.executed_mode, SearchMode::HybridStrict);
            assert_eq!(results.hits[0].title, "Chapter 1");
            assert!(!missing_source.exists());
        }

        // A replacement snapshot changes the fingerprint even at equal byte size.
        let mut replacement = index.bm25.sections.clone();
        replacement[0].body = replacement[0].body.replace("brave", "stern");
        let changed = crate::hybrid::index_fingerprint(&replacement);
        assert_eq!(changed.0, size);
        assert_ne!(changed.1, fingerprint);
        assert!(crate::hybrid::load_semantic_cache_with_dir(
            &target, changed.0, changed.1, Some(&dir)
        ).queries.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_lexical_only_with_nonexistent_target_dir() {
        let bm25 = build_test_bm25();
        let state = IndexState {
            format_version: FORMAT_VERSION_STEMMED,
            target_dir: "/nonexistent/directory/that/does/not/exist/987654321".to_string(),
            db_dir: ".dummy-db".to_string(),
            semantic_enabled: false,
            ollama_entities: false,
            ollama_model: "test".to_string(),
            ollama_url: "http://localhost:11434".to_string(),
            tag_dict_path: None,
            semantic_session_id: None,
            cached_files: HashMap::new(),
            stemmed: true,
            keep_hyphens: false,
        };
        let index = LoadedIndex {
            state: Some(state),
            bm25,
            spelling: None,
            entity_graph: None,
            tagger: None,
            cache_dir: None,
            meta: None,
            corpus_fingerprint: OnceLock::new(),
        };

        let opts = SearchOptions {
            mode: SearchMode::LexicalOnly,
            graph_beta: 0.0,
            ..Default::default()
        };

        let results = search(&index, "captain", &opts)
            .expect("Lexical search should succeed without reading target_dir");
        assert_eq!(results.executed_mode, SearchMode::LexicalOnly);
        assert_eq!(results.hits.len(), 1);
        assert_eq!(results.hits[0].title, "Chapter 1");
    }

    #[test]
    fn test_hybrid_fallback_on_missing_session() {
        let bm25 = build_test_bm25();
        let state = IndexState {
            format_version: FORMAT_VERSION_STEMMED,
            target_dir: "/dummy/target".to_string(),
            db_dir: ".dummy-db".to_string(),
            semantic_enabled: false,
            ollama_entities: false,
            ollama_model: "test".to_string(),
            ollama_url: "http://localhost:11434".to_string(),
            tag_dict_path: None,
            semantic_session_id: None,
            cached_files: HashMap::new(),
            stemmed: true,
            keep_hyphens: false,
        };
        let index = LoadedIndex {
            state: Some(state),
            bm25,
            spelling: None,
            entity_graph: None,
            tagger: None,
            cache_dir: None,
            meta: None,
            corpus_fingerprint: OnceLock::new(),
        };

        let opts = SearchOptions {
            mode: SearchMode::HybridOrFallback,
            alpha: 0.5,
            graph_beta: 0.0,
            ..Default::default()
        };

        let results = search(&index, "captain", &opts).expect("Search should fall back to lexical");
        assert_eq!(results.executed_mode, SearchMode::LexicalOnly);
        assert!(results
            .warnings
            .iter()
            .any(|w| w.contains("Semantic search unavailable")));
        assert_eq!(results.hits.len(), 1);
        assert_eq!(results.hits[0].title, "Chapter 1");
    }

    #[test]
    fn test_hybrid_strict_fails_on_missing_session() {
        let bm25 = build_test_bm25();
        let state = IndexState {
            format_version: FORMAT_VERSION_STEMMED,
            target_dir: "/dummy/target".to_string(),
            db_dir: ".dummy-db".to_string(),
            semantic_enabled: false,
            ollama_entities: false,
            ollama_model: "test".to_string(),
            ollama_url: "http://localhost:11434".to_string(),
            tag_dict_path: None,
            semantic_session_id: None,
            cached_files: HashMap::new(),
            stemmed: true,
            keep_hyphens: false,
        };
        let index = LoadedIndex {
            state: Some(state),
            bm25,
            spelling: None,
            entity_graph: None,
            tagger: None,
            cache_dir: None,
            meta: None,
            corpus_fingerprint: OnceLock::new(),
        };

        let opts = SearchOptions {
            mode: SearchMode::HybridStrict,
            alpha: 0.5,
            graph_beta: 0.0,
            ..Default::default()
        };

        let res = search(&index, "captain", &opts);
        assert!(
            res.is_err(),
            "HybridStrict must error when semantic session is missing"
        );
    }

    #[test]
    fn test_from_parts_lexical_search() {
        let bm25 = build_test_bm25();
        let index = LoadedIndex::from_parts(bm25);
        let opts = SearchOptions {
            mode: SearchMode::LexicalOnly,
            graph_beta: 0.0,
            ..Default::default()
        };
        let results =
            search(&index, "treasure", &opts).expect("Lexical search from parts should succeed");
        assert_eq!(results.hits.len(), 1);
        assert_eq!(results.hits[0].title, "Chapter 2");
    }

    #[test]
    fn concurrent_readers_never_observe_a_partial_index() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::sync::Arc;
        use std::thread;

        let dir = std::env::temp_dir().join(format!(
            "lume-atomic-index-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bm25.json");
        let generations = 40u64;
        save_json(&path, &serde_json::json!({"generation": 0, "sections": 1})).unwrap();

        let stop = Arc::new(AtomicBool::new(false));
        let errors = Arc::new(AtomicUsize::new(0));
        let mut readers = Vec::new();
        for _ in 0..2 {
            let path = path.clone();
            let stop = Arc::clone(&stop);
            let errors = Arc::clone(&errors);
            readers.push(thread::spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    match load_json::<serde_json::Value>(&path) {
                        Ok(value) => {
                            let generation = value.get("generation").and_then(|v| v.as_u64());
                            let sections = value.get("sections").and_then(|v| v.as_u64());
                            if generation.is_none() || sections != Some(1) {
                                errors.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        Err(_) => {
                            errors.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            }));
        }

        for generation in 1..generations {
            save_json(
                &path,
                &serde_json::json!({"generation": generation, "sections": 1}),
            )
            .unwrap();
        }
        stop.store(true, Ordering::Release);
        for reader in readers {
            reader.join().unwrap();
        }

        let final_doc: serde_json::Value = load_json(&path).unwrap();
        assert_eq!(final_doc["generation"], generations - 1);
        assert_eq!(final_doc["sections"], 1);
        assert_eq!(
            errors.load(Ordering::Relaxed),
            0,
            "a reader observed a partial or unreadable index file"
        );
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("bm25.json")]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_stemming_mismatch_refusal() {
        let temp_dir = std::env::temp_dir().join("lume_test_stem_mismatch");
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();

        let state = IndexState {
            format_version: 1,
            target_dir: "/dummy".to_string(),
            db_dir: temp_dir.display().to_string(),
            semantic_enabled: false,
            ollama_entities: false,
            ollama_model: String::new(),
            ollama_url: String::new(),
            tag_dict_path: None,
            semantic_session_id: None,
            cached_files: HashMap::new(),
            stemmed: false,
            keep_hyphens: false,
        };
        save_json(&temp_dir.join("state.json"), &state).unwrap();
        let bm25 = Bm25Index::build_with_options(
            vec![],
            None,
            crate::bm25::Bm25BuildOptions {
                stemmed: false,
                keep_hyphens: false,
            },
        );
        save_json(&temp_dir.join("bm25.json"), &bm25).unwrap();

        // 1. With no checks requested, opening succeeds and uses state.stemmed (false)
        let loaded = LoadedIndex::open_with_checks(&temp_dir, OpenEnvChecks::default()).unwrap();
        assert!(!loaded.bm25.stemmed);

        // 2. With check_stem = Some(true), opening unstemmed index fails with mismatch error
        let err = match LoadedIndex::open_with_checks(
            &temp_dir,
            OpenEnvChecks {
                check_stem: Some(true),
                check_keep_hyphens: None,
            },
        ) {
            Ok(_) => panic!("expected Err"),
            Err(e) => e,
        };
        assert!(err.contains("Stemming configuration mismatch"));
        assert!(err.contains("index was built with stemmed=false, but LUME_STEM=1 was requested"));

        // 3. Now test a stemmed index
        let state_stemmed = IndexState {
            format_version: FORMAT_VERSION_STEMMED,
            stemmed: true,
            ..state
        };
        save_json(&temp_dir.join("state.json"), &state_stemmed).unwrap();

        // With check_stem = Some(true), matches index
        let loaded_stemmed = LoadedIndex::open_with_checks(
            &temp_dir,
            OpenEnvChecks {
                check_stem: Some(true),
                check_keep_hyphens: None,
            },
        )
        .unwrap();
        assert!(loaded_stemmed.bm25.stemmed);

        // With check_stem = Some(false), opening stemmed index fails with mismatch error
        let err2 = match LoadedIndex::open_with_checks(
            &temp_dir,
            OpenEnvChecks {
                check_stem: Some(false),
                check_keep_hyphens: None,
            },
        ) {
            Ok(_) => panic!("expected Err"),
            Err(e) => e,
        };
        assert!(err2.contains("Stemming configuration mismatch"));
        assert!(err2.contains("index was built with stemmed=true, but LUME_STEM=0 was requested"));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_legacy_keep_hyphens_refusal() {
        let temp_dir = std::env::temp_dir().join("lume_test_legacy_hyphens");
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();

        let state = IndexState {
            format_version: 1,
            target_dir: "/dummy".to_string(),
            db_dir: temp_dir.display().to_string(),
            semantic_enabled: false,
            ollama_entities: false,
            ollama_model: String::new(),
            ollama_url: String::new(),
            tag_dict_path: None,
            semantic_session_id: None,
            cached_files: HashMap::new(),
            stemmed: true,
            keep_hyphens: false,
        };
        save_json(&temp_dir.join("state.json"), &state).unwrap();
        let bm25 = Bm25Index::build(vec![], None);
        save_json(&temp_dir.join("bm25.json"), &bm25).unwrap();

        // 1. Index with keep_hyphens=false opens cleanly
        let loaded = LoadedIndex::open(&temp_dir).unwrap();
        assert!(!loaded.bm25.keep_hyphens);

        // 2. Index with keep_hyphens=true is refused with legacy deprecation error
        let state_hyphens = IndexState {
            keep_hyphens: true,
            ..state
        };
        save_json(&temp_dir.join("state.json"), &state_hyphens).unwrap();

        let err = match LoadedIndex::open(&temp_dir) {
            Ok(_) => panic!("expected Err for legacy keep_hyphens=true index"),
            Err(e) => e,
        };
        assert!(err.contains(
            "Index was built with legacy keep_hyphens=true, which is no longer supported; please reindex with 'lume index -f'."
        ));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_old_index_without_stemmed_field_works_unstemmed_and_prints_notice() {
        let temp_dir = std::env::temp_dir().join("lume_test_legacy_unstemmed_notice");
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();

        // Write a legacy state.json without the "stemmed" key
        let legacy_state_json = r#"{
            "target_dir": "/dummy",
            "db_dir": "/dummy",
            "semantic_enabled": false,
            "ollama_entities": false,
            "ollama_model": "",
            "ollama_url": "",
            "tag_dict_path": null,
            "semantic_session_id": null,
            "cached_files": {}
        }"#;
        std::fs::write(temp_dir.join("state.json"), legacy_state_json).unwrap();
        let bm25 = build_test_bm25();
        save_json(&temp_dir.join("bm25.json"), &bm25).unwrap();

        let loaded =
            LoadedIndex::open(&temp_dir).expect("Legacy unstemmed index should open without error");
        assert!(
            !loaded.bm25.stemmed,
            "Legacy index without 'stemmed' field must be treated as unstemmed"
        );

        let opts = SearchOptions {
            mode: SearchMode::LexicalOnly,
            graph_beta: 0.0,
            ..Default::default()
        };
        let res = search(&loaded, "captain", &opts).expect("Search should succeed");
        assert!(
            res.warnings
                .iter()
                .any(|w| w.contains("Notice: this index was built without stemming; run 'lume index -f' to reindex with default stemming.")),
            "Expected notice when searching unstemmed index, got: {:?}",
            res.warnings
        );

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_default_index_and_search_uses_stemming() {
        let temp_dir = std::env::temp_dir().join("lume_test_default_stemming");
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();

        let state = IndexState {
            format_version: FORMAT_VERSION_STEMMED,
            target_dir: "/dummy".to_string(),
            db_dir: temp_dir.display().to_string(),
            semantic_enabled: false,
            ollama_entities: false,
            ollama_model: String::new(),
            ollama_url: String::new(),
            tag_dict_path: None,
            semantic_session_id: None,
            cached_files: HashMap::new(),
            stemmed: true,
            keep_hyphens: false,
        };
        save_json(&temp_dir.join("state.json"), &state).unwrap();
        let bm25 = build_test_bm25();
        save_json(&temp_dir.join("bm25.json"), &bm25).unwrap();

        let loaded = LoadedIndex::open(&temp_dir).unwrap();
        assert!(loaded.bm25.stemmed, "Default index should be stemmed");

        let opts = SearchOptions {
            mode: SearchMode::LexicalOnly,
            graph_beta: 0.0,
            ..Default::default()
        };
        // "sailing" stems to "sail", matching "sailed" in Chapter 1
        let res = search(&loaded, "sailing", &opts).expect("Search should succeed");
        assert_eq!(res.hits.len(), 1);
        assert_eq!(res.hits[0].title, "Chapter 1");
        assert!(
            !res.warnings
                .iter()
                .any(|w| w.contains("Notice: this index was built without stemming")),
            "Stemmed index must not emit unstemmed notice"
        );

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_search_path_honors_explicit_bm25_params() {
        let bm25 = build_test_bm25();
        let state = IndexState {
            format_version: FORMAT_VERSION_STEMMED,
            target_dir: "/dummy/target".to_string(),
            db_dir: ".dummy-db".to_string(),
            semantic_enabled: false,
            ollama_entities: false,
            ollama_model: "test".to_string(),
            ollama_url: "http://localhost:11434".to_string(),
            tag_dict_path: None,
            semantic_session_id: None,
            cached_files: HashMap::new(),
            stemmed: true,
            keep_hyphens: false,
        };
        let index = LoadedIndex {
            state: Some(state),
            bm25,
            spelling: None,
            entity_graph: None,
            tagger: None,
            cache_dir: None,
            meta: None,
            corpus_fingerprint: OnceLock::new(),
        };

        // 1. Lexical search with default params (coord_floor = 1.0, unpenalized)
        let default_opts = SearchOptions {
            mode: SearchMode::LexicalOnly,
            graph_beta: 0.0,
            ..Default::default()
        };
        let default_res = search(&index, "captain treasure", &default_opts).unwrap();
        assert!(!default_res.hits.is_empty());
        let default_score = default_res.hits[0].score;

        // 2. Lexical search with explicit coord_floor = 0.5 (penalized)
        let penalized_params = Bm25Params {
            coord_floor: 0.5,
            ..Default::default()
        };
        let penalized_opts = SearchOptions {
            mode: SearchMode::LexicalOnly,
            graph_beta: 0.0,
            bm25_params: penalized_params,
            ..Default::default()
        };
        let penalized_res = search(&index, "captain treasure", &penalized_opts).unwrap();
        assert!(!penalized_res.hits.is_empty());
        let penalized_score = penalized_res.hits[0].score;

        assert!(default_score > penalized_score);
        let ratio = default_score / penalized_score;
        assert!((ratio - (1.0 / 0.75)).abs() < 1e-4);

        // 3. HybridOrFallback mode (falls back to lexical due to None semantic_session_id)
        // With default params, score must be identical to lexical default
        let fb_default_opts = SearchOptions {
            mode: SearchMode::HybridOrFallback,
            alpha: 0.5,
            graph_beta: 0.0,
            ..Default::default()
        };
        let fb_default_res = search(&index, "captain treasure", &fb_default_opts).unwrap();
        assert_eq!(fb_default_res.hits[0].score, default_score);

        // With explicit custom params in HybridOrFallback mode, score must match penalized_score
        let fb_penalized_opts = SearchOptions {
            mode: SearchMode::HybridOrFallback,
            alpha: 0.5,
            graph_beta: 0.0,
            bm25_params: Bm25Params {
                coord_floor: 0.5,
                ..Default::default()
            },
            ..Default::default()
        };
        let fb_penalized_res = search(&index, "captain treasure", &fb_penalized_opts).unwrap();
        assert_eq!(fb_penalized_res.hits[0].score, penalized_score);
    }

    #[test]
    fn test_unknown_higher_format_version_refusal() {
        let temp_dir = std::env::temp_dir().join(format!(
            "lume_test_higher_version_{}_{}",
            std::process::id(),
            crate::uuid_v4()
        ));
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();

        let higher_version_state_json = r#"{
            "format_version": 99,
            "target_dir": "/tmp/dummy",
            "db_dir": ".dummy-db",
            "semantic_enabled": false,
            "ollama_entities": false,
            "ollama_model": "test",
            "ollama_url": "http://localhost:11434",
            "tag_dict_path": null,
            "semantic_session_id": null,
            "cached_files": {},
            "stemmed": true,
            "keep_hyphens": false
        }"#;
        std::fs::write(temp_dir.join("state.json"), higher_version_state_json).unwrap();

        let err = match LoadedIndex::open(&temp_dir) {
            Ok(_) => panic!("expected Err for higher format_version index"),
            Err(e) => e,
        };
        assert!(
            err.contains("rebuild with a newer lume or reindex"),
            "expected 'rebuild with a newer lume or reindex' in error, got: {}",
            err
        );
        assert!(
            err.contains("format version 99"),
            "expected format version 99 in error, got: {}",
            err
        );

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_search_not_warnings_and_filtering() {
        let sec0 = Section {
            title: "Therapy Research".to_string(),
            body: "Cancer therapy clinical trial outcomes.".to_string(),
            line_number: 1,
            filename: Some("sec0.md".to_string()),
            entities: Vec::new(),
        };
        let sec1 = Section {
            title: "Genetics Study".to_string(),
            body: "Cancer mutations and tumor genetics.".to_string(),
            line_number: 10,
            filename: Some("sec1.md".to_string()),
            entities: Vec::new(),
        };
        let bm25 = Bm25Index::build(vec![sec0, sec1], None);
        let index = LoadedIndex {
            state: None,
            bm25,
            spelling: None,
            entity_graph: None,
            tagger: None,
            cache_dir: None,
            meta: None,
            corpus_fingerprint: OnceLock::new(),
        };

        let opts = SearchOptions {
            mode: SearchMode::LexicalOnly,
            graph_beta: 0.0,
            ..Default::default()
        };

        // Query with -the (stopword) generates a warning and ignores the exclusion
        let res_stopword = search(&index, "cancer -the", &opts).unwrap();
        assert_eq!(res_stopword.hits.len(), 2);
        assert!(res_stopword
            .warnings
            .iter()
            .any(|w| w.contains("excluded term 'the' is a stopword and was ignored")));

        // Query with -therapy excludes sec0
        let res_minus = search(&index, "cancer -therapy", &opts).unwrap();
        assert_eq!(res_minus.hits.len(), 1);
        assert_eq!(res_minus.hits[0].title, "Genetics Study");

        // Query with only NOT terms returns 0 hits and a warning
        let res_only_not = search(&index, "-therapy", &opts).unwrap();
        assert_eq!(res_only_not.hits.len(), 0);
        assert!(res_only_not
            .warnings
            .iter()
            .any(|w| w.contains("query contains only excluded terms")));
    }

    #[test]
    fn test_format_version_2_loads_with_meta_none() {
        let temp_dir = std::env::temp_dir().join(format!(
            "lume_test_v2_{}_{}",
            std::process::id(),
            crate::uuid_v4()
        ));
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();

        let state_json = r#"{
            "format_version": 2,
            "target_dir": "/tmp/dummy",
            "db_dir": ".dummy-db",
            "semantic_enabled": false,
            "ollama_entities": false,
            "ollama_model": "test",
            "ollama_url": "http://localhost:11434",
            "tag_dict_path": null,
            "semantic_session_id": null,
            "cached_files": {},
            "stemmed": true,
            "keep_hyphens": false
        }"#;
        std::fs::write(temp_dir.join("state.json"), state_json).unwrap();

        let sec = Section {
            title: "Sec".to_string(),
            body: "Body text".to_string(),
            line_number: 1,
            filename: Some("sec.md".to_string()),
            entities: Vec::new(),
        };
        let bm25 = Bm25Index::build(vec![sec], None);
        save_json(&temp_dir.join("bm25.json"), &bm25).unwrap();

        let loaded = LoadedIndex::open(&temp_dir).unwrap();
        assert!(loaded.meta.is_none());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_format_version_3_missing_meta_json_error() {
        let temp_dir = std::env::temp_dir().join(format!(
            "lume_test_v3_missing_{}_{}",
            std::process::id(),
            crate::uuid_v4()
        ));
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();

        let state_json = r#"{
            "format_version": 3,
            "target_dir": "/tmp/dummy",
            "db_dir": ".dummy-db",
            "semantic_enabled": false,
            "ollama_entities": false,
            "ollama_model": "test",
            "ollama_url": "http://localhost:11434",
            "tag_dict_path": null,
            "semantic_session_id": null,
            "cached_files": {},
            "stemmed": true,
            "keep_hyphens": false
        }"#;
        std::fs::write(temp_dir.join("state.json"), state_json).unwrap();

        let sec = Section {
            title: "Sec".to_string(),
            body: "Body text".to_string(),
            line_number: 1,
            filename: Some("sec.md".to_string()),
            entities: Vec::new(),
        };
        let bm25 = Bm25Index::build(vec![sec], None);
        save_json(&temp_dir.join("bm25.json"), &bm25).unwrap();

        let err = LoadedIndex::open(&temp_dir)
            .expect_err("expected error loading index without meta.json");
        assert!(
            err.contains("requires meta.json"),
            "expected requires meta.json, got: {}",
            err
        );
        assert!(
            err.contains("reindex"),
            "expected reindex in error, got: {}",
            err
        );

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_format_version_3_section_count_mismatch_error() {
        let temp_dir = std::env::temp_dir().join(format!(
            "lume_test_v3_mismatch_{}_{}",
            std::process::id(),
            crate::uuid_v4()
        ));
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();

        let state_json = r#"{
            "format_version": 3,
            "target_dir": "/tmp/dummy",
            "db_dir": ".dummy-db",
            "semantic_enabled": false,
            "ollama_entities": false,
            "ollama_model": "test",
            "ollama_url": "http://localhost:11434",
            "tag_dict_path": null,
            "semantic_session_id": null,
            "cached_files": {},
            "stemmed": true,
            "keep_hyphens": false
        }"#;
        std::fs::write(temp_dir.join("state.json"), state_json).unwrap();

        let sec = Section {
            title: "Sec".to_string(),
            body: "Body text".to_string(),
            line_number: 1,
            filename: Some("sec.md".to_string()),
            entities: Vec::new(),
        };
        let bm25 = Bm25Index::build(vec![sec], None);
        save_json(&temp_dir.join("bm25.json"), &bm25).unwrap();

        // Write meta.json with num_sections: 5 (mismatch with bm25.sections.len() = 1)
        let meta_disk = crate::meta::MetaIndexOnDisk {
            meta_version: 1,
            num_sections: 5,
            generation: "gen-1".to_string(),
            schema: std::collections::HashMap::new(),
            files: std::collections::HashMap::new(),
            columns: std::collections::HashMap::new(),
        };
        save_json(&temp_dir.join("meta.json"), &meta_disk).unwrap();

        let err = LoadedIndex::open(&temp_dir)
            .expect_err("expected error loading index with section count mismatch");
        assert!(
            err.contains("Index metadata section count mismatch"),
            "expected section count mismatch, got: {}",
            err
        );
        assert!(
            err.contains("reindex"),
            "expected reindex in error, got: {}",
            err
        );

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_search_filters_and_bit_identical_scores() {
        let sec0 = Section {
            title: "Cancer Therapy".to_string(),
            body: "Cancer therapy clinical trial outcomes.".to_string(),
            line_number: 1,
            filename: Some("sec0.md".to_string()),
            entities: Vec::new(),
        };
        let sec1 = Section {
            title: "Genetics Study".to_string(),
            body: "Cancer mutations and tumor genetics.".to_string(),
            line_number: 10,
            filename: Some("sec1.md".to_string()),
            entities: Vec::new(),
        };
        let bm25 = Bm25Index::build(vec![sec0, sec1], None);

        let mut files = HashMap::new();
        let mut f0 = HashMap::new();
        f0.insert("category".to_string(), serde_json::json!("biology"));
        f0.insert("year".to_string(), serde_json::json!(2020));
        files.insert("sec0.md".to_string(), ("manifest".to_string(), f0));

        let mut f1 = HashMap::new();
        f1.insert("category".to_string(), serde_json::json!("physics"));
        f1.insert("year".to_string(), serde_json::json!(2022));
        files.insert("sec1.md".to_string(), ("manifest".to_string(), f1));

        let meta = crate::meta::build_meta_index(
            &["sec0.md".to_string(), "sec1.md".to_string()],
            &files,
            &HashMap::new(),
            "gen-filter-test",
        );

        let index = LoadedIndex {
            state: None,
            bm25,
            spelling: None,
            entity_graph: None,
            tagger: None,
            cache_dir: None,
            meta,
        };

        let opts = SearchOptions {
            mode: SearchMode::LexicalOnly,
            graph_beta: 0.0,
            ..Default::default()
        };

        // 1. Unfiltered query
        let res_unfiltered = search(&index, "cancer", &opts).unwrap();
        assert_eq!(res_unfiltered.hits.len(), 2);
        let score_sec0 = res_unfiltered
            .hits
            .iter()
            .find(|h| h.section_index == 0)
            .unwrap()
            .score;
        let score_sec1 = res_unfiltered
            .hits
            .iter()
            .find(|h| h.section_index == 1)
            .unwrap()
            .score;

        // 2. Positive filter: category:biology
        let res_bio = search(&index, "cancer category:biology", &opts).unwrap();
        assert_eq!(res_bio.hits.len(), 1);
        assert_eq!(res_bio.hits[0].section_index, 0);
        // Assert BIT-IDENTICAL score
        assert_eq!(res_bio.hits[0].score.to_bits(), score_sec0.to_bits());

        // 3. Negated filter: -category:biology
        let res_not_bio = search(&index, "cancer -category:biology", &opts).unwrap();
        assert_eq!(res_not_bio.hits.len(), 1);
        assert_eq!(res_not_bio.hits[0].section_index, 1);
        // Assert BIT-IDENTICAL score
        assert_eq!(res_not_bio.hits[0].score.to_bits(), score_sec1.to_bits());

        // 4. Range filter: year:>=2021
        let res_range = search(&index, "cancer year:>=2021", &opts).unwrap();
        assert_eq!(res_range.hits.len(), 1);
        assert_eq!(res_range.hits[0].section_index, 1);
        assert_eq!(res_range.hits[0].score.to_bits(), score_sec1.to_bits());

        // 5. Unknown prefix treated as text: foo:bar
        let res_unknown = search(&index, "cancer foo:bar", &opts).unwrap();
        assert_eq!(res_unknown.hits.len(), 2);

        // 6. Filter-only query: score must be 0.0
        let res_filter_only = search(&index, "category:biology", &opts).unwrap();
        assert_eq!(res_filter_only.hits.len(), 1);
        assert_eq!(res_filter_only.hits[0].section_index, 0);
        assert_eq!(res_filter_only.hits[0].score, 0.0);

        // 7. Combining filter with -term exclusion
        let res_comb = search(&index, "cancer -mutations category:biology", &opts).unwrap();
        assert_eq!(res_comb.hits.len(), 1);
        assert_eq!(res_comb.hits[0].section_index, 0);
        assert_eq!(res_comb.hits[0].score.to_bits(), score_sec0.to_bits());
    }

    #[test]
    fn test_facets_invariance_and_oracle() {
        let sec0 = Section {
            title: "Sec 0".into(),
            body: "cancer biology dna genetics".into(),
            line_number: 1,
            filename: Some("d0.txt".into()),
            entities: vec![],
        };
        let sec1 = Section {
            title: "Sec 1".into(),
            body: "cancer therapy dna clinical".into(),
            line_number: 1,
            filename: Some("d1.txt".into()),
            entities: vec![],
        };
        let sec2 = Section {
            title: "Sec 2".into(),
            body: "astronomy stars telescope".into(),
            line_number: 1,
            filename: Some("d2.txt".into()),
            entities: vec![],
        };
        let sec3 = Section {
            title: "Sec 3".into(),
            body: "cancer overview epidemiology".into(),
            line_number: 1,
            filename: Some("d3.txt".into()),
            entities: vec![],
        };
        let bm25 = Bm25Index::build(vec![sec0, sec1, sec2, sec3], None);

        let mut schema = HashMap::new();
        schema.insert("category".into(), crate::meta::FieldType::Keyword);
        schema.insert("tags".into(), crate::meta::FieldType::KeywordList);
        schema.insert("year".into(), crate::meta::FieldType::Integer);

        let mut columns = HashMap::new();
        columns.insert(
            "category".into(),
            crate::meta::Column::Keyword {
                dict: vec!["biology".into(), "medicine".into()],
                ords: vec![Some(0), Some(1), None, None],
                bitmaps: vec![
                    crate::fast_retrieval::MiniRoaring::from_sorted(&[0]),
                    crate::fast_retrieval::MiniRoaring::from_sorted(&[1]),
                ],
            },
        );
        columns.insert(
            "tags".into(),
            crate::meta::Column::KeywordList {
                dict: vec!["dna".into(), "rna".into(), "general".into()],
                offsets: vec![0, 2, 3, 3, 4],
                ords: vec![0, 1, 0, 2],
                bitmaps: vec![
                    crate::fast_retrieval::MiniRoaring::from_sorted(&[0, 1]),
                    crate::fast_retrieval::MiniRoaring::from_sorted(&[0]),
                    crate::fast_retrieval::MiniRoaring::from_sorted(&[3]),
                ],
            },
        );
        columns.insert(
            "year".into(),
            crate::meta::Column::Integer {
                present_runs: vec![[0, 3]],
                values: vec![Some(2015), Some(2025), Some(2022), None],
            },
        );

        let meta = crate::meta::MetaIndex {
            meta_version: 1,
            num_sections: 4,
            generation: "gen1".into(),
            schema,
            files: HashMap::new(),
            columns,
        };

        let index = LoadedIndex {
            state: None,
            bm25,
            spelling: None,
            entity_graph: None,
            tagger: None,
            cache_dir: None,
            meta: Some(meta),
        };

        let facet_reqs = vec![
            crate::meta::FacetRequest::Field("category".into()),
            crate::meta::FacetRequest::Field("tags".into()),
            crate::meta::FacetRequest::Range {
                field: "year".into(),
                start: 2020.0,
                end: 2030.0,
                gap: 5.0,
            },
            crate::meta::FacetRequest::Query {
                name: "therapy".into(),
                query: "therapy".into(),
            },
        ];

        // 1. Run search with -l 1
        let opts_l1 = SearchOptions {
            limit: 1,
            mode: SearchMode::LexicalOnly,
            graph_beta: 0.0,
            facets: facet_reqs.clone(),
            ..Default::default()
        };
        let res_l1 = search(&index, "cancer", &opts_l1).unwrap();

        // 2. Run search with -l 1000
        let opts_l1000 = SearchOptions {
            limit: 1000,
            mode: SearchMode::LexicalOnly,
            graph_beta: 0.0,
            facets: facet_reqs.clone(),
            ..Default::default()
        };
        let res_l1000 = search(&index, "cancer", &opts_l1000).unwrap();

        // Check limit invariance
        assert_eq!(res_l1.hits.len(), 1);
        assert_eq!(res_l1000.hits.len(), 3);
        assert_eq!(res_l1.found, 3);
        assert_eq!(res_l1000.found, 3);
        assert_eq!(res_l1.facets, res_l1000.facets);

        // Check brute-force equivalence
        let facets = res_l1.facets.unwrap();
        // Category: sec0 is biology, sec1 is medicine, sec3 is missing
        if let Some(crate::meta::FacetResult::Field { buckets, missing }) = facets.get("category") {
            assert_eq!(*missing, 1);
            let bio = buckets.iter().find(|b| b.val == "biology").unwrap();
            assert_eq!(bio.count, 1);
            let med = buckets.iter().find(|b| b.val == "medicine").unwrap();
            assert_eq!(med.count, 1);
        } else {
            panic!("Expected field facet category");
        }

        // Tags multi-valued sum >= found
        if let Some(crate::meta::FacetResult::Field { buckets, missing }) = facets.get("tags") {
            assert_eq!(*missing, 0);
            let sum: usize = buckets.iter().map(|b| b.count).sum();
            assert!(sum >= res_l1.found);
            let dna = buckets.iter().find(|b| b.val == "dna").unwrap();
            assert_eq!(dna.count, 2);
        } else {
            panic!("Expected field facet tags");
        }

        // Year range: [2020, 2025), [2025, 2030)
        // Matching hits: sec0 (2015 -> before), sec1 (2025 -> bucket 1), sec3 (missing)
        if let Some(crate::meta::FacetResult::Range {
            buckets,
            before,
            after,
            missing,
        }) = facets.get("year")
        {
            assert_eq!(*before, 1);
            assert_eq!(*after, 0);
            assert_eq!(*missing, 1);
            assert_eq!(buckets[0].count, 0);
            assert_eq!(buckets[1].count, 1);
        } else {
            panic!("Expected range facet year");
        }

        // Query facet: therapy
        if let Some(crate::meta::FacetResult::Query { count }) = facets.get("therapy") {
            assert_eq!(*count, 1);
        } else {
            panic!("Expected query facet therapy");
        }
    }

    #[test]
    fn test_filter_before_scoring_exact_parity() {
        let sections = vec![
            Section {
                title: "Cancer biology".into(),
                body: "Cancer cells divide rapidly in biology tissue".into(),
                line_number: 1,
                filename: Some("doc1.txt".into()),
                entities: Vec::new(),
            },
            Section {
                title: "Cancer treatment".into(),
                body: "Cancer therapy and medicine advances".into(),
                line_number: 2,
                filename: Some("doc2.txt".into()),
                entities: Vec::new(),
            },
            Section {
                title: "Physics of radiation".into(),
                body: "Radiation physics and photon beams".into(),
                line_number: 3,
                filename: Some("doc3.txt".into()),
                entities: Vec::new(),
            },
            Section {
                title: "Cancer study".into(),
                body: "Cancer research across multiple domains".into(),
                line_number: 4,
                filename: Some("doc4.txt".into()),
                entities: Vec::new(),
            },
        ];
        let bm25 = Bm25Index::build(sections, None);

        let mut schema = HashMap::new();
        schema.insert("category".into(), crate::meta::FieldType::Keyword);

        let mut columns = HashMap::new();
        columns.insert(
            "category".into(),
            crate::meta::Column::Keyword {
                dict: vec!["biology".into(), "medicine".into(), "physics".into()],
                ords: vec![Some(0), Some(1), Some(2), None],
                bitmaps: vec![
                    crate::fast_retrieval::MiniRoaring::from_sorted(&[0]),
                    crate::fast_retrieval::MiniRoaring::from_sorted(&[1]),
                    crate::fast_retrieval::MiniRoaring::from_sorted(&[2]),
                ],
            },
        );

        let meta = crate::meta::MetaIndex {
            meta_version: 1,
            num_sections: 4,
            generation: "gen-fbs".into(),
            schema,
            files: HashMap::new(),
            columns,
        };

        let index = LoadedIndex {
            state: None,
            bm25,
            spelling: None,
            entity_graph: None,
            tagger: None,
            cache_dir: None,
            meta: Some(meta),
        };

        let opts = SearchOptions {
            limit: 10,
            mode: SearchMode::LexicalOnly,
            graph_beta: 0.0,
            ..Default::default()
        };

        // Search with filter category:biology
        let res = search(&index, "cancer category:biology", &opts).unwrap();
        assert_eq!(res.hits.len(), 1);
        assert_eq!(res.found, 1);
        assert_eq!(res.hits[0].section_index, 0);

        // Compare score with unfiltered cancer search hit for doc 0
        let res_unfiltered = search(&index, "cancer", &opts).unwrap();
        let doc0_unfiltered = res_unfiltered
            .hits
            .iter()
            .find(|h| h.section_index == 0)
            .unwrap();
        assert_eq!(res.hits[0].score.to_bits(), doc0_unfiltered.score.to_bits());

        // Negative filter -category:biology
        let res_neg = search(&index, "cancer -category:biology", &opts).unwrap();
        assert_eq!(res_neg.hits.len(), 2);
        assert_eq!(res_neg.found, 2);
        assert!(!res_neg.hits.iter().any(|h| h.section_index == 0));
    }
}
