use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};

use crate::bm25::{Bm25Index, Bm25Params, SearchVariant, Section, filter_query_stopwords};
use crate::spelling::SpellIndex;
use crate::semantic_mesh::EntityGraph;
use crate::Tagger;
use crate::Entry;
use crate::tokenize;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct IndexState {
    pub target_dir: String,
    pub db_dir: String,
    pub semantic_enabled: bool,
    pub ollama_entities: bool,
    pub ollama_model: String,
    pub ollama_url: String,
    pub tag_dict_path: Option<String>,
    pub semantic_session_id: Option<String>,
    pub cached_files: HashMap<String, (u64, Vec<Section>)>,
}

pub struct LoadedIndex {
    pub state: Option<IndexState>,
    pub bm25: Bm25Index,
    pub spelling: Option<SpellIndex>,
    pub entity_graph: Option<EntityGraph>,
    pub tagger: Option<Tagger>,
    pub cache_dir: Option<PathBuf>,
}

impl LoadedIndex {
    pub fn open(db_dir: impl AsRef<Path>) -> Result<Self, String> {
        let db_path = db_dir.as_ref();
        let state_path = db_path.join("state.json");
        if !state_path.exists() {
            return Err(format!(
                "Index state file not found at {}. Index the directory first.",
                state_path.display()
            ));
        }
        let state: IndexState = load_json(&state_path)?;

        let bm25_path = db_path.join("bm25.json");
        if !bm25_path.exists() {
            return Err(format!(
                "Index bm25.json not found at {}. Index the directory first.",
                bm25_path.display()
            ));
        }
        let bm25: Bm25Index = load_json(&bm25_path)?;
        let spelling: Option<SpellIndex> = load_json(&db_path.join("spelling.json")).ok();
        let entity_graph: Option<EntityGraph> = load_json(&db_path.join("entity_graph.json")).ok();

        let mut tagger = None;
        if let Some(ref tag_dict) = state.tag_dict_path {
            let p = Path::new(tag_dict);
            if p.exists() {
                tagger = load_tagger_csv(p).ok();
            }
        }

        Ok(Self {
            state: Some(state),
            bm25,
            spelling,
            entity_graph,
            tagger,
            cache_dir: Some(db_path.to_path_buf()),
        })
    }

