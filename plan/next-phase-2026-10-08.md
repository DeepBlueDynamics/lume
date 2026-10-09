# Lume TI — next phase plan (2 agents + lead), 2026-10-08

## Context

`plan/lume-ti` is pushed through `3e8ee9c`, with PR #4 open and CI green. Status of the gates:
- **Closed:** M2, M5 items 1–2, and M6 items 1–3.
- **D48 approved:** GO for the single-boat pilot.
- **D49 recorded:** gap 1 deferred, gap 2 run by the user, gap 3 accepted, gap 4 run by the lead, gap 5 done.

The Pi runs:
- the Lume plugin at `5aa7fa0` (`telemetry_lume` is live);
- Grub as a HaLOS app, on the local lite image tagged as the published one;
- Ollama as a HaLOS app, as a cloud proxy that isn't signed in yet;
- memory cgroups, now enabled, with auto-sized caps.

The Pi has only **4.5 GB free**.

The user's choices for this phase:
1. **Lume becomes the store for agent telemetry** (Hyperia and n8: tokens, file events, mail metadata), through a new **OTLP http/json receiver**.
2. **Drop the Ollama container on the Pi** (frees about 4.2 GB). The Ask tab calls **ollama.com directly**, with an API key read from a key file. `lume chat` already sends `OLLAMA_API_KEY` only to ollama.com hosts (`src/chat_sql.rs`: `with_ollama_auth`, `is_ollama_cloud`).
3. **The Ask tab defaults to `glm-5.3:cloud`.**
4. **The split:** Codex (Better Platypus) does the Rust work, Antigravity (Compact Echidna) does the plugin, packaging and docs, and the lead does the Pi deploys, merges and gap 2 support.

Rules for both agents, unchanged:
- Work only in your own lane: `.lanes/w4` for Codex, `.lanes/w3` for Antigravity.
- Use rustc 1.96 with `CARGO_INCREMENTAL=0`; run strict clippy and rustfmt on touched files only.
- No `0.0.0.0` binds in tests.
- Gate on exit codes.
- Keep `target` under 8 GB and run `cargo clean` at the end.
- Don't merge or push.
- Report by `msg_send`.

## Workstream A — Codex (Better Platypus): `ti/otlp` then `ti/pi-bench`

### A1. OTLP http/json receiver (`ti/otlp`)
- **Endpoint:** `POST /v1/metrics` and `POST /v1/logs`. These are the OTLP/HTTP JSON encoding paths Claude Code's exporter uses.
  - They're added to the existing TI HTTP server (`src/ti_http.rs` routes) when the server starts with `--otlp`.
  - They're also available standalone as `lume ti otlp --store <root> --bind 127.0.0.1 --port 4318`.
  - Loopback by default, plus an optional bearer token (the same pattern as the sync token).
- **Metrics:**
  - The entity comes from resource attributes. Use `agent.urn:<service.instance.id or pane>` (D38 generic entity), falling back to `agent.urn:<service.name>`.
  - Each metric name becomes a path.
  - Data points go through `WatermarkBucketer::ingest_point` and `flush_all` into the store (`crates/ti-ingest/src/watermark.rs`). Reuse the `SelfStore` pattern in `crates/ti-ingest/src/self_telemetry.rs`: own bucketer, flush per batch.
  - Sums and gauges map to numeric values. Histograms map to sum and count.
- **Logs:** each log record becomes one `docs` row through the DocStore API (`crates/ti-store/src/docs.rs`).
  - The title is the event name.
  - The body is the attributes, including file path, op, tool, lines added and removed, and mail from, to and kind.
  - `ts_start` is the event time. The entity is as above.
  - The row is searchable with `match(body, …)`.
- **Store:** a dedicated store, `<root>/stores/agents`, served as `telemetry_agents` and `docs`. It uses the same auto-registration as `telemetry_lume` (`crates/ti-sql/src/engine.rs`) and its own retention (default 90 days).
- **No new heavy dependencies.** Hand-written `serde` structs for the OTLP JSON subset (resourceMetrics, resourceLogs). No protobuf, and no opentelemetry crates unless the decisions log approves them.
- **Tests:**
  - Golden OTLP JSON fixtures for Claude Code-style metrics and logs, ingested and then queried through SQL: bucket sums, a doc `match()` on a file path, and the entity mapping.
  - A malformed payload gets a 400 and must not crash.
  - A body above a size limit (8 MiB) is rejected.
  - Loopback-only by default.
- **Decision:** record as D50, covering the endpoint, the entity mapping, and why no OTLP crates.

### A2. D48 gap 4: per-class p95 on the Pi (`ti/pi-bench`)
- **Cross-build `ti-query-bench` for arm64** with `scripts/cross-arm64.sh`. Codex prepares a `--bin ti-query-bench` variant and a 1-vessel store recipe; the lead runs the build on the host.
- **The store:** a deterministic 1-vessel × 90-day subset of `store-full`, or `ti-bench gen` with seed 42, small enough to fit within about 1 GB on the Pi.
- **Codex delivers** the harness command line, the store recipe and the result format (`bench/results/<date>-pi-<sha>.json`). The lead runs it on the Pi.

## Workstream B — Antigravity (Compact Echidna): `ti/plugin-cloud`, then `.deb` rebuild, then docs

