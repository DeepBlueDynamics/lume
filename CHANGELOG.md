# Changelog

## 0.13.1 — 2026-10-10

### Performance
- **SIMD MiniRoaring kernels with runtime dispatch** (#15). Bitmap containers use AVX2 on x86_64 when the CPU supports it, with a scalar fallback. On aarch64, NEON is used for popcount and fused AND+popcount, and AND/OR/ANDNOT stay scalar because the compiler already vectorizes them there. All `unsafe` code is confined to `src/fast_retrieval/simd.rs`, and differential tests check it against the scalar reference at every container edge size.
  - x86_64 microbench (rustc 1.96, 10,000 × 8 KiB containers): AND 2.36×, popcount 2.32×, AND+popcount 3.18×.
  - Hot TREC-COVID, x86_64 (same setup as 0.13.0): p50 6.27 → **5.88 ms** (−6%) and p99 11.77 → **11.23 ms** (−5%). Throughput is unchanged within noise, and SciFact is flat. Rankings are byte-identical.
  - Raspberry Pi 5 (Cortex-A76): popcount 1.20× and AND+popcount 1.32×; AND, OR and ANDNOT stay at scalar speed by design. SciFact rankings are byte-identical on the Pi (300 queries).

### Docs
- The README Performance section now includes the v0.13.0 results (search speed, hybrid on identical vectors, facets, index build).
- `docs/HALOS.md` now points at the v0.13.0 HaLOS package. For 0.13.1, change the version in its download lines.

## 0.13.0 — 2026-10-10

### Performance highlights
Hot-server numbers come from a separate driver container, with the engine capped at 8 CPU / 8 GiB, built with rustc 1.96 release (thin LTO). Search results are byte-identical to the previous build on SciFact and TREC-COVID unless a row says otherwise.

| Default profile (stemming + coordination floor 1.0) | before¹ | 0.13.0 |
|---|---:|---:|
| TREC-COVID hot query p50 | 111.6 ms | **6.27 ms** (≈18× faster) |
| TREC-COVID hot query p99 | 231.0 ms | **11.77 ms** |
| TREC-COVID throughput (8 concurrent) | 60 QPS | **609 QPS** |
| SciFact hot query p50 | 9.05 ms | **2.19 ms** |
| TREC-COVID fresh index build (171k files) | 90.4 s² | **56.0 s** (−38%), peak RSS 2.70 → 2.43 GB |

¹ The 0.12.x search path with the resident index reused across requests. In 0.12.2 and earlier, every request also reloaded the index, which made TREC-COVID p50 7,892 ms.
² The pre-0.13 index build path, on the same corpus and machine.

Hybrid quality on SciFact with **identical EmbeddingGemma 2 vectors** for both engines, nDCG@10: Lume `LUME_BLEND=normalized-v2` scores **0.853**, Luxir hybrid (RRF) 0.779, and dense-only 0.845. α was tuned on SciFact. On **held-out NFCorpus** with α fixed in advance: Lume 0.373 against Luxir 0.370, which is a tie.

### Search speed
- **Exact MaxScore pruning, integer term ids and a bounded top-k heap**: these cut the BM25 hot path from 111.6 to 6.77 ms p50 on TREC-COVID, with rankings byte-identical at every step (#12).
- **Candidate/allow bitmap API** (`candidates()`, `search_top_k_filtered()`): exhaustive candidate sets and filter-before-scoring, used by facets and NOT (#12).
- Plain searches skip candidate collection and facet work entirely when no facets are requested (#13).

### Facets and typed metadata (#13, docs/FACETS.md)
- **Metadata input**: a `lume.meta.jsonl` manifest, or a restricted YAML frontmatter subset that is blanked in place so line numbers are preserved. Types are keyword, keyword_list, integer, float and date. An optional `lume.schema.json` overrides the types.
- **Filters**: `field:value`, `-field:value`, and numeric or date ranges, applied *before* scoring.
- **Facets**: `--facet` counts are taken over the **full** candidate set using bitmap popcount, so they don't depend on `-l`. Buckets are ordered by count descending, then value ascending.
- **SQL**: metadata fields appear as `sections` columns with filter pushdown. `GROUP BY` returns the same buckets as native facets.
- **Index format**: `format_version` 3 is written only when metadata exists, together with `meta.json`. Plain indexes stay at version 2 (stemmed) or 1.

### Hybrid search (#14, docs/HYBRID.md)
- **Resident local vectors**: `lume index --embed-model … --embed-dimensions …` stores section vectors next to the index. Vectors can come from Shivvr `/embed` (with explicit `document`/`query` tasks) or be imported without any service calls via `--embed-docs` and `--embed-queries`. Model, dimensions, IDs, coverage and vector values are validated on import. Search uses exact cosine scoring.
- **Opt-in fusion modes**: `LUME_BLEND=rrf` (`LUME_RRF_K`, default 60), `normalized-v2` (min-max scaling of both BM25 and cosine) and `vector`. `LUME_LOCAL_VECTOR_DEPTH` takes a count or `all` (default 100). Existing defaults are unchanged.
- **No per-query corpus walk**: hybrid caches are fingerprinted from the loaded index snapshot instead of stat-ing every corpus file on every query. That walk had cost about 7 s per query on a 5k-file corpus on a network filesystem.
- **Explicit Shivvr URL**: the configured URL now reaches ingest, session, query, inversion and cleanup calls. Caches record the server URL and refuse a mismatch.
- `/embed` input is bounded to 256 texts per request and 32 KiB per text. Oversize input is rejected before any network call.

### Indexing (#16)
- **One BM25 build per index**: ordinary indexing used to re-tokenize everything and rebuild BM25 at each of four periodic flushes. It now builds and publishes once, giving the 38% faster TREC-COVID build above with an identical stored index. Periodic searchable flushes remain only for slow Ollama entity extraction.
- **Scan checkpoints**: an interrupted scan writes `index-scan-checkpoint.json`. The previously published index stays loadable and unchanged, and resume restores frontmatter.
- **`LUME_TIMING=1`**: opt-in JSON timing records on stderr for each index and cold-open phase (walk, read, parse, tokenize, BM25, spelling, serialize, write, sync, and per-file parse). It's silent when unset, and search output is unchanged.

### Search & ranking
- **Boolean NOT support (`-term` and `NOT term`)**:
  - `lume search` now supports term exclusions via `-term` and `NOT term` (e.g. `cancer -therapy` or `cancer NOT therapy`).
  - Excluded terms' posting lists are subtracted from the candidate set via `MiniRoaring::andnot` before candidate pruning and BM25 scoring; excluded terms do not contribute to scores.
  - Excluded terms undergo the identical tokenization, stemming, and stopword pipeline as positive terms; NOT on a stopword is safely ignored with a diagnostic notice.
  - Hyphenated words within a term (e.g. `covid-19`) are treated as positive terms, not NOT.
  - Queries with only NOT terms return empty results with a clear notice.
- **SQL `AND NOT match()` pushdown (`lume sql`)**:
  - `lume sql` supports exact pushdown for negated full-text filters (`WHERE match(...) AND NOT match(...)` and standalone `WHERE NOT match(...)`).
  - Negated matches exclude matching section rows without rescoring surviving rows; standalone `NOT match()` returns non-matching rows with `score IS NULL`.
  - Non-pushed down filters (e.g. under `OR`) fail cleanly with a clear top-level `AND` requirement error.
- **Default stemming and coordination floor 1.0**: Ranking change: stemming + no coordination penalty by default; reindex to benefit. Note: stemmed indexes write index format_version 2 in `state.json`; older lume binaries (v0.12.3 or earlier) ignore this setting and search stemmed indexes with unstemmed queries, so upgrade lume or reindex.
  - Stemming (Snowball English) is now enabled by default for new indexes (`LUME_STEM=0` opts out). Existing indexes preserve their unstemmed setting in `state.json` (format version 1); searching an unstemmed index prints a notice suggesting `lume index -f` to benefit.
  - The default coordination factor floor is now 1.0 (disabling coordination down-weighting penalties; configurable via `LUME_COORD_FLOOR`).
  - Unknown future index format versions (`format_version > 2`) are refused with a clear "rebuild with a newer lume or reindex" message.
  - Benchmarked across SciFact (nDCG@10 0.645 → 0.677, R_cap@100 0.873 → 0.906, MRR@10 0.608 → 0.644), TREC-COVID (nDCG@10 0.561 → 0.632, R_cap@100 0.442 → 0.485, MRR@10 0.842 → 0.911), and held-out NFCorpus (nDCG@10 0.296 → 0.314, R_cap@100 0.236 → 0.251, MRR@10 0.515 → 0.528).
  - Legacy `keep_hyphens=true` indexes are rejected with a clear reindex error; hyphen-internal token preservation flag dropped from index construction.

### OTLP receiver & agent telemetry (Lume TI)
- **Built-in OTLP HTTP/JSON receiver** (`POST /v1/metrics` and `POST /v1/logs`, D50):
  ingests agent telemetry (token usage, active time, tool executions, file edits)
  into a dedicated store (`<root>/stores/agents`), registered as SQL table
  `telemetry_agents` for metrics and `docs` for logs.
  - Standalone daemon: `lume ti otlp --store <root> [--bind 127.0.0.1] [--port 4318] [--otlp-token-file <path>]`.
  - Integrated HTTP server: `lume serve --ti-store <root> --otlp` and
    `lume ti ingest ... --serve --otlp` serve OTLP endpoints on the shared HTTP port (default 5863).
  - Protobuf requests are rejected with HTTP 415 ("lume accepts OTLP http/json only; set protocol json");
    requests exceeding 8 MiB return HTTP 413.
  - Bearer token authentication is supported via `--otlp-token-file`; loopback binds allow
    unauthenticated requests while non-loopback binds strictly require a token.
- **Monotonic counter persistence (A4)**: cumulative and delta monotonic counter running totals
  are maintained in memory and persisted across receiver restarts in `<root>/stores/agents/otlp-counters.json`.
- **Dimensional paths**: counter metrics with attributes are indexed as dimensional columns
  (`<name>.<type>.model.<model>@last`), enabling windowed usage queries via `max(...) - min(...)`.
- **Log records in `docs`**: OTLP logs are ingested into the shared document catalog as `kind = 'logbook'`,
  keyed by `agent.urn:<instance_id>` with event name in `title` and attributes in `body`, searchable
  via lexical `match(body, '...')`.

### PostgreSQL & Grafana
- **PostgreSQL wire protocol (pgwire)**: `lume serve --pg <port>` and `lume ti ingest ... --pg <port>`
  expose read-only PostgreSQL protocol access (default port 5864) for psql and Grafana.
  Supports SCRAM-SHA-256 verifier authentication via `--pg-auth-config <path>`, with optional TLS
  (`--pg-tls-cert`, `--pg-tls-key`, `--pg-require-tls`).
- **Grafana agent telemetry dashboard**: added `bench/grafana/lume-agents-dashboard.json`, providing:
  - Tokens Over Time: `MAX - MIN` token usage over the query window using D50 dimensional paths (`claude_code.token.usage@last`).
  - Active Time: mean active execution time from `claude_code.active_time`.
  - Active Buckets per Agent: 10-second active bucket counts grouped by agent entity (`vessel AS entity`, `count(DISTINCT ts)`).
  - Recent Logbook Docs: table of recent OTLP log events (`title`, `vessel AS entity`, `ts_start`) from `docs`
    with safe SQL-string escaping via `${q:sqlstring}`.
- **Dashboard test harness**: `bench/grafana/test_agents_dashboard.py` (structure, datasource, secret scan)
  and `bench/grafana/test_agents_dashboard_sql.py` (live query execution against an OTLP-ingested instance with macro expansion).

### Signal K plugin & Ask tab
- **Cloud-direct Ask tab (B1)**: the Ask tab in `plugins/signalk-lume-ti` defaults to `https://ollama.com`
  with model `glm-5.3:cloud`. Added `chatApiKeyFile` setting: the plugin reads the API key from a private
  file (mode 0600) and passes `OLLAMA_API_KEY` in the child environment only (never on argv or in logs),
  freeing ~4.2 GB of disk on edge devices by avoiding a local model container.
- **Plugin OTLP receiver setting (B9)**: added `otlpEnabled` and `otlpTokenFile` settings to the plugin schema.
  The supervisor passes `--otlp` (and `--otlp-token-file <path>` if configured) to the child process.
  The query server remains bound to loopback `127.0.0.1` (ignoring any configured `serveBind`) to prevent
  unauthenticated network exposure.

### Deployment & provisioning
- **Idempotent Pi provisioning script (`scripts/provision-pi.sh`, B7)**: provisions a fresh or reflashed
  Raspberry Pi running HaLOS:
  - Reports system version, disk usage, and memory.
  - Verifies and configures memory cgroups (`cgroup_enable=memory cgroup_memory=1` in `/boot/firmware/cmdline.txt`) with backup.
  - Reports pinned self vessel UUID from Signal K server settings.
  - Installs HaLOS `.deb` packages (skipping Ollama by default).
  - Deploys plugin and arm64 binary via `scripts/deploy-pi.sh`.
  - Displays SETUP §13 API key setup instructions.
- **Pi plugin deploy script (`scripts/deploy-pi.sh`, B4)**: bundles arm64 `lume` binary at mode 755 and
  plugin files (excluding `node_modules` and `test`), transfers to `/var/lib/container-apps/.../signalk-lume-ti`,
  restarts the service via `sudo -n systemctl`, and checks health.
- **Ollama retirement script (`scripts/pi-retire-ollama.sh`, B4)**: stops and disables `marine-ollama-container`,
  removes the 4.2 GB Docker image, and preserves persistent models/data.
- **SSH isolation**: scripts support `LUME_DEPLOY_SSH_CONFIG` to isolate SSH configuration without modifying
  the host's `~/.ssh/config`.

### Self-telemetry
- **Lume operational telemetry (`telemetry_lume`)**: Lume ingest service records internal performance counters,
  process RSS, CPU usage, lag, and flush cost every 10 seconds under `<store>/stores/lume`.
  Auto-registered in the SQL engine as `telemetry_lume` (entity `lume.urn:host:<hostname>`).
  Maintains an independent bucketer and watermark to ensure vessel bucket boundaries are never affected.
  Disabled with `LUME_TI_SELF_TELEMETRY=0` (or `off`, `false`).

### HaLOS container applications
- **Offline container packages**: `deploy/halos/` packages `marine-grubcrawler-container` (v0.16.1-1, offline
  cruiser library fetcher) and `marine-ollama-container` (v0.1.0-1, optional cloud gateway).
- **Auto memory limits**: `app-prestart.sh` dynamically sizes container memory limits based on available system
  RAM (`MEMORY_LIMIT=auto`: Grub up to 20% / max 4 GiB; Ollama 12% / ~1 GiB on Pi 5).
- **Debian package builder**: `scripts/build-halos-debs.sh` builds both arm64 `.deb` packages into `dist/halos/`
  with Maintainer `DeepBlue Dynamics <kord@deepbluedynamics.com>`.

### CI & automated testing
- **Deploy script integration tests (B8)**: `scripts/test/deploy-pi.test.sh` tests `deploy-pi.sh`, `pi-retire-ollama.sh`,
  and `provision-pi.sh` against a throwaway Debian sshd container with mock `systemctl`, `docker`, `apt-get`, and `dpkg` shims.
- **CI workflow (`.github/workflows/ci.yml`)**: added automated `deploy-test` job running ShellCheck on all deploy
  scripts and running `deploy-pi.test.sh`.

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

### Agentic answering
- **`lume answer` + viz "Ask" mode**: an agentic plan → retrieve → evaluate →
  refine → answer loop over a local Ollama model (default gpt-4o-mini). The model
  plans search queries, the field is retrieved and animated, the model judges
  whether the passages suffice and refines the queries if not (up to N rounds),
  then synthesizes a **cited** answer. Streams NDJSON events (`question`, `plan`,
  `evaluate`, relaxation frames, `answer`) through the same bridge. In the viz,
  the answer panel shows the per-round plan log and the answer; nodes fed to the
  model get a soft halo, the ones it **cited** glow, and clicking a source chip
  highlights its orb. New `src/answer.rs`.

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
