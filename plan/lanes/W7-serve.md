# W7 — `ti-serve` (weeks 7–9, gates M5)

Depends on: W0 contracts. Wraps a `TiEngine` handle over the other lanes.
Spec: [08-interfaces](../spec/08-interfaces.md), [09-install-fleet](../spec/09-install-fleet.md), [03-single-box-budget](../spec/03-single-box-budget.md).

## Owns

MCP tools, HTTP routes, Postgres wire, CLI, `ti_resolve` over Lume hybrid search,
Signal K plugin, container app, resource limits.

## Tasks

### `TiEngine`
- [ ] Single handle owning the DataFusion session, store, ingest supervisor; every surface is a thin wrapper.

### MCP (added to `lume serve`)
- [ ] `ti_resolve`, `ti_schema`, `ti_sql`, `ti_intervals`, `ti_explain`, `ti_status` with the spec'd inputs/outputs.
- [ ] Caps: 500 rows / 64 KB with "aggregate" hint; units echoed on every result; `@agg` naming mandatory in output.
- [ ] Call `TiEngine` in-process (not the shell-out-to-CLI pattern current tools use).
- [x] `ti_resolve`: lexical Lume BM25 over stored catalog paths + pinned Signal K descriptions; 100-phrase test set (93/100 top-3).

### HTTP (same port)
- [ ] `POST /ti/sql` → Arrow IPC / JSON / CSV by `Accept`.
- [ ] `GET /ti/schema`, `GET /ti/status`, `POST /ti/ingest` (NDJSON → W3).
- [ ] `GET /ti/manifest`, `GET /ti/shards/{vessel}/{shard}/{version}.tar` (for W8).
- [ ] Auth: NUTS on shore; LAN bind + optional bearer token on boat.

### Postgres wire
- [ ] `lume ti serve --pg 5432` via `pgwire`: simple + extended protocol, read-only, SCRAM from `ti.toml`, LAN-only default.
- [ ] `information_schema` + minimal `pg_catalog` views for Grafana and DBeaver.
- [ ] Per-query memory pool: 512 MB Pi 4, 1 GB Pi 5.

### CLI
- [ ] `lume ti init | ingest | backfill | sql | seal | verify | sync | serve` (fits the hand-rolled arg parsing in `src/main.rs`).

### Resource governance (single-box budget)
- [ ] systemd unit / container limits: `MemoryMax=1.5G`, `CPUWeight=50`, nice 10, `ionice -c3` for background.
- [ ] Admission: `target_partitions = 2`, one heavy query at a time, queue, 30 s timeout.
- [ ] Thermal: above 75 °C pause background jobs, query threads → 1.
- [ ] Shutdown: on HALPI shutdown signal, flush WAL (W2 hook).

### Signal K plugin `signalk-lume-ti` (Node)
- [ ] Access-request flow; store device token.
- [ ] Prebuilt static musl `lume` for linux-arm64 and linux-x64 as optional npm deps (release workflow change).
- [ ] Supervise `lume ti ingest` + `lume ti serve`; Admin UI status (lag, WAL, disk).
- [ ] Webapp: SQL console, saved queries + CSV, schema browser; PV-1 energy queries as defaults.
- [ ] Pin-to-chart → Resources API (notes/regions) on explicit user action only.
- [ ] Store-path picker with SD-card warning; Backfill button.

### HaLOS container app
- [ ] `lume-ti-container` via Hat Labs container-packaging-tools; multi-arch OCI (arm64, amd64).
- [ ] Join Signal K container network; Traefik + Authelia routing; Homarr tile; Postgres on LAN 5432.
- [ ] Provision Grafana PostgreSQL data source + starter energy dashboard.
- [ ] Config form with one-time InfluxDB backfill.
- [ ] Submit upstream to halos-marine-containers (or DeepBlue store definition).

## First deliverable
Agent end-to-end: plain-English question → correct rows.

## Gate (M5)
- [x] MCP tools live in `lume serve`; `ti_resolve` top-3 correct for ≥ 90 % of 100 phrases (93/100; resolve slice below)
- [ ] nemesis8 agent with only Lume MCP answers 20 scripted fleet questions, graded against oracle
- [ ] Plugin installs on HALPI2 (HaLOS Marine store) and OpenPlotter (Pi 4 4 GB + Pi 5), gets a token, supervises ingest; psql + Grafana pass a 20-query smoke set

## Stretch (M6+)
- Signal K History API provider (`/signalk/v2/api/history/values`).

