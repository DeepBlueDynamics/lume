use crate::fast_retrieval::{MiniRoaring, PrimeFilter};
use crate::tokenize_with_options;
use crate::Tagger;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub fn serialize_u8_map<S, T>(map: &HashMap<Vec<u8>, T>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
    T: Serialize,
{
    use serde::ser::SerializeMap;
    let mut map_ser = serializer.serialize_map(Some(map.len()))?;
    for (k, v) in map {
        let key_str = String::from_utf8_lossy(k).into_owned();
        map_ser.serialize_entry(&key_str, v)?;
    }
    map_ser.end()
}

pub fn deserialize_u8_map<'de, D, T>(deserializer: D) -> Result<HashMap<Vec<u8>, T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    let string_map: HashMap<String, T> = HashMap::deserialize(deserializer)?;
    let mut u8_map = HashMap::with_capacity(string_map.len());
    for (k, v) in string_map {
        u8_map.insert(k.into_bytes(), v);
    }
    Ok(u8_map)
}

pub fn serialize_vec_u8_map<S>(
    vec: &[HashMap<Vec<u8>, usize>],
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::ser::SerializeSeq;
    let mut seq = serializer.serialize_seq(Some(vec.len()))?;
    for map in vec {
        let mut string_map = HashMap::with_capacity(map.len());
        for (k, v) in map {
            string_map.insert(String::from_utf8_lossy(k).into_owned(), *v);
        }
        seq.serialize_element(&string_map)?;
    }
    seq.end()
}

pub fn deserialize_vec_u8_map<'de, D>(
    deserializer: D,
) -> Result<Vec<HashMap<Vec<u8>, usize>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let vec_string_maps: Vec<HashMap<String, usize>> = Vec::deserialize(deserializer)?;
    let mut vec_u8_maps = Vec::with_capacity(vec_string_maps.len());
    for map in vec_string_maps {
        let mut u8_map = HashMap::with_capacity(map.len());
        for (k, v) in map {
            u8_map.insert(k.into_bytes(), v);
        }
        vec_u8_maps.push(u8_map);
    }
    Ok(vec_u8_maps)
}

/// Minimum coordination multiplier. A document matching none of the distinct
/// query terms beyond candidacy keeps this fraction of its score; matching all
/// of them keeps the full score. Keeps single-term matches viable while
/// rewarding multi-term coverage. Default is 1.0 (disabling coordination penalty).
const COORD_FLOOR: f64 = 1.0;

/// Returns the effective coordination floor multiplier, configurable via
/// the `LUME_COORD_FLOOR` environment variable. Defaults to `1.0`.
/// Setting `LUME_COORD_FLOOR=0.5` restores the legacy coordination penalty.
pub fn coord_floor() -> f64 {
    std::env::var("LUME_COORD_FLOOR")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(COORD_FLOOR)
}

/// Common English function words and question words that carry little
/// discriminative value for retrieval. Filtered out of the *query* (never the
/// index) so content terms drive ranking. Without this, a query like
/// "how does Dantes know Mercedes" is dominated by "how/does/know", which match
/// unrelated sections (e.g. a chapter titled "How a Gardener...").
pub const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "been", "being", "but", "by", "can", "could", "did",
    "do", "does", "for", "from", "had", "has", "have", "he", "her", "here", "hers", "him", "his",
    "how", "i", "if", "in", "into", "is", "it", "its", "may", "me", "might", "must", "my", "no",
    "nor", "not", "of", "on", "or", "our", "shall", "she", "should", "so", "than", "that", "the",
    "their", "them", "then", "there", "these", "they", "this", "those", "to", "us", "was", "we",
    "were", "what", "when", "where", "which", "while", "who", "whom", "why", "will", "with",
    "would", "you", "your",
];

/// Returns true if the folded token bytes correspond to a stopword.
pub fn is_stopword(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes)
        .map(|s| STOPWORDS.contains(&s))
        .unwrap_or(false)
}

/// Drops stopword tokens from a tokenized query. If every token is a stopword
/// (e.g. the query is literally "how are you"), the original tokens are kept so
/// the search still returns something rather than nothing.
pub fn filter_query_stopwords(tokens: Vec<crate::Token>) -> Vec<crate::Token> {
    let filtered: Vec<crate::Token> = tokens
        .iter()
        .filter(|t| !is_stopword(&t.bytes))
        .cloned()
        .collect();
    if filtered.is_empty() {
        tokens
    } else {
        filtered
    }
}

/// Query parsed into positive terms (for candidate retrieval and BM25 scoring) and
/// excluded NOT terms (subtracted from the candidate set via MiniRoaring::andnot).
///
/// Syntax:
/// - `-term` (prefix hyphen with no whitespace between '-' and term)
/// - `NOT term` (uppercase 'NOT' operator followed by whitespace and term)
/// - Hyphenated words inside a term (e.g. `covid-19`) are treated as positive terms, not NOT.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedQuery {
    pub positive_terms: Vec<String>,
    pub not_terms: Vec<String>,
}

/// Parses a query string into positive terms and excluded NOT terms.
pub fn parse_query(query: &str) -> ParsedQuery {
    let mut positive_terms = Vec::new();
    let mut not_terms = Vec::new();
    let mut next_is_not = false;

    for word in query.split_whitespace() {
        if word == "NOT" {
            next_is_not = true;
            continue;
        }

        if next_is_not {
            next_is_not = false;
            let term = word.trim_start_matches('-');
            if !term.is_empty() {
                not_terms.push(term.to_string());
            }
            continue;
        }

        if word.starts_with('-') && word.len() > 1 {
            let term = word.trim_start_matches('-');
            if !term.is_empty() {
                not_terms.push(term.to_string());
                continue;
            }
        }

        // Lone "-" or "--" dashes are ignored as punctuation.
        if word == "-" || word == "--" {
            continue;
        }

        positive_terms.push(word.to_string());
    }

    ParsedQuery {
        positive_terms,
        not_terms,
    }
}

/// Represents a section parsed from a Markdown document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Section {
    pub title: String,
    pub body: String,
    pub line_number: usize,
    pub filename: Option<String>,
    #[serde(default)]
    pub entities: Vec<String>,
}

/// The three BM25 variants supported by the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SearchVariant {
    Classic,
    Plus,
    L,
}

/// Tuning parameters and field weights for the field-aware BM25 engine.
#[derive(Debug, Clone)]
pub struct Bm25Params {
    pub k1: f64,
    pub b: f64,
    pub delta: f64, // Used for BM25+
    pub title_weight: f64,
    pub body_weight: f64,
    pub coord_floor: f64,
}

impl Default for Bm25Params {
    fn default() -> Self {
        Self {
            k1: 1.2,
            b: 0.75,
            delta: 1.0,
            title_weight: 2.0,
            body_weight: 1.0,
            coord_floor: 1.0,
        }
    }
}

impl SearchVariant {
    pub fn from_env() -> Self {
        match std::env::var("VARIANT").as_deref() {
            Ok("plus") => SearchVariant::Plus,
            Ok("l") => SearchVariant::L,
            _ => SearchVariant::Classic,
        }
    }
}

impl Bm25Params {
    pub fn from_env() -> Self {
        Self {
            k1: std::env::var("K1")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(1.2),
            b: std::env::var("B")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.75),
            delta: std::env::var("DELTA")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(1.0),
            title_weight: std::env::var("TITLE_WEIGHT")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(2.0),
            body_weight: std::env::var("BODY_WEIGHT")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(1.0),
            coord_floor: coord_floor(),
        }
    }
}

/// Options controlling token processing during index construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bm25BuildOptions {
    pub stemmed: bool,
    pub keep_hyphens: bool,
}