    pub fn from_parts(bm25: Bm25Index) -> Self {
        Self {
            state: None,
            bm25,
            spelling: None,
            entity_graph: None,
            tagger: None,
            cache_dir: None,
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
    pub skg_seeds: Vec<String>,
    pub skg_neighbors: Vec<(String, f64)>,
    pub warnings: Vec<String>,
}

pub fn save_json<T: Serialize>(path: &Path, val: &T) -> Result<(), String> {
    let file = File::create(path).map_err(|e| format!("Failed to create file {}: {}", path.display(), e))?;
    let writer = io::BufWriter::new(file);
    serde_json::to_writer_pretty(writer, val).map_err(|e| format!("Failed to write JSON to {}: {}", path.display(), e))?;
    Ok(())
}

pub fn load_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let file = File::open(path).map_err(|e| format!("Failed to open file {}: {}", path.display(), e))?;
    let reader = io::BufReader::new(file);
    let val = serde_json::from_reader(reader).map_err(|e| format!("Failed to parse JSON from {}: {}", path.display(), e))?;
    Ok(val)
}

pub fn load_tagger_csv(path: &Path) -> io::Result<Tagger> {
    let kind = path.file_stem().and_then(|s| s.to_str()).unwrap_or("entity").to_string();
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

        let mut entry = Entry::new(phrase, kind.clone(), format!("csv-{}", i))
            .with_regex(is_regex_val);
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
        let clean: String = word.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase();
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
    let q_tokens = filter_query_stopwords(tokenize(query));
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

pub fn search(index: &LoadedIndex, query: &str, opts: &SearchOptions) -> Result<SearchResults, String> {
    let mut warnings = Vec::new();

    // 1. Spell correction
    let (corrected_query_opt, effective_query) = if opts.spell_check {
        if let Some(ref spelling) = index.spelling {
            let corrected = correct_query(spelling, query);
            if corrected != query {
                (Some(corrected.clone()), corrected)
            } else {
                (None, query.to_string())
            }
        } else {
            (None, query.to_string())
        }
    } else {
        (None, query.to_string())
    };

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
            let walk = crate::graph_search::compute_skg_scores(&index.bm25, graph, &effective_query, &skg_params);
            let label = |k: &str| index.bm25.entity_labels.get(k).cloned().unwrap_or_else(|| k.to_string());
            skg_seeds = walk.seeds.iter().map(|s| label(s)).collect();
            skg_neighbors = walk.expanded.iter().map(|(k, w)| (label(k), *w)).collect();
            skg_scores = walk.scores;
        } else if let Some(ref db_path) = index.cache_dir {
            let graph_path = db_path.join("entity_graph.json");
            if !graph_path.exists() {
                warnings.push("\x1B[35m[SKG] No entity_graph.json found; graph boost disabled (re-run `lume index`).\x1B[0m".to_string());
            } else {
                warnings.push("\x1B[35m[SKG] Failed to load entity_graph.json; graph boost disabled.\x1B[0m".to_string());
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
        let token_opt = opts.auth_token.clone().or_else(crate::hybrid::load_nuts_token);
        let has_session = index.state.as_ref().and_then(|s| s.semantic_session_id.as_ref()).is_some();

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
            let target_dir = index.state.as_ref().map(|s| s.target_dir.as_str()).unwrap_or("");
            let hybrid_res = crate::hybrid::execute_hybrid_search(
                &index.bm25,
                index.tagger.as_ref(),
                target_dir,
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
                    h_results.hits.truncate(opts.limit);
                    let mut hits = Vec::new();
                    for h in h_results.hits {
                        let snippet = best_snippet_with_cap(&h.body, &effective_query, opts.max_snippet_chars);
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
    let (lexical_params, lexical_variant) = match opts.mode {
        SearchMode::HybridOrFallback => (Bm25Params::default(), SearchVariant::Classic),
        _ => (opts.bm25_params.clone(), opts.bm25_variant),
    };
    let mut bm25_hits = index.bm25.search(&effective_query, lexical_variant, &lexical_params, index.tagger.as_ref());
    crate::graph_search::apply_skg_boost(&mut bm25_hits, &skg_scores, beta);
    bm25_hits.truncate(opts.limit);

    let mut hits = Vec::new();
    for (i, hit) in bm25_hits.iter().enumerate() {
        if let Some(sec) = index.bm25.sections.get(hit.section_index) {
            let filename = sec.filename.clone();
            let skg_score = skg_scores.get(&hit.section_index).copied();
            let snippet = best_snippet_with_cap(&sec.body, &effective_query, opts.max_snippet_chars);
            let entities = sec.entities.iter()
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
    let target_dir = index.state.as_ref().map(|s| s.target_dir.as_str()).unwrap_or("unknown");
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
            stdout.push_str(&format!("[SKG walk] seeds: {} → neighbors: {}\n", seeds_str, neighbors_str));
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
        stdout.push_str(&format!("Executing lexical BM25 search (graph={})...\n", results.graph_beta));
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
                    hit.rank,
                    hit.score,
                    skg_tag,
                    hit.title,
                    filename,
                    hit.line_number
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
    fn test_lexical_only_with_nonexistent_target_dir() {
        let bm25 = build_test_bm25();
        let state = IndexState {
            target_dir: "/nonexistent/directory/that/does/not/exist/987654321".to_string(),
            db_dir: ".dummy-db".to_string(),
            semantic_enabled: false,
            ollama_entities: false,
            ollama_model: "test".to_string(),
            ollama_url: "http://localhost:11434".to_string(),
            tag_dict_path: None,
            semantic_session_id: None,
            cached_files: HashMap::new(),
        };
        let index = LoadedIndex {
            state: Some(state),
            bm25,
            spelling: None,
            entity_graph: None,
            tagger: None,
            cache_dir: None,
        };

        let opts = SearchOptions {
            mode: SearchMode::LexicalOnly,
            graph_beta: 0.0,
            ..Default::default()
        };

        let results = search(&index, "captain", &opts).expect("Lexical search should succeed without reading target_dir");
        assert_eq!(results.executed_mode, SearchMode::LexicalOnly);
        assert_eq!(results.hits.len(), 1);
        assert_eq!(results.hits[0].title, "Chapter 1");
    }

    #[test]
    fn test_hybrid_fallback_on_missing_session() {
        let bm25 = build_test_bm25();
        let state = IndexState {
            target_dir: "/dummy/target".to_string(),
            db_dir: ".dummy-db".to_string(),
            semantic_enabled: false,
            ollama_entities: false,
            ollama_model: "test".to_string(),
            ollama_url: "http://localhost:11434".to_string(),
            tag_dict_path: None,
            semantic_session_id: None,
            cached_files: HashMap::new(),
        };
        let index = LoadedIndex {
            state: Some(state),
            bm25,
            spelling: None,
            entity_graph: None,
            tagger: None,
            cache_dir: None,
        };

        let opts = SearchOptions {
            mode: SearchMode::HybridOrFallback,
            alpha: 0.5,
            graph_beta: 0.0,
            ..Default::default()
        };

        let results = search(&index, "captain", &opts).expect("Search should fall back to lexical");
        assert_eq!(results.executed_mode, SearchMode::LexicalOnly);
        assert!(results.warnings.iter().any(|w| w.contains("Semantic search unavailable")));
        assert_eq!(results.hits.len(), 1);
        assert_eq!(results.hits[0].title, "Chapter 1");
    }

    #[test]
    fn test_hybrid_strict_fails_on_missing_session() {
        let bm25 = build_test_bm25();
        let state = IndexState {
            target_dir: "/dummy/target".to_string(),
            db_dir: ".dummy-db".to_string(),
            semantic_enabled: false,
            ollama_entities: false,
            ollama_model: "test".to_string(),
            ollama_url: "http://localhost:11434".to_string(),
            tag_dict_path: None,
            semantic_session_id: None,
            cached_files: HashMap::new(),
        };
        let index = LoadedIndex {
            state: Some(state),
            bm25,
            spelling: None,
            entity_graph: None,
            tagger: None,
            cache_dir: None,
        };

        let opts = SearchOptions {
            mode: SearchMode::HybridStrict,
            alpha: 0.5,
            graph_beta: 0.0,
            ..Default::default()
        };

        let res = search(&index, "captain", &opts);
        assert!(res.is_err(), "HybridStrict must error when semantic session is missing");
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
        let results = search(&index, "treasure", &opts).expect("Lexical search from parts should succeed");
        assert_eq!(results.hits.len(), 1);
        assert_eq!(results.hits[0].title, "Chapter 2");
    }
}
