# Design: in-process Lume search API

Author: n8-keen-kiwi (read-only review, 2026-10-06). Spot-checked by the lead
against `src/main.rs` and `src/hybrid.rs` at 867c8db. Status: **proposal, not approved.**
Unblocks W5 (`match()`) and W7 (`ti_resolve`). See [repo-fit §3](../repo-fit.md).

## Current pipeline (`handle_search`, src/main.rs:1295)

1. CLI parse (1313–1347): `-c`, `--db` (default `.lume-index`), `-l` (10), `-a` (env `ALPHA` or 0.5), `-g`, `--scoring`, query.
2. FS check + `lume::hybrid::set_cache_dir(db_path)` (1357), which sets the global `CACHE_DIR` mutex.
3. Load `state.json` → `IndexState`, `bm25.json` → `Bm25Index`, `spelling.json` → `SpellIndex`; optional `Tagger` via `load_tagger_csv` (703).
4. Spell correction: `correct_query` (1273) → `SpellIndex::correct_word`.
5. SKG walk: beta = `-g` or env `GRAPH_ALPHA` or 0.4; `compute_skg_for_search` (1925) loads `entity_graph.json`, calls `graph_search::compute_skg_scores`, **prints** the walk.
6. Hybrid branch (1394–1426) if session + token + alpha > 0: **`env::set_var("ALPHA")`** (1406), then `hybrid::execute_hybrid_search` (hybrid.rs:859). That function reads the token, **corpus file metadata** (873), semantic cache, env BM25 params (`VARIANT`, `K1`, `B`, `DELTA`, `TITLE_WEIGHT`, `BODY_WEIGHT`), calls Shivvr, BM25, and `blend_hybrid_scores` (env `LUME_BLEND_NORM`). On error it falls through to step 7.
7. Lexical branch (1428–1443): **hardcoded** `Bm25Params::default()` + `SearchVariant::Classic`, `apply_skg_boost`, truncate, print.
8. Snippets: `best_snippet` (1981) from both printers.

Implicit inputs: env `ALPHA`, `GRAPH_ALPHA`, `SHIVVR_BASE_URL`, `NUTS_SERVICES_TOKEN` (+ `.env`),
`LUME_BLEND_NORM`, `VARIANT`, `K1`, `B`, `DELTA`, `TITLE_WEIGHT`, `BODY_WEIGHT`,
`LUME_QUERY_INVERSION`, `DATA`; global `CACHE_DIR`; files `state.json`, `bm25.json`,
`spelling.json`, `entity_graph.json`, tag dict CSV, semantic cache/session JSON, and
the raw corpus dir (metadata only).

## Proposed API (`src/search.rs`, `pub mod search`)

```rust
pub enum SearchMode { LexicalOnly, HybridOrFallback, HybridStrict }
pub enum BlendMode  { Multiplicative, Normalized }

pub struct SearchOptions {
    pub limit: usize, pub spell_check: bool, pub mode: SearchMode,
    pub alpha: f32, pub graph_beta: f64, pub use_relatedness: bool,
    pub bm25_params: Bm25Params, pub bm25_variant: SearchVariant,
    pub blend_mode: BlendMode, pub shivvr_url: Option<String>,
    pub auth_token: Option<String>, pub query_inversion: bool,
    pub max_snippet_chars: usize,
}

pub struct LoadedIndex {
    pub state: Option<IndexState>, pub bm25: Bm25Index,
    pub spelling: Option<SpellIndex>, pub entity_graph: Option<EntityGraph>,
    pub tagger: Option<Tagger>, pub cache_dir: Option<PathBuf>,
}
impl LoadedIndex {
    pub fn open(db_dir: impl AsRef<Path>) -> Result<Self, String>;
    pub fn from_parts(bm25: Bm25Index) -> Self;
}

pub struct SearchResults {
    pub query: String, pub corrected_query: Option<String>,
    pub executed_mode: SearchMode, pub hits: Vec<SearchResultHit>,
    pub total_sections: usize, pub skg_seeds: Vec<String>,
    pub skg_neighbors: Vec<(String, f64)>, pub warnings: Vec<String>,
}

pub fn search(index: &LoadedIndex, query: &str, opts: &SearchOptions) -> Result<SearchResults, String>;
pub fn format_cli_output(results: &SearchResults, index: &LoadedIndex, db_dir: &str) -> (String, String);
```

`SearchMode::LexicalOnly` is the boat path: no network, no token, no corpus-dir access.

## Extraction plan

1. New `src/search.rs`. Move `IndexState`, `compute_skg_for_search`, `print_skg_walk`, `best_snippet`, `correct_query` and `load_tagger_csv` out of `main.rs`. Implement `LoadedIndex`, `search` and `format_cli_output`.
2. `handle_search` becomes a thin adapter. It fills `SearchOptions` from CLI flags, falling back to the same env vars so defaults don't change.
3. `agent.rs` `lume_search` calls `search::search` in-process, replacing `run_lume_cli`.
4. Parity: capture `lume search` stdout and stderr on a fixed index before the change and diff after. Both diffs must be empty.

## Risks (non-mechanical)

- `env::set_var("ALPHA")` (main.rs:1406) and global `CACHE_DIR` are unsound for a multithreaded server. Pass both explicitly.
- The hybrid and lexical paths use different BM25 params (env vs hardcoded defaults). The CLI adapter must reproduce today's split exactly, so output stays byte-identical. Unifying the two paths is a separate, visible change.
- `execute_hybrid_search` needs the raw corpus dir (hybrid.rs:873). That is absent on a boat or shore node, which is a second reason `LexicalOnly` must skip it.
- ANSI-coloured progress goes straight to stderr (main.rs:1938, 1944). Collect it into `warnings`/the walk instead and let the caller print.