impl Default for Bm25BuildOptions {
    fn default() -> Self {
        Self {
            stemmed: true,
            keep_hyphens: false,
        }
    }
}

impl Bm25BuildOptions {
    pub fn from_env() -> Self {
        Self {
            stemmed: std::env::var("LUME_STEM")
                .map(|v| v != "0" && v.to_lowercase() != "false")
                .unwrap_or(true),
            keep_hyphens: false,
        }
    }
}

/// A parsed, in-memory index of Markdown sections.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bm25Index {
    pub sections: Vec<Section>,
    pub num_docs: usize,

    // Per-document term frequency maps for each field (indexed by token bytes)
    #[serde(
        serialize_with = "serialize_vec_u8_map",
        deserialize_with = "deserialize_vec_u8_map"
    )]
    pub title_tfs: Vec<HashMap<Vec<u8>, usize>>,
    #[serde(
        serialize_with = "serialize_vec_u8_map",
        deserialize_with = "deserialize_vec_u8_map"
    )]
    pub body_tfs: Vec<HashMap<Vec<u8>, usize>>,

    // Total token counts per document field
    pub title_lens: Vec<usize>,
    pub body_lens: Vec<usize>,

    // Average field lengths across the corpus
    pub avg_title_len: f64,
    pub avg_body_len: f64,

    // Corpus-wide document frequencies: token bytes -> number of docs containing it
    #[serde(
        serialize_with = "serialize_u8_map",
        deserialize_with = "deserialize_u8_map"
    )]
    pub title_dfs: HashMap<Vec<u8>, usize>,
    #[serde(
        serialize_with = "serialize_u8_map",
        deserialize_with = "deserialize_u8_map"
    )]
    pub body_dfs: HashMap<Vec<u8>, usize>,

    // Native roaring bitmaps and prime/Gödel partitioned signature filters
    #[serde(
        serialize_with = "serialize_u8_map",
        deserialize_with = "deserialize_u8_map"
    )]
    pub posting_lists: HashMap<Vec<u8>, MiniRoaring>,
    pub prime_filters: Vec<PrimeFilter>,
    pub tag_prime_map: HashMap<String, u128>,

    // Entity information for Semantic Mesh (Option A)
    pub entity_posting_lists: HashMap<String, MiniRoaring>,
    pub entity_kinds: HashMap<String, String>,
    pub entity_labels: HashMap<String, String>,

    #[serde(default)]
    pub stemmed: bool,
    #[serde(default)]
    pub keep_hyphens: bool,

    // Derived from the saved byte-keyed maps once; never changes the JSON format.
    #[serde(skip)]
    interned: std::sync::OnceLock<std::sync::Arc<InternedIndex>>,
}

#[derive(Debug, Clone, Copy)]
struct TermPosting {
    doc: u32,
    title_tf: usize,
    body_tf: usize,
}

#[derive(Debug)]
struct InternedIndex {
    vocabulary: HashMap<Vec<u8>, u32>,
    postings: Vec<Vec<TermPosting>>,
}

impl InternedIndex {
    fn build(index: &Bm25Index) -> Self {
        let mut result = Self {
            vocabulary: HashMap::new(),
            postings: Vec::new(),
        };
        for doc in 0..index.num_docs {
            let title = &index.title_tfs[doc];
            let body = &index.body_tfs[doc];
            for (bytes, &title_tf) in title {
                result.add(
                    bytes,
                    TermPosting {
                        doc: doc as u32,
                        title_tf,
                        body_tf: body.get(bytes).copied().unwrap_or(0),
                    },
                );
            }
            for (bytes, &body_tf) in body {
                if !title.contains_key(bytes) {
                    result.add(
                        bytes,
                        TermPosting {
                            doc: doc as u32,
                            title_tf: 0,
                            body_tf,
                        },
                    );
                }
            }
        }
        result
    }

    fn add(&mut self, bytes: &[u8], posting: TermPosting) {
        let id = *self.vocabulary.entry(bytes.to_vec()).or_insert_with(|| {
            let id = u32::try_from(self.postings.len()).expect("index vocabulary exceeds u32");
            self.postings.push(Vec::new());
            id
        });
        self.postings[id as usize].push(posting);
    }
}

/// A hit returned by the search query.
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub section_index: usize,
    pub score: f64,
}

/// Represents the reason why a candidate section was rejected during ranking.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RejectReason {
    MissingSection,
    TagSignatureMismatch,
    NoTokenMatch,
    ScoreBelowThreshold(f64),
    EmptyText,
    FieldNotRankable,
}

/// Diagnostic information for a ranked candidate.
#[derive(Debug, Clone)]
pub struct RankDebug {
    pub section_id: u32,
    pub score: Option<f64>,
    pub rejected: Option<RejectReason>,
}

/// Simple, robust line-by-line Markdown section parser.
/// Cuts sections at `#` headers and records their starting line numbers.
pub fn parse_markdown_with_options(content: &str, use_fallback: bool) -> Vec<Section> {
    let has_header = content.lines().any(|line| {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            let hashes_count = trimmed.chars().take_while(|&c| c == '#').count();
            hashes_count > 0 && !trimmed[hashes_count..].trim().is_empty()
        } else {
            false
        }
    });

    let mut sections = Vec::new();
    let mut current_title = String::from("Introduction");
    let mut current_body = Vec::new();
    let mut start_line = 1;
    let mut fallback_title_found = false;

    for (i, line) in content.lines().enumerate() {
        let line_num = i + 1;
        let trimmed = line.trim();

        if use_fallback && !has_header && !fallback_title_found {
            if !trimmed.is_empty() {
                current_title = trimmed.to_string();
                start_line = line_num;
                fallback_title_found = true;
                continue;
            }
            continue;
        }

        if trimmed.starts_with('#') {
            let hashes_count = trimmed.chars().take_while(|&c| c == '#').count();
            let header_text = trimmed[hashes_count..].trim().to_string();

            if hashes_count > 0 && !header_text.is_empty() {
                // Save previous section if it has any content
                let body_text = current_body.join("\n");
                sections.push(Section {
                    title: current_title,
                    body: body_text,
                    line_number: start_line,
                    filename: None,
                    entities: Vec::new(),
                });

                current_title = header_text;
                current_body.clear();
                start_line = line_num;
                continue;
            }
        }
        current_body.push(line.to_string());
    }

    // Push the final section
    let body_text = current_body.join("\n");
    sections.push(Section {
        title: current_title,
        body: body_text,
        line_number: start_line,
        filename: None,
        entities: Vec::new(),
    });

    // Retain sections that aren't completely blank or table of contents artifacts
    sections.retain(|s| !s.title.trim().is_empty() && s.body.trim().len() > 100);
    sections
}

/// Simple, robust line-by-line Markdown section parser.
/// Cuts sections at `#` headers and records their starting line numbers.
/// Honors `LUME_TITLE_FALLBACK=1` in the environment.
pub fn parse_markdown(content: &str) -> Vec<Section> {
    let use_fallback = std::env::var("LUME_TITLE_FALLBACK")
        .map(|v| v == "1")
        .unwrap_or(false);
    parse_markdown_with_options(content, use_fallback)
}

impl Bm25Index {
    /// Constructs a search index over a collection of Markdown sections.
    /// Constructs a search index over a collection of Markdown sections, reading options from environment.
    pub fn build(sections: Vec<Section>, tagger: Option<&Tagger>) -> Self {
        Self::build_with_options(sections, tagger, Bm25BuildOptions::from_env())
    }

