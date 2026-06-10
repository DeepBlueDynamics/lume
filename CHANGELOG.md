# Changelog

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
