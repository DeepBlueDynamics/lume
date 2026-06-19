# Changelog

## 0.12.0 — 2026-06-19

### Search & ranking
- **SKG significance scoring**: entity-graph edges now carry a `relatedness`
  score alongside Jaccard — a z-score of *observed* vs. *expected* co-occurrence
  (`expected = |A||B|/N`), squashed to `[-1,1]`. Promiscuous hub entities that
  co-occur with everything are damped toward zero; genuine associations rise.
  Computed directly from the roaring-bitmap intersection counts already used for
  Jaccard (no extra scan). New `cooccurrence_relatedness` in `semantic_mesh.rs`.
  The z-score is log-compressed before the tanh bound so strong edges on large
  corpora keep their gradation instead of all saturating at ±1.
- **`--scoring` flag** on `lume search` (and `lume eval`): choose `relatedness`
  (significance, default) or `jaccard` (raw overlap) for the SKG walk. The graph
  walk and edge sort now key on significance by default, Jaccard as tie-breaker.
- `entity_graph.json` export and the ASCII relationship table now include the
  relatedness score.

### Evaluation
- **`lume eval` subcommand**: measure retrieval quality (Hit@k, MRR, nDCG@k)
  against a Q&A file. Relevance is judged by answer-token containment (no human
  labels), so it needs no chunk-id alignment. `--compare` runs both SKG scoring
  modes and prints the delta. New `src/eval.rs` (pure, unit-tested) plus
  `handle_eval` wiring. UTF-8-tolerant Q&A loading (cp1252 files don't abort).

### Visualization
- **`lume stream` + `viz/`**: live 3D visualizer for the search dynamics.
  `lume stream <query>` runs a phase-binding + Weber relaxation over the query's
  top-K candidates (shivvr embeddings, read-only) and emits one NDJSON frame per
  step on stdout — each node's 3D PCA position, velocity, **acceleration**, phase,
  cluster, and **approach-acceleration toward the query** (the `d̈` static cosine
  discards). `viz/` is a Node WebSocket bridge + React/three.js app that renders
  it: candidates as a force field, green/red arrows for accelerating toward/away
  from the query, emergent phase clusters, and a Kuramoto coherence meter. New
  `src/stream.rs`; no new Rust dependencies (std + existing serde_json).

### Tests
- New unit tests for the significance function, hub down-weighting (significance
  flips a ranking Jaccard gets wrong), and the eval metrics/relevance judging.

## 0.11.0 — 2026-06-10

### Indexing
- **Parallel entity extraction**: `-o` extraction now runs across 10 worker threads
  (tunable via `LUME_EXTRACT_WORKERS`), with a single collector thread aggregating
  progress output and checkpointing `state.json` after every completed chunk.
  End-of-run summary reports extracted/cached/failed counts and throughput.
- **Non-UTF-8 tolerance**: indexing no longer aborts on non-UTF-8 files.
  UTF-16 files are decoded (with or without BOM, both endiannesses); other
  encodings are decoded lossily; files that look binary are skipped with a warning.
- **Mid-run searchable flushes**: `bm25.json`/`spelling.json`/`entity_graph.json`
  are rewritten at most every 30 s during long runs, so in-progress indexes can
  already be searched instead of erroring with a missing `bm25.json`.
- **File-level progress**: `[file N/total]` counters on every processed file and
  an indexed/skipped/total summary line.

### Crawler
- Direct-GET fallback converts HTML to clean Markdown (`.md`) instead of saving
  raw `.html` that polluted search snippets; HTML files already in a corpus are
  cleaned at index time.

### Search & ranking
- Entity graph build: Jaccard similarities computed via allocation-free
  intersection counting + inclusion-exclusion, with a cardinality-ratio prune —
  same edges, a fraction of the work.
- Tagger drops fully-identical duplicate emissions (same span/output/kind/id);
  synonym records on a shared span are preserved.
- `lume search` prints which corpus and db it is searching.

### Server & defaults
- MCP serve: concurrent connections capped at 64 (503 on overflow); default
  port is now **5863** ("LUME" on a phone keypad).
- Default entity-extraction model: `gpt-4o-mini:latest`. Agent and summarize
  default to `gemma4:31b-cloud`.
- Shared, cached Ollama endpoint resolution across agent loop, summarize, and
  entity extraction.
- `lume --version` / `lume version`; banner version now tracks `Cargo.toml`.

### PDF extraction
- `lume_extractor.py` repairs pypdf split-word artifacts ("l aw of t he" →
  "law of the") using the document's own vocabulary, and rejoins hyphenated
  line breaks.

## 0.10.0

Baseline: FST tagger, field-aware BM25 with roaring/prime-filter pruning,
trigram spell correction, SKG graph boost, shivvr semantic sessions +
inversion-steered generation, Markov synthesizer, agent loop, MCP server,
HN/Grub crawler.