    /// Constructs a search index over a collection of Markdown sections with explicit options.
    pub fn build_with_options(
        sections: Vec<Section>,
        tagger: Option<&Tagger>,
        options: Bm25BuildOptions,
    ) -> Self {
        let mut tag_prime_map = HashMap::new();
        if let Some(t) = tagger {
            let mut unique_tags = std::collections::BTreeSet::new();
            for sec in &sections {
                for tag in t.tag(&sec.title) {
                    unique_tags.insert(tag.output.clone());
                }
                for tag in t.tag(&sec.body) {
                    unique_tags.insert(tag.output.clone());
                }
            }
            for (idx, tag_out) in unique_tags.into_iter().enumerate() {
                let prime = crate::fast_retrieval::get_nth_prime(idx + 1);
                tag_prime_map.insert(tag_out, prime);
            }
        }

        let num_docs = sections.len();
        let mut title_tfs = Vec::with_capacity(num_docs);
        let mut body_tfs = Vec::with_capacity(num_docs);
        let mut title_lens = Vec::with_capacity(num_docs);
        let mut body_lens = Vec::with_capacity(num_docs);

        let mut title_dfs = HashMap::new();
        let mut body_dfs = HashMap::new();

        let mut total_title_len = 0;
        let mut total_body_len = 0;

        let mut posting_lists: HashMap<Vec<u8>, MiniRoaring> = HashMap::new();
        let mut prime_filters = Vec::with_capacity(num_docs);

        let mut entity_posting_lists: HashMap<String, MiniRoaring> = HashMap::new();
        let mut entity_kinds = HashMap::new();
        let mut entity_labels = HashMap::new();
        let stem = options.stemmed;

        for (doc_idx, sec) in sections.iter().enumerate() {
            let doc_id = doc_idx as u32;
            let t_toks = tokenize_with_options(&sec.title, stem, false);
            let b_toks = tokenize_with_options(&sec.body, stem, false);

            title_lens.push(t_toks.len());
            body_lens.push(b_toks.len());
            total_title_len += t_toks.len();
            total_body_len += b_toks.len();

            // Build Title TF
            let mut t_tf = HashMap::new();
            for tok in &t_toks {
                *t_tf.entry(tok.bytes.clone()).or_insert(0) += 1;
                posting_lists
                    .entry(tok.bytes.clone())
                    .or_default()
                    .insert(doc_id);
            }
            for tok_bytes in t_tf.keys() {
                *title_dfs.entry(tok_bytes.clone()).or_insert(0) += 1;
            }
            title_tfs.push(t_tf);

            // Build Body TF
            let mut b_tf = HashMap::new();
            for tok in &b_toks {
                *b_tf.entry(tok.bytes.clone()).or_insert(0) += 1;
                posting_lists
                    .entry(tok.bytes.clone())
                    .or_default()
                    .insert(doc_id);
            }
            for tok_bytes in b_tf.keys() {
                *body_dfs.entry(tok_bytes.clone()).or_insert(0) += 1;
            }
            body_tfs.push(b_tf);

            // Compute PrimeFilter signatures
            let mut pf = PrimeFilter::new();
            for tok in &t_toks {
                pf.add_term(&tok.bytes);
            }
            for tok in &b_toks {
                pf.add_term(&tok.bytes);
            }

            if let Some(t) = tagger {
                let title_tags = t.tag(&sec.title);
                for tag in title_tags {
                    if let Some(&prime) = tag_prime_map.get(&tag.output) {
                        pf.add_tag_prime(prime);
                    }

                    // Track for semantic mesh (Option A)
                    entity_posting_lists
                        .entry(tag.output.clone())
                        .or_default()
                        .insert(doc_id);
                    entity_kinds.insert(tag.output.clone(), tag.kind.clone());

                    // Keep the best version of the surface label (longer / capitalized)
                    let entry = entity_labels.entry(tag.output.clone());
                    match entry {
                        std::collections::hash_map::Entry::Vacant(v) => {
                            v.insert(tag.surface.clone());
                        }
                        std::collections::hash_map::Entry::Occupied(mut o) => {
                            let curr = o.get();
                            let is_better =
                                (tag.surface.chars().next().is_some_and(|c| c.is_uppercase())
                                    && !curr.chars().next().is_some_and(|c| c.is_uppercase()))
                                    || tag.surface.len() > curr.len();
                            if is_better {
                                o.insert(tag.surface.clone());
                            }
                        }
                    }
                }
                let body_tags = t.tag(&sec.body);
                for tag in body_tags {
                    if let Some(&prime) = tag_prime_map.get(&tag.output) {
                        pf.add_tag_prime(prime);
                    }

                    // Track for semantic mesh (Option A)
                    entity_posting_lists
                        .entry(tag.output.clone())
                        .or_default()
                        .insert(doc_id);
                    entity_kinds.insert(tag.output.clone(), tag.kind.clone());

                    // Keep the best version of the surface label (longer / capitalized)
                    let entry = entity_labels.entry(tag.output.clone());
                    match entry {
                        std::collections::hash_map::Entry::Vacant(v) => {
                            v.insert(tag.surface.clone());
                        }
                        std::collections::hash_map::Entry::Occupied(mut o) => {
                            let curr = o.get();
                            let is_better =
                                (tag.surface.chars().next().is_some_and(|c| c.is_uppercase())
                                    && !curr.chars().next().is_some_and(|c| c.is_uppercase()))
                                    || tag.surface.len() > curr.len();
                            if is_better {
                                o.insert(tag.surface.clone());
                            }
                        }
                    }
                }
            }
            for ent in &sec.entities {
                let ent_key = ent.trim().to_lowercase();
                if !ent_key.is_empty() && ent_key != "__lume_processed__" {
                    entity_posting_lists
                        .entry(ent_key.clone())
                        .or_default()
                        .insert(doc_id);
                    entity_kinds
                        .entry(ent_key.clone())
                        .or_insert_with(|| "ollama".to_string());
                    entity_labels
                        .entry(ent_key.clone())
                        .or_insert_with(|| ent.clone());
                }
            }
            prime_filters.push(pf);
        }

        let avg_title_len = if num_docs > 0 {
            total_title_len as f64 / num_docs as f64
        } else {
            0.0
        };

        let avg_body_len = if num_docs > 0 {
            total_body_len as f64 / num_docs as f64
        } else {
            0.0
        };

        Self {
            sections,
            num_docs,
            title_tfs,
            body_tfs,
            title_lens,
            body_lens,
            avg_title_len,
            avg_body_len,
            title_dfs,
            body_dfs,
            posting_lists,
            prime_filters,
            tag_prime_map,
            entity_posting_lists,
            entity_kinds,
            entity_labels,
            stemmed: stem,
            keep_hyphens: false,
            interned: std::sync::OnceLock::new(),
        }
    }

    /// Evaluates a query and returns matching sections ordered by their BM25 score.
    /// Prints pruning and rejection diagnostics to stderr (the CLI's behaviour).
    pub fn search(
        &self,
        query: &str,
        variant: SearchVariant,
        params: &Bm25Params,
        tagger: Option<&Tagger>,
    ) -> Vec<SearchHit> {
        self.search_impl(query, variant, params, tagger, true)
    }

    /// `search` without stderr diagnostics, for in-process callers (Lume TI `match()`).
    pub fn search_quiet(
        &self,
        query: &str,
        variant: SearchVariant,
        params: &Bm25Params,
        tagger: Option<&Tagger>,
    ) -> Vec<SearchHit> {
        self.search_impl(query, variant, params, tagger, false)
    }