## Note
This lane is far larger than the others (Rust server + Node plugin + container packaging + pg wire). Consider splitting into W7a (Rust surfaces) and W7b (packaging/plugin) — see [repo-fit](../repo-fit.md) §6.

## Rust surfaces — first slice (2026-10-06)

Implemented on `ti/w7-surfaces`, scoped to the lead's CLI/MCP assignment:

- [x] `lume ti query <sql> --store <root> [--json]`, `explain`, `status`, and `import-docs <docs_dir>`; existing `verify` and root LumeText injection retained.
- [x] In-process `ti_query`, `ti_schema`, `ti_explain`, `ti_status` registration behind `ti`; optional store argument defaults to `TI_STORE_ROOT` or `./ti`.
- [x] Shared `ti_sql::TiEngine` adapters; query streaming, 500-row / 64 KiB caps, truncation hint, elapsed time, pushdown summary and units. JSON/CSV/Markdown query envelopes are capped including MCP text escaping. Direct mean aliases are emitted with canonical `@mean` names.
- [x] Width derived from persisted RBM headers, with explicit/config/header mismatch rejection; empty stores require `--width <seconds>` or `<store>/ti.toml`.
- [x] Parser, four-tool JSON shape, row/byte caps, UTF-8, canonical units, width conflicts, read-only rejection and idempotent Parquet import tests.
- [x] Real-store CLI smoke: count(*) returned 3,974,400; default cargo build passed. See ti-sql/CHECKS.md for commands and limits.
- [ ] Host fmt/strict clippy: assigned to Pike at merge.

`import-docs` uses W5's existing six-column Parquet reader and stores the document set under `<store>/docs/`. Query replies provide columns, rows, row_count, truncated, elapsed_ms, pushdown, units and hint; rendered formats also provide data. Status provides WAL bytes and open/sealed shard counts; ingest lag and last sync are explicitly null/unavailable until the later ingest-supervisor/sync integration. Sessions are planning snapshots opened for each CLI/MCP operation. This slice does not complete the broader W7 engine supervisor, HTTP, pgwire, resolve, intervals, packaging or M5 gates.

## Rust surfaces — HTTP slice (2026-10-06)

The lead's approved slice uses `POST /ti/query` and `ti_query` naming (rather than the earlier spec's /ti/sql and ti_sql).

- [x] `lume serve --ti-store <root> [--port <port>]` opens one LumeText-backed TiEngine and runtime at startup, shared by Arc across HTTP and the four MCP tools. Without --ti-store, existing serve behavior remains available and /ti returns a disabled response.
- [x] `POST /ti/query {sql, max_rows}` returns Arrow IPC stream format by default; `Accept: application/json` returns the ti_query JSON envelope. Arrow preserves types, canonical @agg names and units metadata. Results are admitted up to 500 rows / 64 KiB; Arrow headers report row count, truncation and the aggregate hint.
- [x] `GET /ti/schema`, `POST /ti/explain {sql}`, `GET /ti/status`; read-only SQL violations return HTTP 400 with a JSON error.
- [x] Queries are serialized through the shared handle; diagnostic buffers are reset between admitted requests. Explicit MCP store/width overrides must match the configured server store.
- [x] Ephemeral-port real-server integration covers each endpoint, Arrow decoding and JSON equivalence, truncation, empty timestamp results, oversized UTF-8 rows, read-only rejection and catalog hiding after startup to verify HTTP/MCP snapshot reuse.
- [x] Width discovery reads one representative RBM header per shard field directory; configuration and requested-width mismatch checks remain. Fresh-process store-full status elapsed time: 85.546 s before, 70.199 s after (17.94% decrease). OS caches were not purged; these runs do not isolate the remaining snapshot/catalog startup cost.

This HTTP checkpoint predates the resolve/bind slice below. Configurable binding and loopback default are now implemented; NUTS/bearer authentication remains pending. HTTP ingest/sync/CSV, pgwire, resolve, intervals tools, packaging and full M5 gates remain pending. The engine captures a startup planning snapshot; restart to refresh externally changed catalogs/shards until ingest-supervisor integration. Arrow IPC bodies are bounded before writing their Content-Length; no unbounded result collection is added. Request bodies are capped at 64 KiB; chunked request encoding is unsupported.

## Rust surfaces — resolve and binding slice (2026-10-06)

