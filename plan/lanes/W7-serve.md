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
- [ ] `ti_resolve`: Lume hybrid search over the `paths` catalog + Signal K spec descriptions; 100-phrase test set.

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
- [ ] MCP tools live in `lume serve`; `ti_resolve` top-3 correct for ≥ 90 % of 100 phrases
- [ ] nemesis8 agent with only Lume MCP answers 20 scripted fleet questions, graded against oracle
- [ ] Plugin installs on HALPI2 (HaLOS Marine store) and OpenPlotter (Pi 4 4 GB + Pi 5), gets a token, supervises ingest; psql + Grafana pass a 20-query smoke set

## Stretch (M6+)
- Signal K History API provider (`/signalk/v2/api/history/values`).

## Note
This lane is far larger than the others (Rust server + Node plugin + container packaging + pg wire). Consider splitting into W7a (Rust surfaces) and W7b (packaging/plugin) — see [repo-fit](../repo-fit.md) §6.