    fn search_impl(
        &self,
        query: &str,
        variant: SearchVariant,
        params: &Bm25Params,
        tagger: Option<&Tagger>,
        verbose: bool,
    ) -> Vec<SearchHit> {
        macro_rules! diag {
            ($($arg:tt)*) => {
                if verbose {
                    eprintln!($($arg)*);
                }
            };
        }
        let parsed = parse_query(query);

        // Positive query tokens for candidate retrieval and BM25 scoring.
        // If there are no NOT terms, tokenizing the raw query preserves exact byte-identical
        // behavior with previous versions.
        let query_tokens = if parsed.not_terms.is_empty() {
            filter_query_stopwords(tokenize_with_options(query, self.stemmed, false))
        } else {
            let pos_query = parsed.positive_terms.join(" ");
            filter_query_stopwords(tokenize_with_options(&pos_query, self.stemmed, false))
        };

        if query_tokens.is_empty() || self.num_docs == 0 {
            if !parsed.not_terms.is_empty() {
                diag!("[NOT] Query contains only excluded terms; returning 0 results");
            }
            return Vec::new();
        }

        // Excluded tokens: go through the same tokenize, stem and stopword path as normal terms.
        // A NOT on a stopword is ignored with a diagnostic note.
        let mut effective_not_tokens = Vec::new();
        for not_term in &parsed.not_terms {
            let toks = tokenize_with_options(not_term, self.stemmed, false);
            for tok in toks {
                if is_stopword(&tok.bytes) {
                    diag!(
                        "[NOT] Excluded term '{}' is a stopword and will be ignored",
                        String::from_utf8_lossy(&tok.bytes)
                    );
                } else {
                    effective_not_tokens.push(tok);
                }
            }
        }

        let start_pruning = std::time::Instant::now();

        // 1. Gather all candidates using union of query term roaring bitmaps
        let mut candidate_set = MiniRoaring::new();
        let mut first = true;
        for q_tok in &query_tokens {
            if let Some(list) = self.posting_lists.get(&q_tok.bytes) {
                if first {
                    candidate_set = list.clone();
                    first = false;
                } else {
                    candidate_set = candidate_set.union(list);
                }
            }
        }

        // 1b. Exclude any sections containing excluded terms using MiniRoaring::andnot
        for not_tok in &effective_not_tokens {
            if let Some(list) = self.posting_lists.get(&not_tok.bytes) {
                candidate_set = candidate_set.andnot(list);
            }
        }

        let candidate_ids = candidate_set.iter();
        let num_candidates_roaring = candidate_ids.len();

        // 2. Further prune using Gödel tag signatures if query has tagged entities
        let mut query_tag_primes = Vec::new();
        if let Some(t) = tagger {
            let tag_query = if parsed.not_terms.is_empty() {
                query.to_string()
            } else {
                parsed.positive_terms.join(" ")
            };
            let query_tags = t.tag(&tag_query);
            for tag in &query_tags {
                if let Some(&prime) = self.tag_prime_map.get(&tag.output) {
                    query_tag_primes.push(prime);
                } else {
                    let dummy_prime =
                        crate::fast_retrieval::get_nth_prime(self.tag_prime_map.len() + 2);
                    query_tag_primes.push(dummy_prime);
                }
            }
        }

        let mut rejected_missing = 0;
        let mut rejected_empty = 0;
        let mut rejected_tag_mismatch = 0;
        let mut rejected_no_token = 0;
        let rejected_below_threshold = 0;
        let mut rejected_not_rankable = 0;

        let mut candidate_details = Vec::with_capacity(num_candidates_roaring);
        let mut pruned_candidates = Vec::with_capacity(num_candidates_roaring);

        for doc_id in candidate_ids {
            let doc_idx = doc_id as usize;
            if doc_idx >= self.sections.len() {
                rejected_missing += 1;
                candidate_details.push(RankDebug {
                    section_id: doc_id,
                    score: None,
                    rejected: Some(RejectReason::MissingSection),
                });
                continue;
            }

            let sec = &self.sections[doc_idx];
            if sec.title.is_empty() && sec.body.is_empty() {
                rejected_empty += 1;
                candidate_details.push(RankDebug {
                    section_id: doc_id,
                    score: None,
                    rejected: Some(RejectReason::EmptyText),
                });
                continue;
            }

            if self.title_lens[doc_idx] == 0 && self.body_lens[doc_idx] == 0 {
                rejected_not_rankable += 1;
                candidate_details.push(RankDebug {
                    section_id: doc_id,
                    score: None,
                    rejected: Some(RejectReason::FieldNotRankable),
                });
                continue;
            }

            let pf = &self.prime_filters[doc_idx];

            // Tag signature verification: Candidate must contain all query tag outputs if present
            let mut tag_match = true;
            for &prime in &query_tag_primes {
                if !pf.test_tag_prime(prime) {
                    tag_match = false;
                    break;
                }
            }
            if !tag_match {
                rejected_tag_mismatch += 1;
                candidate_details.push(RankDebug {
                    section_id: doc_id,
                    score: None,
                    rejected: Some(RejectReason::TagSignatureMismatch),
                });
                continue;
            }

            pruned_candidates.push(doc_id);
        }

        let pruning_elapsed = start_pruning.elapsed();
        diag!(
            "\x1B[32m[Two-Stage Pruning] Pruned candidate space from {} to {} (roaring generated: {}) sections in {:.2?}\x1B[0m",
            self.num_docs, pruned_candidates.len(), num_candidates_roaring, pruning_elapsed
        );

        // Stage 2: Heavy Scoring on active candidates only
        let mut hits = Vec::new();

        // Distinct query terms drive the coordination factor below: a document
        // that matches more of the distinct query terms is more relevant than
        // one that matches a single term many times. Without this, small chunks
        // that repeat a common term (e.g. "Dantès") outrank chunks that contain
        // the rarer, more discriminative term the user actually cares about.
        let distinct_query_terms: std::collections::HashSet<&[u8]> =
            query_tokens.iter().map(|t| t.bytes.as_slice()).collect();
        let num_distinct = distinct_query_terms.len().max(1);

        let interned = self
            .interned
            .get_or_init(|| std::sync::Arc::new(InternedIndex::build(self)));
        let mut totals = vec![0.0; self.num_docs];
        let mut matched_counts = vec![0usize; self.num_docs];
        let mut seen_terms = std::collections::HashSet::new();
        // Visit tokens in their original order. This keeps every f64 addition,
        // including repeated query terms, identical to document-at-a-time scoring.
        for token in &query_tokens {
            let Some(&term_id) = interned.vocabulary.get(token.bytes.as_slice()) else {
                continue;
            };
            let first_occurrence = seen_terms.insert(term_id);
            let mut signature = PrimeFilter::new();
            signature.add_term(&token.bytes);
            let title_df = self.title_dfs.get(&token.bytes).copied().unwrap_or(0) as f64;
            let body_df = self.body_dfs.get(&token.bytes).copied().unwrap_or(0) as f64;
            let title_idf = ((self.num_docs as f64 - title_df + 0.5) / (title_df + 0.5) + 1.0)
                .ln()
                .max(0.0);
            let body_idf = ((self.num_docs as f64 - body_df + 0.5) / (body_df + 0.5) + 1.0)
                .ln()
                .max(0.0);
            for posting in &interned.postings[term_id as usize] {
                let doc = posting.doc as usize;
                if self.prime_filters[doc].term_mask & signature.term_mask != signature.term_mask {
                    continue;
                }
                let title_score = calculate_bm25_term_score(
                    posting.title_tf as f64,
                    self.title_lens[doc] as f64,
                    self.avg_title_len,
                    title_idf,
                    variant,
                    params,
                );
                let body_score = calculate_bm25_term_score(
                    posting.body_tf as f64,
                    self.body_lens[doc] as f64,
                    self.avg_body_len,
                    body_idf,
                    variant,
                    params,
                );
                totals[doc] += params.title_weight * title_score + params.body_weight * body_score;
                if first_occurrence {
                    matched_counts[doc] += 1;
                }
            }
        }

        for doc_id in pruned_candidates {
            let doc_idx = doc_id as usize;
            let mut total_score = totals[doc_idx];

            // Coordination factor: softly down-weight documents that match only
            // a fraction of the distinct query terms. coverage=1.0 (all terms
            // present) leaves the score untouched; a single-term match out of
            // three terms keeps ~2/3 of its score. For single-term queries this
            // is always 1.0, so ordinary lookups are unaffected.
            let coverage = matched_counts[doc_idx] as f64 / num_distinct as f64;
            let floor = params.coord_floor;
            let coord = floor + (1.0 - floor) * coverage;
            total_score *= coord;

            if total_score > 0.0 {
                hits.push(SearchHit {
                    section_index: doc_idx,
                    score: total_score,
                });
                candidate_details.push(RankDebug {
                    section_id: doc_id,
                    score: Some(total_score),
                    rejected: None,
                });
            } else {
                rejected_no_token += 1;
                candidate_details.push(RankDebug {
                    section_id: doc_id,
                    score: Some(0.0),
                    rejected: Some(RejectReason::NoTokenMatch),
                });
            }
        }

        // Print high-level Rejection Accounting summary to stderr
        diag!("\x1B[33mCandidates: {}\x1B[0m", num_candidates_roaring);
        diag!("\x1B[33mRanked: {}\x1B[0m", hits.len());
        diag!("\x1B[33mRejected:\x1B[0m");
        diag!("  MissingSection: {}", rejected_missing);
        diag!("  EmptyText: {}", rejected_empty);
        diag!("  FieldNotRankable: {}", rejected_not_rankable);
        diag!("  TagSignatureMismatch: {}", rejected_tag_mismatch);
        diag!("  NoTokenMatch: {}", rejected_no_token);
        diag!("  ScoreBelowThreshold: {}", rejected_below_threshold);

        // Trigger deep diagnostic explanation if hits is empty but we had candidates
        if hits.is_empty() && num_candidates_roaring > 0 {
            diag!("\n\x1B[1;31m🔍 [Deep Rejection Diagnostics] Why zero ranked results?\x1B[0m");
            for detail in &candidate_details {
                if let Some(reason) = detail.rejected {
                    let doc_id = detail.section_id;
                    diag!(
                        "  \x1B[1;33mCandidate {} rejected:\x1B[0m {:?}",
                        doc_id,
                        reason
                    );

                    let doc_idx = doc_id as usize;
                    if doc_idx < self.sections.len() {
                        let sec = &self.sections[doc_idx];
                        diag!("     - Header: {:?}", sec.title);
                        diag!(
                            "     - Body Snippet: {:?}",
                            diagnostic_body_preview(&sec.body)
                        );

                        let title_tokens =
                            tokenize_with_options(&sec.title, self.stemmed, self.keep_hyphens);
                        let body_tokens =
                            tokenize_with_options(&sec.body, self.stemmed, self.keep_hyphens);

                        let title_terms: Vec<String> = title_tokens
                            .iter()
                            .map(|t| String::from_utf8_lossy(&t.bytes).to_string())
                            .collect();
                        let body_terms: Vec<String> = body_tokens
                            .iter()
                            .map(|t| String::from_utf8_lossy(&t.bytes).to_string())
                            .collect();

                        diag!("     - Title Tokens: {:?}", title_terms);
                        diag!("     - Body Tokens: {:?}", body_terms);

                        let pf = &self.prime_filters[doc_idx];

                        diag!("     - Token-by-Token Query Evaluation:");
                        for q_tok in &query_tokens {
                            let term_str = String::from_utf8_lossy(&q_tok.bytes);
                            let prime_match = pf.test_term(&q_tok.bytes);

                            let title_tf = self.title_tfs[doc_idx]
                                .get(&q_tok.bytes)
                                .copied()
                                .unwrap_or(0);
                            let body_tf = self.body_tfs[doc_idx]
                                .get(&q_tok.bytes)
                                .copied()
                                .unwrap_or(0);

                            diag!(
                                "       * Term '{}' -> Prime Filter Match: {} | Title TF: {} | Body TF: {}",
                                term_str, prime_match, title_tf, body_tf
                            );
                        }
                    }
                }
            }
            diag!();
        }

        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        hits
    }