- [x] MCP `ti_resolve {phrase, vessel?, limit?}` and `GET /ti/resolve?q=<phrase>&vessel=<URN-or-name-or-MMSI>&limit=8`. Candidates provide path, canonical column, agg, units, description, BM25 score, last value, timestamp and vessel. Unknown vessels are rejected; a vessel filter excludes unreported fields.
- [x] Offline Lume BM25 over stored columns, enriched by 488 pinned Signal K 1.8.4 schema metadata patterns. Runtime does not consume golden phrases or access the network. Custom paths fall back to path tokens. The configured server caches its immutable resolver alongside the shared TiEngine.
- [x] Last values come from the most recent populated bucket for each candidate, using shard presence bitmaps and one-row reads. Null last values indicate a registered field without data. No SI conversions are applied to stored column values.
- [x] `tests/golden/resolve.json`: 100 distinct manually authored phrases across 50 expected paths. The fixture ranks against 488 catalog columns and requires >= 90/100 top-3; verified result 93/100. Seven misses remain visible in the test diagnostics. This is the specified fixture gate, not a fleet-wide accuracy claim.
- [x] `lume serve --ti-store <root>` defaults to 127.0.0.1; `--bind <IP>` selects a boat LAN interface, 0.0.0.0, or IPv6 address explicitly. Ordinary serve without --ti-store retains 0.0.0.0 by default. The lead's explicit loopback-default requirement supersedes the earlier container-wide bind rule for the TI server.
- [x] No wildcard CORS on /ti success/error/OPTIONS replies. Configured TI server MCP/SSE replies and standalone TI tool replies also omit wildcard CORS, preventing an alternate browser path to the same telemetry. Non-TI serve transport retains its existing CORS behavior.
- [x] Real-server tests confirm loopback default, explicit bind, resolve HTTP/MCP shapes and values, URL decoding, invalid inputs, preflight/error CORS and shared snapshot reuse. Scratch uses CARGO_TARGET_TMPDIR.

### Next items

- [ ] NUTS authentication for shore, plus explicit boat bearer-token policy.
- [ ] Extended pgwire protocol, SCRAM authentication, catalog compatibility, then the remaining ingest/sync/plugin/packaging and M5 gates.

## Rust surfaces — simple-query Postgres and question fixtures (2026-10-06)

- `lume serve --ti-store <root> --pg <port>` enables an additional read-only Postgres listener; it is off by default. Port 0 requests an ephemeral port. It uses the same --bind address, including the default loopback when TI is configured, and the same startup TiEngine and admission gate as HTTP/MCP.
- This slice implements the simple-query protocol for psql and tokio-postgres simple_query, with one read-only statement per request. Values are encoded as nullable PostgreSQL TEXT fields; complex values use JSON text. Extended/binary protocol, transactions, SCRAM/TLS, pg_catalog compatibility and Pi query memory pools remain follow-ups. The listener uses no password authentication in this explicit opt-in slice; D13's SCRAM musl test still applies before shore authentication ships.
- The 500-row / 64 KiB engine cap and 64 KiB Postgres result framing cap apply. Over-cap queries fail explicitly with SQLSTATE 54000 and an aggregate/narrow-time hint, rather than returning silently incomplete rows. SQL requests are capped at 64 KiB; up to 32 Postgres connections are admitted.
- D37 records the approved optional pgwire dependency and disabled native TLS defaults. Root default builds do not enable the listener dependencies. engine.rs remains untouched for concurrent D30 work.
- tests/golden/fleet_questions.json contains 20 natural-language questions, oracle SQL, authoritative outputs with corpus provenance, and separate hand-calculated small-store expectations. The deterministic regression resolves the top-one canonical column, substitutes a SQL template and calls ti_query. A separate explicit golden-store replay uses the committed oracle outputs.
- The actual agent-with-only-MCP run and integrator grading remain required for M5 item 2; the deterministic harness does not mark that gate complete.
- Verified 20/20 full-store oracle comparisons before D30 integration, then 58 feature tests and the default build after rebasing onto ef9148a. The full-store replay was not repeated after D30. Host formatting/strict clippy and the actual M5 agent run remain assigned to Pike.
- The golden replay exposed source columns outranking ordinary value columns when `$source` prevented Signal K metadata matching. Source metadata now matches its base path and adds reporting-source/provenance context. The regression includes source-column distractors and checks explicit source requests still resolve to `$source`.

Signal K schema data attribution, commit pin and upstream CC-BY-SA 2.0 license are in src/ti_resolve/. No runtime dependencies or frozen contracts changed. Container bind-mount status timings are not a code optimization target; Pike's host timings supersede that diagnostic direction. Host Rust 1.96 fmt/strict clippy remain assigned to Pike for this slice.