### B1. Ask tab goes cloud-direct (`ti/plugin-cloud`)
- **New plugin settings** in the `plugins/signalk-lume-ti/index.js` schema:
  - `chatApiKeyFile`: path to a file that holds the ollama.com key, readable by the Signal K container user. The plugin never stores the key in its own config.
  - Default `chatOllamaUrl` becomes `https://ollama.com`, and default `chatModel` becomes `glm-5.3:cloud`.
- **`lib/chat.js`:** reads the key file at spawn time and passes `OLLAMA_API_KEY` in the child env only. It never goes on argv and never into logs. A missing or unreadable file gives a clear Ask-tab error naming the setting.
- **Tests (`test/chat.test.js`):**
  - the env carries the key when the file is set, and argv doesn't;
  - nothing is passed when the setting is unset;
  - an unreadable file gives an error.
- **`npm test` must exit 0, and `node --check` must pass on edited files.**

### B2. Rebuild the `.deb` packages and retire the Ollama app's default role
- Rebuild both `.deb`s with `scripts/build-halos-debs.sh`, to pick up the Maintainer `kord@deepbluedynamics.com` and the auto memory hooks (`app-prestart.sh`).
- Mark the Ollama app README "optional (laptop/shore local models); the Pi uses ollama.com directly".

### B3. Docs
- **SETUP:** an "Ask tab with ollama.com" section covering creating the key file on the Pi, its permissions, and the setting. Also note the OTLP receiver once A1 lands.
- **STATUS:** refresh after each merge.

## Lead (Industrial Pike)

1. **Pi cleanup** after B1 merges:
   - stop and disable `marine-ollama-container`;
   - `docker image rm ollama/ollama`, which frees about 4.2 GB;
   - keep its data directory until the cloud path is verified.
2. **Deploy the plugin to the Pi:** copy `plugins/signalk-lume-ti` at the merged head into `/var/lib/container-apps/marine-signalk-server-container/data/data/lume-plugin/signalk-lume-ti`, then restart Signal K. This also brings Library search through the server (`a5be3f8`) and the TLS options (`c769d23`).
3. **The user creates the key file** on the Pi, for example `/var/lib/container-apps/marine-signalk-server-container/data/data/plugin-config-data/signalk-lume-ti/ollama.key`, mode 600 and owned by the container user. The lead sets the plugin setting and verifies a real Ask-tab question end to end.
4. **Grub:** once there's headroom after step 1, run `docker pull deepbluedynamics/grubcrawler:latest-lite`, which replaces the locally tagged stand-in, then restart the app.
5. **Gap 4:** cross-build and run A2 on the Pi, and record the results in the benchmark report §2.
6. **Gap 2:** when the user is at the screen, run the OpenCPN contention test. The lead drives a Q4 + Q8 query loop for 10 minutes and samples Signal K delta latency and drops; the user pans and zooms.
7. **Merge, verify and push** each branch:
   - full `cargo test --features ti`;
   - crate tests and strict clippy;
   - `npm test`;
   - bench Python tests.

## Save current state (done at plan approval)
- Append a "Next phase" pointer to the STATUS Handoff section linking this plan. Update memory with the agent split and the Pi disk/Ollama decision.

## Verification
- **A1:** `cargo test --features ti` covering the new OTLP tests. Then a manual check: `curl -X POST localhost:4318/v1/logs -d @fixture.json`, then `lume ti query "SELECT … FROM docs WHERE match(body,'service.rs')"`.
- **A2 and gap 4:** a JSON results file from the Pi with p95 for all 26 queries.
- **B1:** `npm test` exits 0. On the Pi, an Ask-tab question answered through glm-5.3:cloud, with no key in `ps`/argv or the plugin logs.
- **Pi:** `df -h` shows 8 GB or more free after removing the Ollama image. All HaLOS services stay active, and `telemetry_lume` keeps receiving.

## Code map (verified 2026-10-08)
- HTTP: `src/agent.rs` `handle_connection` (~580) sends `/ti/*` to `ti_http::handle` (~848), then to `TiServer::response` (~403). The routes are the `match path` blocks at ~659 and ~732, and the sync token check is at ~413–656. Add `/v1/metrics` and `/v1/logs` alongside these.
- Server start:
  - `lume serve --ti-store`: `src/main.rs` ~182–285, calling `agent::serve_with_ti_pg_tls_config`.
  - `lume ti ingest --serve`: `handle_ti_ingest` (~365), then `TiServer::open_with_width` (~535).
- Docs: `crates/ti-store/src/docs.rs` `DocStore::open` / `upsert_all` (~131) / `delete` (~191). The `Document` fields are id, vessel, kind, ts_start, ts_end, title and body. Example callers are `crates/ti-sql/src/cli.rs` ~179 and `src/ti_parquet.rs` `run_docs_cli` ~333.
- Numeric ingest: `crates/ti-ingest/src/watermark.rs` `ingest_point` (~391) and `flush_all` (~567). Follow the `SelfStore` pattern in `crates/ti-ingest/src/self_telemetry.rs` (~138–181).
- Plugin chat: `plugins/signalk-lume-ti/lib/chat.js` ~42–60 (spawn with an inherited env). The schema is `index.js` `chatOllamaUrl` (~138) and `chatModel` (~144). The key handling is `src/chat_sql.rs` `with_ollama_auth` (~120) and `is_ollama_cloud` (~129).
- There are no OTLP or protobuf dependencies in the workspace.