    #[cfg(test)]
    fn search_reference(
        &self,
        query: &str,
        variant: SearchVariant,
        params: &Bm25Params,
        tagger: Option<&Tagger>,
        verbose: bool,
    ) -> Vec<SearchHit> {
        macro_rules! diag {
            ($($arg:tt)*) => {
                if verbose {
                    eprintln!($($arg)*);
                }
            };
        }
        let query_tokens = filter_query_stopwords(tokenize_with_options(
            query,
            self.stemmed,
            self.keep_hyphens,
        ));
        if query_tokens.is_empty() || self.num_docs == 0 {
            return Vec::new();
        }

        let start_pruning = std::time::Instant::now();

        // 1. Gather all candidates using union of query term roaring bitmaps
        let mut candidate_set = MiniRoaring::new();
        let mut first = true;
        for q_tok in &query_tokens {
            if let Some(list) = self.posting_lists.get(&q_tok.bytes) {
                if first {
                    candidate_set = list.clone();
                    first = false;
                } else {
                    candidate_set = candidate_set.union(list);
                }
            }
        }

        let candidate_ids = candidate_set.iter();
        let num_candidates_roaring = candidate_ids.len();

        // 2. Further prune using Gödel tag signatures if query has tagged entities
        let mut query_tag_primes = Vec::new();
        if let Some(t) = tagger {
            let query_tags = t.tag(query);
            for tag in &query_tags {
                if let Some(&prime) = self.tag_prime_map.get(&tag.output) {
                    query_tag_primes.push(prime);
                } else {
                    let dummy_prime =
                        crate::fast_retrieval::get_nth_prime(self.tag_prime_map.len() + 2);
                    query_tag_primes.push(dummy_prime);
                }
            }
        }

        let mut rejected_missing = 0;
        let mut rejected_empty = 0;
        let mut rejected_tag_mismatch = 0;
        let mut rejected_no_token = 0;
        let rejected_below_threshold = 0;
        let mut rejected_not_rankable = 0;

        let mut candidate_details = Vec::with_capacity(num_candidates_roaring);
        let mut pruned_candidates = Vec::with_capacity(num_candidates_roaring);

        for doc_id in candidate_ids {
            let doc_idx = doc_id as usize;
            if doc_idx >= self.sections.len() {
                rejected_missing += 1;
                candidate_details.push(RankDebug {
                    section_id: doc_id,
                    score: None,
                    rejected: Some(RejectReason::MissingSection),
                });
                continue;
            }

            let sec = &self.sections[doc_idx];
            if sec.title.is_empty() && sec.body.is_empty() {
                rejected_empty += 1;
                candidate_details.push(RankDebug {
                    section_id: doc_id,
                    score: None,
                    rejected: Some(RejectReason::EmptyText),
                });
                continue;
            }

            if self.title_lens[doc_idx] == 0 && self.body_lens[doc_idx] == 0 {
                rejected_not_rankable += 1;
                candidate_details.push(RankDebug {
                    section_id: doc_id,
                    score: None,
                    rejected: Some(RejectReason::FieldNotRankable),
                });
                continue;
            }

            let pf = &self.prime_filters[doc_idx];

            // Tag signature verification: Candidate must contain all query tag outputs if present
            let mut tag_match = true;
            for &prime in &query_tag_primes {
                if !pf.test_tag_prime(prime) {
                    tag_match = false;
                    break;
                }
            }
            if !tag_match {
                rejected_tag_mismatch += 1;
                candidate_details.push(RankDebug {
                    section_id: doc_id,
                    score: None,
                    rejected: Some(RejectReason::TagSignatureMismatch),
                });
                continue;
            }

            pruned_candidates.push(doc_id);
        }

        let pruning_elapsed = start_pruning.elapsed();
        diag!(
            "\x1B[32m[Two-Stage Pruning] Pruned candidate space from {} to {} (roaring generated: {}) sections in {:.2?}\x1B[0m",
            self.num_docs, pruned_candidates.len(), num_candidates_roaring, pruning_elapsed
        );

        // Stage 2: Heavy Scoring on active candidates only
        let mut hits = Vec::new();

        // Distinct query terms drive the coordination factor below: a document
        // that matches more of the distinct query terms is more relevant than
        // one that matches a single term many times. Without this, small chunks
        // that repeat a common term (e.g. "Dantès") outrank chunks that contain
        // the rarer, more discriminative term the user actually cares about.
        let distinct_query_terms: std::collections::HashSet<&[u8]> =
            query_tokens.iter().map(|t| t.bytes.as_slice()).collect();
        let num_distinct = distinct_query_terms.len().max(1);

        for doc_id in pruned_candidates {
            let doc_idx = doc_id as usize;
            let mut total_score = 0.0;
            let mut matched_terms: std::collections::HashSet<&[u8]> =
                std::collections::HashSet::new();
            let pf = &self.prime_filters[doc_idx];

            for q_tok in &query_tokens {
                let tok_bytes = &q_tok.bytes;
                if pf.test_term(tok_bytes)
                    && (self.title_tfs[doc_idx].contains_key(tok_bytes)
                        || self.body_tfs[doc_idx].contains_key(tok_bytes))
                {
                    matched_terms.insert(tok_bytes.as_slice());
                }

                // 1. Title Contribution
                let title_score = {
                    // Check prime filter first for fast signature membership test
                    if pf.test_term(tok_bytes) {
                        let tf =
                            self.title_tfs[doc_idx].get(tok_bytes).copied().unwrap_or(0) as f64;
                        if tf > 0.0 {
                            let df = self.title_dfs.get(tok_bytes).copied().unwrap_or(0);

                            let idf = ((self.num_docs as f64 - df as f64 + 0.5)
                                / (df as f64 + 0.5)
                                + 1.0)
                                .ln();
                            let idf = idf.max(0.0);

                            let doc_len = self.title_lens[doc_idx] as f64;
                            let avgdl = self.avg_title_len;

                            calculate_bm25_term_score(tf, doc_len, avgdl, idf, variant, params)
                        } else {
                            0.0
                        }
                    } else {
                        0.0
                    }
                };

                // 2. Body Contribution
                let body_score = {
                    // Check prime filter first for fast signature membership test
                    if pf.test_term(tok_bytes) {
                        let tf = self.body_tfs[doc_idx].get(tok_bytes).copied().unwrap_or(0) as f64;
                        if tf > 0.0 {
                            let df = self.body_dfs.get(tok_bytes).copied().unwrap_or(0);

                            let idf = ((self.num_docs as f64 - df as f64 + 0.5)
                                / (df as f64 + 0.5)
                                + 1.0)
                                .ln();
                            let idf = idf.max(0.0);

                            let doc_len = self.body_lens[doc_idx] as f64;
                            let avgdl = self.avg_body_len;

                            calculate_bm25_term_score(tf, doc_len, avgdl, idf, variant, params)
                        } else {
                            0.0
                        }
                    } else {
                        0.0
                    }
                };

                total_score += params.title_weight * title_score + params.body_weight * body_score;
            }

            // Coordination factor: softly down-weight documents that match only
            // a fraction of the distinct query terms. coverage=1.0 (all terms
            // present) leaves the score untouched; a single-term match out of
            // three terms keeps ~2/3 of its score. For single-term queries this
            // is always 1.0, so ordinary lookups are unaffected.
            let coverage = matched_terms.len() as f64 / num_distinct as f64;
            let floor = params.coord_floor;
            let coord = floor + (1.0 - floor) * coverage;
            total_score *= coord;

            if total_score > 0.0 {
                hits.push(SearchHit {
                    section_index: doc_idx,
                    score: total_score,
                });
                candidate_details.push(RankDebug {
                    section_id: doc_id,
                    score: Some(total_score),
                    rejected: None,
                });
            } else {
                rejected_no_token += 1;
                candidate_details.push(RankDebug {
                    section_id: doc_id,
                    score: Some(0.0),
                    rejected: Some(RejectReason::NoTokenMatch),
                });
            }
        }

        // Print high-level Rejection Accounting summary to stderr
        diag!("\x1B[33mCandidates: {}\x1B[0m", num_candidates_roaring);
        diag!("\x1B[33mRanked: {}\x1B[0m", hits.len());
        diag!("\x1B[33mRejected:\x1B[0m");
        diag!("  MissingSection: {}", rejected_missing);
        diag!("  EmptyText: {}", rejected_empty);
        diag!("  FieldNotRankable: {}", rejected_not_rankable);
        diag!("  TagSignatureMismatch: {}", rejected_tag_mismatch);
        diag!("  NoTokenMatch: {}", rejected_no_token);
        diag!("  ScoreBelowThreshold: {}", rejected_below_threshold);

        // Trigger deep diagnostic explanation if hits is empty but we had candidates
        if hits.is_empty() && num_candidates_roaring > 0 {
            diag!("\n\x1B[1;31m🔍 [Deep Rejection Diagnostics] Why zero ranked results?\x1B[0m");
            for detail in &candidate_details {
                if let Some(reason) = detail.rejected {
                    let doc_id = detail.section_id;
                    diag!(
                        "  \x1B[1;33mCandidate {} rejected:\x1B[0m {:?}",
                        doc_id,
                        reason
                    );

                    let doc_idx = doc_id as usize;
                    if doc_idx < self.sections.len() {
                        let sec = &self.sections[doc_idx];
                        diag!("     - Header: {:?}", sec.title);
                        diag!(
                            "     - Body Snippet: {:?}",
                            diagnostic_body_preview(&sec.body)
                        );

                        let title_tokens = tokenize_with_options(&sec.title, self.stemmed, false);
                        let body_tokens = tokenize_with_options(&sec.body, self.stemmed, false);

                        let title_terms: Vec<String> = title_tokens
                            .iter()
                            .map(|t| String::from_utf8_lossy(&t.bytes).to_string())
                            .collect();
                        let body_terms: Vec<String> = body_tokens
                            .iter()
                            .map(|t| String::from_utf8_lossy(&t.bytes).to_string())
                            .collect();

                        diag!("     - Title Tokens: {:?}", title_terms);
                        diag!("     - Body Tokens: {:?}", body_terms);

                        let pf = &self.prime_filters[doc_idx];

                        diag!("     - Token-by-Token Query Evaluation:");
                        for q_tok in &query_tokens {
                            let term_str = String::from_utf8_lossy(&q_tok.bytes);
                            let prime_match = pf.test_term(&q_tok.bytes);

                            let title_tf = self.title_tfs[doc_idx]
                                .get(&q_tok.bytes)
                                .copied()
                                .unwrap_or(0);
                            let body_tf = self.body_tfs[doc_idx]
                                .get(&q_tok.bytes)
                                .copied()
                                .unwrap_or(0);

                            diag!(
                                "       * Term '{}' -> Prime Filter Match: {} | Title TF: {} | Body TF: {}",
                                term_str, prime_match, title_tf, body_tf
                            );
                        }
                    }
                }
            }
            diag!();
        }

        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        hits
    }
}

fn diagnostic_body_preview(body: &str) -> String {
    match body.char_indices().nth(100) {
        Some((end, _)) => format!("{}...", &body[..end]),
        None => body.to_string(),
    }
}

/// Helper function to perform BM25 variant score calculation.
fn calculate_bm25_term_score(
    tf: f64,
    doc_len: f64,
    avgdl: f64,
    idf: f64,
    variant: SearchVariant,
    params: &Bm25Params,
) -> f64 {
    if tf == 0.0 {
        return 0.0;
    }

    let k1 = params.k1;
    let b = params.b;

    let len_normalization = if avgdl > 0.0 {
        1.0 - b + b * (doc_len / avgdl)
    } else {
        1.0
    };

    match variant {
        SearchVariant::Classic => idf * (tf * (k1 + 1.0)) / (tf + k1 * len_normalization),
        SearchVariant::Plus => {
            let term_tf_score = (tf * (k1 + 1.0)) / (tf + k1 * len_normalization);
            idf * (term_tf_score + params.delta)
        }
        SearchVariant::L => {
            let scaled_tf = tf / len_normalization;
            idf * (scaled_tf * (k1 + 1.0)) / (scaled_tf + k1)
        }
    }
}

#[cfg(test)]
mod diagnostic_tests {
    use super::*;

    #[test]
    fn rejected_multibyte_body_diagnostics_do_not_panic() {
        let body = format!("{}🚤 bilge pump", "a".repeat(99));
        assert!(body.len() > 100);
        assert!(!body.is_char_boundary(100));
        let index = Bm25Index::build(
            vec![Section {
                title: String::new(),
                body,
                line_number: 1,
                filename: None,
                entities: vec![],
            }],
            None,
        );
        let params = Bm25Params {
            title_weight: 0.0,
            body_weight: 0.0,
            ..Default::default()
        };
        assert!(index
            .search("bilge", SearchVariant::Classic, &params, None)
            .is_empty());
    }
    #[test]
    fn test_parse_markdown_title_fallback() {
        let plain_doc = "A Study on Cellular Apoptosis\n\n\
Apoptosis is a form of programmed cell death that occurs in multicellular organisms. \
Biochemical events lead to characteristic cell changes and death. \
These changes include blebbing, cell shrinkage, nuclear fragmentation, and chromatin condensation.";

        let md_doc = "# A Study on Cellular Apoptosis\n\n\
Apoptosis is a form of programmed cell death that occurs in multicellular organisms. \
Biochemical events lead to characteristic cell changes and death. \
These changes include blebbing, cell shrinkage, nuclear fragmentation, and chromatin condensation.";

        // Default behavior (use_fallback: false): plain text gets "Introduction"
        let default_plain = parse_markdown_with_options(plain_doc, false);
        assert_eq!(default_plain.len(), 1);
        assert_eq!(default_plain[0].title, "Introduction");

        let default_md = parse_markdown_with_options(md_doc, false);
        assert_eq!(default_md.len(), 1);
        assert_eq!(default_md[0].title, "A Study on Cellular Apoptosis");

        // With use_fallback: true:
        let fallback_plain = parse_markdown_with_options(plain_doc, true);
        assert_eq!(fallback_plain.len(), 1);
        assert_eq!(fallback_plain[0].title, "A Study on Cellular Apoptosis");

        // Markdown with headers remains byte-identical
        let fallback_md = parse_markdown_with_options(md_doc, true);
        assert_eq!(fallback_md.len(), default_md.len());
        assert_eq!(fallback_md[0].title, default_md[0].title);
        assert_eq!(fallback_md[0].body, default_md[0].body);
        assert_eq!(fallback_md[0].line_number, default_md[0].line_number);
    }
    #[test]
    fn test_coordination_factor_override() {
        let doc1 = Section {
            title: "Alpha Document".to_string(),
            body: "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma tau upsilon phi chi psi omega".to_string(),
            line_number: 1,
            filename: None,
            entities: vec![],
        };
        let doc2 = Section {
            title: "Beta Document".to_string(),
            body: "beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma tau upsilon phi chi psi omega".to_string(),
            line_number: 2,
            filename: None,
            entities: vec![],
        };
        let index = Bm25Index::build_with_options(
            vec![doc1, doc2],
            None,
            Bm25BuildOptions {
                stemmed: false,
                keep_hyphens: false,
            },
        );
        let default_params = Bm25Params::default();

        // Query with two terms where doc2 only matches one: "alpha beta"
        // Under default coord_floor (1.0), doc2 matches 1/2 distinct terms:
        // coverage = 0.5, coord = 1.0 (no coordination penalty by default).
        let hits_default =
            index.search("alpha beta", SearchVariant::Classic, &default_params, None);
        let hit_doc2_default = hits_default.iter().find(|h| h.section_index == 1).unwrap();
        let default_score = hit_doc2_default.score;

        // With explicit legacy coord_floor = 0.5: coord = 0.5 + 0.5 * 0.5 = 0.75 (penalized)
        let penalized_params = Bm25Params {
            coord_floor: 0.5,
            ..Default::default()
        };
        let hits_penalized = index.search(
            "alpha beta",
            SearchVariant::Classic,
            &penalized_params,
            None,
        );
        let hit_doc2_penalized = hits_penalized
            .iter()
            .find(|h| h.section_index == 1)
            .unwrap();
        let penalized_score = hit_doc2_penalized.score;

        // Default score is unpenalized (penalized_score / 0.75)
        assert!(default_score > penalized_score);
        let expected_ratio = 1.0 / 0.75;
        let actual_ratio = default_score / penalized_score;
        assert!((actual_ratio - expected_ratio).abs() < 1e-4);
    }

    #[test]
    fn test_stemming_tokenize_and_search_agreement() {
        let sec1 = Section {
            title: "Cellular Connections".to_string(),
            body: "The device is connecting to wireless towers successfully.".to_string(),
            line_number: 1,
            filename: None,
            entities: Vec::new(),
        };
        let sec2 = Section {
            title: "Battery Maintenance".to_string(),
            body: "Keep the battery charged overnight for longest longevity.".to_string(),
            line_number: 10,
            filename: None,
            entities: Vec::new(),
        };

        // Case 1: unstemmed index (stemmed: false)
        let unstemmed_index = Bm25Index::build_with_options(
            vec![sec1.clone(), sec2.clone()],
            None,
            Bm25BuildOptions {
                stemmed: false,
                keep_hyphens: false,
            },
        );
        assert!(!unstemmed_index.stemmed);
        let unstemmed_hits = unstemmed_index.search_quiet(
            "connect",
            SearchVariant::Classic,
            &Bm25Params::default(),
            None,
        );
        assert!(
            unstemmed_hits.is_empty(),
            "Unstemmed index should not match inflected forms for 'connect'"
        );

        // Case 2: default index (stemmed: true by default)
        let stemmed_index = Bm25Index::build(vec![sec1, sec2], None);
        assert!(stemmed_index.stemmed);

        // Searching for base form "connect" matches sec1
        let stemmed_hits = stemmed_index.search_quiet(
            "connect",
            SearchVariant::Classic,
            &Bm25Params::default(),
            None,
        );
        assert_eq!(
            stemmed_hits.len(),
            1,
            "Stemmed index should match inflected form via stemmed query"
        );
        assert_eq!(stemmed_hits[0].section_index, 0);

        // Searching for inflected form "connection" also matches
        let stemmed_hits_inflected = stemmed_index.search_quiet(
            "connection",
            SearchVariant::Classic,
            &Bm25Params::default(),
            None,
        );
        assert_eq!(stemmed_hits_inflected.len(), 1);
        assert_eq!(stemmed_hits_inflected[0].section_index, 0);
    }

    #[test]
    fn test_hyphen_joining_behavior() {
        let sec1 = Section {
            title: "Pathogen Identification".to_string(),
            body: "The sample confirmed presence of SARS-CoV-2 in respiratory droplets."
                .to_string(),
            line_number: 1,
            filename: None,
            entities: Vec::new(),
        };
        let sec2 = Section {
            title: "Control Group".to_string(),
            body: "Control subjects showed no viral infection or respiratory symptoms.".to_string(),
            line_number: 10,
            filename: None,
            entities: Vec::new(),
        };

        // Default: hyphens are stripped/joined -> "sarscov2"
        let def_index = Bm25Index::build(vec![sec1, sec2], None);
        assert!(!def_index.keep_hyphens);
        let hits_def = def_index.search_quiet(
            "sarscov2",
            SearchVariant::Classic,
            &Bm25Params::default(),
            None,
        );
        assert_eq!(hits_def.len(), 1);
        assert_eq!(hits_def[0].section_index, 0);
    }

    #[test]
    fn test_parse_query() {
        let q1 = parse_query("vitamin d -deficiency");
        assert_eq!(q1.positive_terms, vec!["vitamin", "d"]);
        assert_eq!(q1.not_terms, vec!["deficiency"]);

        let q2 = parse_query("cancer NOT therapy");
        assert_eq!(q2.positive_terms, vec!["cancer"]);
        assert_eq!(q2.not_terms, vec!["therapy"]);

        // Hyphenated term inside word is NOT excluded
        let q3 = parse_query("covid-19 -vaccine");
        assert_eq!(q3.positive_terms, vec!["covid-19"]);
        assert_eq!(q3.not_terms, vec!["vaccine"]);

        // Lone hyphen ignored
        let q4 = parse_query("cancer - therapy");
        assert_eq!(q4.positive_terms, vec!["cancer", "therapy"]);
        assert!(q4.not_terms.is_empty());

        // Trailing NOT ignored
        let q5 = parse_query("cancer NOT");
        assert_eq!(q5.positive_terms, vec!["cancer"]);
        assert!(q5.not_terms.is_empty());

        // Only NOT terms
        let q6 = parse_query("-therapy -radiation");
        assert!(q6.positive_terms.is_empty());
        assert_eq!(q6.not_terms, vec!["therapy", "radiation"]);

        // Multiple mixed NOT forms
        let q7 = parse_query("cancer NOT therapy -surgery chemo");
        assert_eq!(q7.positive_terms, vec!["cancer", "chemo"]);
        assert_eq!(q7.not_terms, vec!["therapy", "surgery"]);
    }

    #[test]
    fn test_search_not_operator() {
        let sec0 = Section {
            title: "Immunotherapy Study".to_string(),
            body: "Cancer therapy and immunotherapy clinical trial results.".to_string(),
            line_number: 1,
            filename: None,
            entities: Vec::new(),
        };
        let sec1 = Section {
            title: "Genomic Sequencing".to_string(),
            body: "Cancer biology and oncogene mutation profiles without treatment.".to_string(),
            line_number: 10,
            filename: None,
            entities: Vec::new(),
        };
        let sec2 = Section {
            title: "Nutrition Guide".to_string(),
            body: "Cardiovascular health and vitamin d supplementation.".to_string(),
            line_number: 20,
            filename: None,
            entities: Vec::new(),
        };

        let index = Bm25Index::build(vec![sec0, sec1, sec2], None);
        let params = Bm25Params::default();

        // Base search: "cancer" matches sec0 and sec1
        let base_hits = index.search_quiet("cancer", SearchVariant::Classic, &params, None);
        assert_eq!(base_hits.len(), 2);
        let matched_indices: Vec<usize> = base_hits.iter().map(|h| h.section_index).collect();
        assert!(matched_indices.contains(&0));
        assert!(matched_indices.contains(&1));

        // Subtraction with -therapy: sec0 must be excluded
        let minus_hits =
            index.search_quiet("cancer -therapy", SearchVariant::Classic, &params, None);
        assert_eq!(minus_hits.len(), 1);
        assert_eq!(minus_hits[0].section_index, 1);
        assert!(
            !index.sections[minus_hits[0].section_index]
                .body
                .contains("therapy")
        );

        // Subtraction with NOT therapy: sec0 must be excluded
        let not_hits =
            index.search_quiet("cancer NOT therapy", SearchVariant::Classic, &params, None);
        assert_eq!(not_hits.len(), 1);
        assert_eq!(not_hits[0].section_index, 1);

        // Subtraction of absent term: both sec0 and sec1 remain
        let absent_hits =
            index.search_quiet("cancer -radiation", SearchVariant::Classic, &params, None);
        assert_eq!(absent_hits.len(), 2);

        // Subtraction of stopword: ignored with diagnostic note, both remain
        let stopword_hits =
            index.search_quiet("cancer -the", SearchVariant::Classic, &params, None);
        assert_eq!(stopword_hits.len(), 2);

        // Only NOT terms: returns empty results
        let only_not_hits =
            index.search_quiet("-therapy", SearchVariant::Classic, &params, None);
        assert!(only_not_hits.is_empty());
        let only_not_op_hits =
            index.search_quiet("NOT therapy", SearchVariant::Classic, &params, None);
        assert!(only_not_op_hits.is_empty());

        // Byte-identical scores when no NOT terms are present
        let pure_query_hits =
            index.search_quiet("cancer biology", SearchVariant::Classic, &params, None);
        assert_eq!(pure_query_hits.len(), 2);
        assert!(pure_query_hits[0].score > 0.0);
    }
}

#[cfg(test)]
mod integer_scoring_tests {
    use super::*;

    #[test]
    fn integer_postings_preserve_reference_score_bits_and_ties() {
        let words = ["anchor", "bilge", "pump", "wind", "sea", "café", "missing"];
        let sections = (0..96)
            .map(|doc| Section {
                title: words[..doc % 5 + 1].join(" "),
                body: (0..doc % 13 + 1)
                    .map(|i| words[(doc + i) % 6])
                    .collect::<Vec<_>>()
                    .join(" "),
                line_number: doc + 1,
                filename: None,
                entities: Vec::new(),
            })
            .collect();
        let index = Bm25Index::build_with_options(sections, None, Bm25BuildOptions::default());
        for variant in [
            SearchVariant::Classic,
            SearchVariant::Plus,
            SearchVariant::L,
        ] {
            for floor in [0.0, 0.5, 1.0] {
                for query in [
                    "anchor",
                    "pump pump wind",
                    "wind sea café",
                    "missing bilge",
                    "how are you",
                    "no-such-token",
                    "",
                ] {
                    let params = Bm25Params {
                        coord_floor: floor,
                        ..Bm25Params::default()
                    };
                    let expected = index.search_reference(query, variant, &params, None, false);
                    let actual = index.search_quiet(query, variant, &params, None);
                    assert_eq!(actual.len(), expected.len(), "{query}");
                    for (actual, expected) in actual.iter().zip(&expected) {
                        assert_eq!(actual.section_index, expected.section_index, "{query}");
                        assert_eq!(actual.score.to_bits(), expected.score.to_bits(), "{query}");
                    }
                }
            }
        }
    }

    #[test]
    fn derived_integer_index_does_not_change_saved_format() {
        let index = Bm25Index::build_with_options(
            vec![Section {
                title: "Bilge".into(),
                body: "bilge pump pump".into(),
                line_number: 1,
                filename: None,
                entities: Vec::new(),
            }],
            None,
            Bm25BuildOptions::default(),
        );
        let before = serde_json::to_value(&index).unwrap();
        index.search_quiet("pump", SearchVariant::Classic, &Bm25Params::default(), None);
        assert_eq!(serde_json::to_value(&index).unwrap(), before);
        let reopened: Bm25Index = serde_json::from_value(before).unwrap();
        assert!(reopened.interned.get().is_none());
        let hits =
            reopened.search_quiet("pump", SearchVariant::Classic, &Bm25Params::default(), None);
        assert_eq!(hits.len(), 1);
    }
}
