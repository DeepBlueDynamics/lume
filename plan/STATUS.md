# Lume TI — Status board

Last updated: **2026-10-06** (docs keeper, after `ddd6398`: **M6 HTTP sync merged**, plugin-managed SCRAM pgwire + Grafana provisioning, strict TI CI job, 50-min Pi benchmark; earlier: Influx-vs-Lume harness, History API provider, `lume sql`, CRoaring (D39/D40), notes/logbook (D41), pgwire SCRAM (D42))

Integration branch `plan/lume-ti` is at `ddd6398`. **`ti-contracts` is frozen** (`96ac45d`). Root tests: 46 at `8e87a11`; `cargo test --features ti` was 55 at `39c0096` (not recounted since). Plugin `npm test` 17/17 and `cargo test` `ti_http` 8/8 at `e09bb87`.
Workspace members: `ti-contracts`, `ti-core`, `ti-store`, `ti-sql`, `ti-ingest`, `ti-bench`, `ti-geo`, `ti-sync`. Signal K plugin: `plugins/signalk-lume-ti/`. Next free decision: **D43** (ask the lead before taking it).
Setup and workflow: [SETUP.md](SETUP.md).

## Critical path right now

- **🚢 Deploying to the user's Raspberry Pi 5** (HaLOS Marine RPI, no HAT, Signal K v2.31.1 in a container, Ubuntu 24.04 with glibc 2.39).
  - KIP and Freeboard-SK are installed. Grafana, QuestDB and OpenCPN are installed. Grafana shares the `influxdb` container's network namespace and reaches the host as `halos.local` = docker0 `172.17.0.1`.
  - `signalk-to-influxdb2` 2.3.0 writes InfluxDB bucket `marine` at 1 s resolution (self vessel only).
  - **Deployed:** the thin-LTO native arm64 binary at `1bbbac2` (`CARGO_PROFILE_RELEASE_LTO=thin`, `CODEGEN_UNITS=16`, `BUILD_JOBS=2`; fat LTO OOMs) runs in the plugin (glibc 2.39 OK), with the plugin JS and History provider. `postgresql-client` is installed on the Pi.
  - **Vessel UUID pinned.** Signal K on HaLOS regenerated its self UUID on every restart, which split history in both Lume and Influx. The lead pinned it in `data/baseDeltas.json` (`urn:mrn:signalk:uuid:0eb191d0-1f5a-42da-979e-ead792d676ee`). Now a deployment step ([SETUP §3](SETUP.md)).
  - **pg smoke on the Pi** (throwaway loopback instance): SCRAM login OK, **16/20 pass**. It stopped at case 17 (raw 24 h series) on the 500-row / 64 KiB cap. Fix in flight on `ti/pg-limits` (Artificial Shark).
  - **Access request still pending the user's approval.** Live ingest isn't blocked (`allow_readonly` is true), but the notes/logbook poller needs the token.
  - **Next:** `ti/pg-limits`, then rerun the pg smoke and Grafana Save & Test; the user approves the access request; select Lume as the default history provider.
  - **The Pi runs hot without an Active Cooler** (82–86 °C under load).
- **📊 Pi benchmark, 50-min window** (`bench/influx_vs_lume.py`, 23:00–23:50Z, 20 runs, warm p50, InfluxDB vs Lume). Replaces the earlier 17-min preliminary table:

  | Query | InfluxDB | Lume | Result |
  |---|---|---|---|
  | Raw depth | 42.9 ms | 43.7 ms | |
  | Hourly max | 21.4 ms | 6.1 ms | PASS |
  | Minute mean SOG | 15.2 ms | 9.3 ms | FIDELITY 0.44 % |
  | Multi-condition | 53.2 ms | 9.5 ms | 40/40 minutes; minute minimums differ (Influx drops samples) |
  | Min depth + position | 156.8 ms | 5.6 ms | Same depth; Influx's first occurrence is 10 min later (drops) |
  | Per-path counts | 4,080 ms | 57 ms | One-bucket gap on 3 constant battery paths (290 Influx vs 289 Lume), **open** |

  Caveat: the HaLOS Influx writer's 1 s resolution drops samples, so raw bucket means and minimums differ from Lume's.
- **✅ Signal K History API provider** (`e09bb87`, merge of `64cbc0d`). `plugins/signalk-lume-ti` registers as a Signal K v2.31 History API provider backed by Lume's loopback HTTP. Plugin tests 17/17, `ti_http` 8/8. `first`/`last` need the `@last` aggregate retained. **It must be selected as the server's default history provider**, because `signalk-to-influxdb2` also registers one.
- **✅ M6 HTTP sync merged** (`ddd6398`, `c99b66a`, Artificial Shark). See the M6 gate detail. **Pending:** the release 5-vessel run, then the 50-vessel release run and lossy-HTTP results go to the lead.
- **✅ Grafana-compatible pgwire + SCRAM** (`7c4cb23`, post-merge fixes `2bbcc4b`). Typed results, extended protocol, `pg_catalog`, Grafana macros, and verifier-only SCRAM-SHA-256 (**D42**). TLS is still not offered. **Plugin-managed** since `1bbbac2` (see Merged surfaces).
- **✅ Strict TI CI job** (`ec48673`, fmt fix `329d8a1`). Rust 1.96 pinned; fmt on 8 crates plus the root TI files; clippy `-D warnings` on the TI crates; `cargo test --features ti`; plugin npm on Node 20; bench Python. **Not exercised in GitHub yet**, because `plan/lume-ti` is local only.
- **✅ W10 alerts ACCEPTED** (`a8f32bd` + `6d51df5`). Signal K notifications become `alerts` documents (`d656dd4`). Bitmap alert rules run on closed buckets, with history, hold, caps and live document refresh. Host DuckDB oracle: **118/118/118** ranges, and `match(alerts, 'battery')` covers 357 buckets.
- **✅ W9 generic Parquet ACCEPTED** (`167f391` + `dd2db2f`). Mapped long/wide Parquet and document import, common backfill drivers, D38 entity identity, and a robot-fleet golden set. Robot DuckDB oracle: **14/14** non-empty and matching. `lume ti verify` on robots: **14/0/0**. Boat regression on the new backfill path: **65/65 seal hashes identical**, corpus **58/0/4**. Backfill ran at 174,769 rows/s.
- **✅ Sealed-data repair fixed** (`b23514a`). An append to a sealed shard used to lose data. Now the shard is restored and resealed as a new version, and identical content keeps the same hash. The host suite passes, including the 1,000-run kill -9 test.
- **Merged surfaces:**
  - pgwire (`--pg`, D37, `451bfc7`; extended protocol and SCRAM in `7c4cb23`, D42). The 20 scripted fleet questions pass as a regression (20/20), but **the M5 item 2 agent run is still open**; the lead does it.
  - `lume sql`: read-only SQL over ordinary Lume indexes, the `lume_sql` MCP tool, and `--docs-index` in TI sessions (`c130bc1`).
  - Signal K notes and optional logbook polled into owned documents (D41), chart pins in the plugin (`db2e777`).
  - Pi 5 fixes (`042d681`): live serve reopens read-only on flush/seal (5 s debounce), so it sees new data; applied-timestamp ingest status; plugin UUID access request.
  - `bench/influx_vs_lume.py` (`e9d95fc`, `9e823ea`, `0c4cdf6`): stdlib-only paired Influx-vs-Lume harness, 19 unit tests, statuses PASS/FIDELITY/MISMATCH/EMPTY.
  - Plugin-managed SCRAM pgwire (`1bbbac2` + `495d836`): plugin options `enablePg`, `pgPort` (5864), `pgUser`, `pgBind`; an admin-only webapp form stores only the verifier. `--pg-bind` and `--pg-auth-config` (auth section only, replaces the store's users, mode 0600 required on Unix). Grafana provisioning `bench/grafana/lume-ti-datasource.yaml` (`halos.local:5864`, password from `$__env{LUME_PG_PASSWORD}`). 20-query `tests/pg_smoke.sh`; its Python harness test skips on Windows.
  - The notes/logbook poller treats 404 or anonymous 401 on optional sources as absent; it logs once, then hourly (`0c4cdf6`).
  - Top-level `lume --help` lists `ti query|repl|ingest|status` (`b3cce8b`; only in `--features ti` builds).
  - `lume ti ingest` live service (`75a1a4f`).
  - `lume ti backfill --parquet|--signalk`.
  - `lume ti rules list|test`.
  - `lume ti repl` (`f1bb15a`).
  - The `signalk-lume-ti` plugin (`31841c3`).
- **In flight:**
  - Long Horse: **`count_paths`** (approved contract plus a magnitude amendment: count every finite sample; representable values still feed mean/min/max/last; `path@skipped_magnitudes` is reported). Unblocks `q1-007`, `q6-006`, `q2-001`. Corpus and hash checks are moving to the host.
  - Artificial Shark: **`ti/pg-limits`**: configurable `pg_max_rows` / `pg_max_bytes` (default 100,000 / 16 MiB) with streaming. HTTP and MCP stay at 500 rows / 64 KiB. Then the M6 release runs.
- **🟡 User decisions:** item 16, the CC-BY-SA licence on `src/ti_resolve/signalk_paths.json`, is still needed before publishing.
- **Disk policy (lead):**
  - Keep at least **25 GB free** on the host.
  - Keep all build caches together at **25 GB or less**, and each agent's `target/` at **8 GB or less**.
  - Run `cargo clean` after each handoff.
  - Delete verification stores immediately.
  - See [SETUP §8](SETUP.md).

## Pane changes (about 06:00 UTC)

| Role | Now | Formerly |
|---|---|---|
| W4–W7, W9, W10 SQL, surfaces, Parquet, alerts | **Long Horse** `888bff45`, nemesis8/n8-hazy-badger | Rigid Roadrunner `d58ca1b1`, n8-sly-viper |
| W3 ingest, W8 sync and bench, plugin | **Artificial Shark** `fd91f4b1`, nemesis8/n8-quiet-crane | Romantic Pike `90fc608c`, n8-keen-kiwi |
| Corpus and generator | Lead took it over and merged it (`c65e515`) | Zygomorphic Prawn `eccaf836`, n8-noble-toad: **retired** (permanently offline, per the user) |
| Host build pane | **Compact Echidna** `6914c38e` | Regular Pheasant `364a3fc7` (gone) |

Commit messages, the decisions log and older docs use the former names. Re-check pane ids with Hyperia `terminal_status` before mailing.

## Recent merges into `plan/lume-ti`

| Commit | What |
|---|---|
| `ddd6398` | **M6** (`c99b66a`, Artificial Shark): `ti-sync` HTTP transport, shore endpoints `/ti/manifest` and `/ti/shards/...` with bearer auth, `lume ti sync --to`, `[sync]` in `ti.toml`, lossy-HTTP two-node test, fleet equality test (`TI_FLEET_VESSELS`). Host fmt, clippy and sync tests green |
| `ec48673` + `329d8a1` | Strict TI CI job (`8c603dc`), legacy jobs unchanged; `329d8a1` rustfmts `src/ti_parquet.rs` for the new root-format check |
| `1bbbac2` + `495d836` | **Plugin-managed SCRAM pgwire** (`7aa42b5`, Long Horse): `--pg-bind`, `--pg-auth-config`, admin-only verifier form, HaLOS Grafana provisioning, 20-query `tests/pg_smoke.sh`, `tests/golden/grafana-smoke.json`. `495d836`: rustfmt root TI files; pg_smoke harness test skips on Windows |
| `ebd96a4` | rustfmt `ti-ingest` after the pi-polish merge |
| `0c4cdf6` | `ti/pi-polish` (`3788614`): benchmark threshold-edge and path-type classification; the notes/logbook poller treats 404 or anonymous 401 on optional sources as absent, logs once then hourly |
| `9e823ea` | `ti/bench-influx` (`21546f1`): bucket-aligned windows, lat/lon position, FIDELITY status, path-set diff |
| `e9d95fc` | **`bench/influx_vs_lume.py`** (`bd465ad`, Long Horse): stdlib-only paired InfluxDB-vs-Lume Signal K query benchmark (19 unit tests by `0c4cdf6`) |
| `b3cce8b` | Top-level `lume --help` lists the TI subcommands (`ti query`, `repl`, `ingest`, `status`); `--features ti` builds only |
| `e09bb87` | **Signal K v2.31 History API provider** (`64cbc0d`, Long Horse) in `plugins/signalk-lume-ti`, backed by Lume loopback HTTP. npm 17/17, `ti_http` 8/8. Needs `@last` retained for `first`/`last`; must be the server's default history provider |
| `7c4cb23` + `2bbcc4b` | **Typed Grafana-compatible pgwire** (`2cc5597`): extended protocol, `pg_catalog`, Grafana macros, verifier-only SCRAM (**D42**). `2bbcc4b`: rustfmt of the pgwire files; `main` runs on a 64 MB thread (the debug build overflowed the 1 MB Windows main stack); webapp shows a Signal K login hint on 401 |
| `530f6b1` | Webapp `apiBase` is `/plugins/signalk-lume-ti`, presets fixed, display name "Lume TI" |
| `0c169c2` | Tests bind loopback only (no `0.0.0.0`, so no Windows Firewall prompts); Windows-only race in the Signal K resources mock fixed |
| `db2e777` | Signal K notes and optional logbook into documents (**D41**), plugin chart pins and owned-resource cleanup, portable plugin tests, live-server document freshness test |
| `042d681` | `ti/pi5-fixes` (`c79a91d`): live serve read-only reopen on flush/seal (5 s debounce), empty-store width, applied-timestamp ingest status, plugin UUID access request |
| `f38ecb4` + `b173296` | Backfill profiling (no regression) and the **CRoaring evaluation**: D39 bench-only `bench/croaring-eval`, **D40 reject for M4** ([design/croaring-eval.md](design/croaring-eval.md)) |
| `c130bc1` + `ff20ec4` | **`lume sql`** (`f711cfc`): read-only SQL over Lume indexes, `lume_sql` MCP tool, `--docs-index` in TI sessions |
| `167f391` + `dd2db2f` | **W9 final** (`ebdb848`, Long Horse): mapped document import (`lume ti import-docs --parquet`), common backfill drivers, the robot-fleet generator (`ti-bench gen --profile robots`) and a 14-query golden set (`tests/golden/robots/`) with a host DuckDB oracle. **Accepted** (see Critical path). `dd2db2f`: host fmt and a run-time `CARGO_TARGET_TMPDIR` fix |
| `c4e3a63` | **D38** entity identity (opaque `<kind>.urn:<id>`) and bounded per-entity watermark backfill with a scratch journal (`70d047d`). Host D38 regression: 65/65 seal hashes identical, corpus 58/0/4 |
| `31841c3` + `08836bb` | **`signalk-lume-ti` plugin** (`ec3d43b`, Artificial Shark): supervision, Signal K access-request auth, loopback proxy, SQL webapp. Also `ti-bench --hz` with per-path override and the W8 bench harness (`2159fa6`) |
| `dfce248` + `b6c6ee0` | **W9 first slice** (`857d777`): mapped long/wide Parquet reader, units/scales, `lume ti backfill --parquet`. Host fmt and strict clippy |
| `b23514a` + `6297cf2` | **Sealed-data repair** (`67b44ca`): appends to a sealed shard restore it and reseal as a new version; identical content keeps the hash. Host suite incl. the 1,000-run kill -9 test passes |
| `75a1a4f` + `cdd9628` | **`lume ti ingest` live service** (`4971810`, `e6b604a`): reconnect, flush/seal/retention timers, SIGTERM WAL flush, `ingest_status.json`, `--serve` on loopback by default. `cdd9628` gates the shutdown-signal handler to `cfg(unix)` |
| `a8f32bd` + `6d51df5` | **W10 rules** (`1760742`): bitmap alert rules with history, hold, caps and live document refresh. `6d51df5` fixes the rules oracle (raw path encoding; 30 s hold, because the 5 min hold gave zero runs and passed vacuously). **W10 accepted** (118/118/118) |
| `8d232ca` + `0344835` | Live `receive_time` fix (live timestamps were clamped to EPOCH) and non-fatal notification errors (`3c192b0`) |
| `6bd99a4`, `d656dd4`, `a1c65e9` | W10 step 1: `ClosedBucketObserver` hook (`1afb7f3`), and Signal K notifications kept as `alerts` documents in live ingest and backfill (`cb61ff7`). Host fmt/clippy |
| `13c58c0` + `12cad6a` | **ti-sync** (`0f2f642`, Artificial Shark): manifest diff, resumable chunked shipping, shore import, lossy two-node test (20 % loss, 30 min outage); D30 follow-ups. Host fixes |
| `451bfc7` | **pgwire** (`7118ef8`, Long Horse): read-only simple-query Postgres listener (`--pg`, **D37**) and the 20 scripted fleet questions (regression 20/20) |
| `41858ed`, `e72298b` | W10 lane file (notifications as alerts; rules that write their own alerts) and W9 lane file (generic time-series Parquet) |
| `f1bb15a`, `1d362d0` | `lume ti repl` and `backfill_store` `TI_WIDTH`. README: documents and time series in one store |
| `ef9148a` | **D30 end to end** (`4a90c96`, Artificial Shark): multi-store backfill and stream, the `telemetry_hr` table, retention drops only sealed shards |
| `d3ce0f0` | Docs refresh after W7 HTTP and `ti_resolve` |

Earlier: `39c0096` `ti_resolve` 93/100 + `--bind`, `0b3a17a` fmt, `400557c` README TI section, `bce7779` **HTTP `/ti`**, `4626591` `open_timing`, `b95f890` docs, `26e535a` host fmt, `20117c4` **M2 idempotence + D30 fan-out**, `31fd76a` **W7 CLI/MCP**, `cb39a4d` **full corpus 58/0/4**, `3354bd0` line-tables-only, `b0ba6b0` docs, `cd9bb3e`/`11d961a`/`ad05f94`/`c1d4941` **W5 text**, `6dc19c5` docs, `711d2c4` **closes M3**, `f6c47b4`, `b07ec05`, `f5806c0` D34, `bfd7373` D33, `8f776da`/`82a8aa5`/`829c771` backfill speed, `5632aad`, `b4a1879` **Q4 72.4×**, `be6ffdb`, `312f6a0` **closes M0**, `8931877` **W6 geo**, `cf98c61` W2 (**M1 complete**), `f7faf5f` W1, `96ac45d` contracts freeze, `06dd5c5` workspace. Full list: `git log --oneline --first-parent plan/lume-ti`.

## Agents

| Agent (pane name) | Pane | Lane/scope | Branch | Clone | Last known state |
|---|---|---|---|---|---|
| Industrial Pike | `ee764a09` | Lead and integrator. Host runs and oracles (DuckDB, `lume ti verify`, rules and robots oracles, D38 regression), host fmt/clippy at merge, disk policy. **Pi 5 deployment** and the Influx-vs-Lume benchmark runs. Owns the M5 item 2 agent run | `plan/lume-ti` | shared tree | `ddd6398` |
| Long Horse (formerly Rigid Roadrunner) | `888bff45` | **`count_paths`** contract with the magnitude amendment. Delivered plugin-managed pgwire, the Influx-vs-Lume bench, pgwire + SCRAM, the History API provider, `lume sql`, the CRoaring evaluation, W9 and W10 | `ti/count-paths` | `.lanes/w4` | In progress; `ti/plugin-pg` merged in `1bbbac2` |
| Artificial Shark (formerly Romantic Pike) | `fd91f4b1` | **`ti/pg-limits`** (configurable pg row/byte caps, streaming), then the M6 release runs. Delivered M6 HTTP sync, ti-sync, the live ingest service, the plugin, `--hz` and the bench harness | `ti/pg-limits` | `.lanes/w3` | In progress; `ti/m6-fleet-sync` merged in `ddd6398` |
| Zygomorphic Prawn | `eccaf836` | — | — | `.lanes/corpus` (retired) | **Retired.** Its `target/` has been deleted |
| Compact Echidna (formerly Regular Pheasant) | `6914c38e` | Host build pane (PowerShell 7, rustc 1.96.1). Not an agent | — | — | Bulk data and DuckDB jobs run here. **Check free disk before big builds** |

How to refresh the last column: `git -C .lanes/<x> log --oneline -5` and `git -C .lanes/<x> status --short`.

## Milestones

Week numbers count from kickoff. A milestone closes only when every gate test passes in CI ([spec/12](spec/12-milestones.md)).

| Milestone | Lanes | Weeks | Gate status |
|---|---|---|---|
| M0 Contracts | W0 | 1 | **✅ Complete** (`312f6a0`) |
| M1 Core and store | W1, W2 | 2–4 | **Complete** (`cf98c61`). Sealed-data repair hardened in `b23514a` |
| M2 Ingest | W3 | 2–5 | **In progress.** Items 1 and 2 passed. Item 3 (Pi 5 throughput/RSS) is open; the Pi 5 is now being set up |
| M3 SQL and pushdown | W4 | 2–6 | **✅ Closed 2026-10-06** (`711d2c4`) |
| M4 Text, geo, intervals | W4, W5, W6 | 6–8 | **In progress.** Item 1 met except 4 contract exclusions (58/0/4). Item 2 met (72.4×). Item 3 met: CRoaring evaluated and rejected for M4 (D40, `f38ecb4`) |
| M5 Agent surface | W7 | 7–9 | **In progress.** Item 1 met (93/100). Item 2: scripted questions pass as a regression (20/20), but **the agent run is still open** (lead). Item 3: plugin, History API provider and plugin-managed SCRAM pgwire merged and deployed on the Pi (token pending). Pi pg smoke 16/20, blocked by the pg row/byte cap (`ti/pg-limits`) |
| M6 Fleet and benchmarks | W8, integrator | 9–12 | **In progress.** HTTP sync, the lossy-HTTP test and the fleet test merged (`ddd6398`). Release runs (5, then 50 vessels) pending. Influx-vs-Lume Pi benchmark run (50 min); report not written |

Also landed outside the original milestones: W9 generic Parquet (accepted) and W10 alerts (accepted). Remaining: M6 release runs and report, `ti/pg-limits` plus the Pi pg/Grafana smoke, `count_paths`, and the Pi 5 deployment.

### M2 gate detail

| # | Gate item | State |
|---|---|---|
| 1 | Replaying a recorded 24 h delta log yields `BucketRecord`s equal to oracle bucketing | **Passing on real data**: 460,112/460,112 (`8931877`) |
| 2 | Parquet backfill of the correctness set is idempotent | **✅ Passed** (`11c0edc`): 65 sealed shards identical on rerun. Still holds on the W9 backfill path (65/65, `167f391`) |
| 3 | Pi 5 sustains 20,000 values/s for 1 h within the CPU and RSS budget | **Open.** Only a host x86 figure so far (181,783 values/s). The user's Pi 5 is being set up; note that it runs at 82–86 °C under load without an Active Cooler |

### M4 gate detail

| # | Gate item | State |
|---|---|---|
| 1 | Full golden corpus green, including `match()`, `in_bbox`, `within_nm` and `intervals()` | **Met except 4 contract exclusions**: 58 passed, 0 failed, 4 excluded (`q1-007`, `q6-006`, `q2-001` need a `count_paths` contract; `qx-003` DataFusion 55 limit, covered by `qx-013`). Re-confirmed after D38 and W9 |
| 2 | `BitmapAggregateExec` ≥ 10× faster than the materializing path on Q4 at shore scale | **Met in release**: 72.4× (`b4a1879`) |
| 3 | `croaring` frozen-view evaluation written up in the decisions log, adopt or reject | **✅ Met: rejected** (D40, `f38ecb4`; [design/croaring-eval.md](design/croaring-eval.md)). A Portable-view prototype is a follow-up needing its own C-dependency approval |

### M5 gate detail

| # | Gate item | State |
|---|---|---|
| 1 | MCP tools live in `lume serve`, and `ti_resolve` returns the right column in the top 3 for ≥ 90 % of a 100-phrase test set | **✅ Met** (`39c0096`): 93/100 (fixture accuracy) |
| 2 | A nemesis8 agent with only Lume MCP answers 20 scripted fleet questions; integrator grades against oracle results | **Open.** The 20 questions exist (`tests/golden/fleet_questions.json`) and pass as a regression (20/20, `451bfc7`). The agent-only run and grading are still to do (lead) |
| 3 | Signal K plugin installs on a HALPI2 from the HaLOS Marine container store, and on OpenPlotter from the Signal K App Store (Pi 4 4 GB and Pi 5), obtains a token and supervises ingest. psql and Grafana pass a 20-query smoke set | **Open.** Plugin merged (`31841c3`) with access-request auth and supervision, plus the History API provider (`e09bb87`). pgwire merged (`451bfc7`), Grafana-compatible with verifier-only SCRAM since `7c4cb23` (D42; TLS not offered), plugin-managed with Grafana provisioning since `1bbbac2`. **Installed on the Pi 5** by hand (binary at `1bbbac2`); the token is still pending the user's approval. **psql smoke on the Pi: 16/20**, stopped at case 17 on the 500-row/64 KiB cap (`ti/pg-limits`). Grafana Save & Test, store-based installs, Pi 4 and OpenPlotter are not done |

### M6 gate detail

| # | Gate item | State |
|---|---|---|
| 1 | Two-node sync converges with 20 % chunk loss and a 30-minute link outage | **In progress.** Passes in process (`13c58c0`). The HTTP version (`two_node_sync_http.rs`, bearer auth) merged in `ddd6398`; the lossy-HTTP results go to the lead |
| 2 | Shore node answers fleet queries across 50 synthetic vessels | **In progress.** Fleet equality test merged (`ddd6398`, `TI_FLEET_VESSELS`, default 50). Debug: 5 vessels 107 s, 10 vessels 243 s. Release 5-vessel run pending, then the 50-vessel release run (results to the lead) |
| 3 | Benchmark report against every target; go/no-go in the decisions log | **Open.** W8 bench harness (`2159fa6`) and the Influx-vs-Lume harness merged; a 50-min Pi run exists (Critical path). No report yet |

M0, M1 and M3 gate details are unchanged since they closed; see `git show d3ce0f0:plan/STATUS.md`.

## Open decisions waiting on the user

From [spec/11-risks-decisions.md](spec/11-risks-decisions.md) (open questions):

1. ~~Default bucket width: 10 s, or 1 s with 1-min rollups?~~ **Resolved by D30**: keep 10 s, and add a 1 s `telemetry_hr` store for the navigation, wind and depth allow-list.
2. Per-source values as first-class columns in v1, or only `$source` sets?
3. AIS contacts as a second `contacts` table, or out of scope?
4. Shore storage: local NVMe only, or sealed shards in object storage with a read cache?
5. Should `ti_resolve` also index Signal K spec descriptions for paths a vessel has never reported?
6. Does anything besides sealed shards move to shore? (p. 7 says only sealed shards. p. 20 ships open-shard WAL tails every 5 min)
7. PV-1 Done criterion: HALPI install from the Signal K App Store (p. 2), or from the HaLOS Marine container store (p. 19)?
8. Licensing check: borrow ideas only from FeatureBase (Apache-2.0), copy no code, keep Lume BSD-3-clean.

From [spec/02-pilot-vessel.md](spec/02-pilot-vessel.md) (owner assumptions to confirm):

9. Boat computer: exact HALPI model, CM5 RAM size, OS image (HaLOS Marine or desktop + OpenCPN), and whether OpenCPN runs on the same HALPI. *(The test device now is a Raspberry Pi 5 on HaLOS Marine RPI with OpenCPN installed.)*
10. Final NMEA 2000 equipment list: MFD brand, instrument vendor, engine gateway.
11. Whether Victron stays on 2.0, and whether a Cerbo GX or Venus OS device is aboard.
12. Consent to record 90 days of data for the benchmark dataset, and what may be shared publicly.
13. Satellite link (Starlink or Viasat) and the data budget `ti-sync` may use.

Spec deviations and proposals needing the user:

14. **D27: accept `zstd-sys` (C)**, forced in by DataFusion 55.1.0's `arrow-ipc`. Amends D11 and deviates from spec/10 (C bindings only for `croaring`). Awaiting spec-owner confirmation.
15. **Pinned `rust-toolchain.toml`**, proposed because the host (rustc 1.96.1) and the containers (1.99) report different clippy lints.
16. **Licence of `src/ti_resolve/signalk_paths.json`** (150 KB, `39c0096`). Extracted from SignalK/specification 1.8.4 @ `fb628fb4` under **CC-BY-SA 2.0**, with its own `LICENSE` and `README.md` in `src/ti_resolve/`. Lume is BSD-3. Decide before publishing.
17. **Pi 5 cooling:** the user's Pi 5 reaches 82–86 °C under load without an Active Cooler. Fit one before the M2 item 3 one-hour run (suggestion; unconfirmed whether planned).

## Open items (team, not user)

- [ ] **Pi 5 deployment** (lead): binary at `1bbbac2` and plugin JS deployed, vessel UUID pinned. Still to do: the user approves the plugin's access request (needed by the notes/logbook poller), and select Lume as the default history provider (over `signalk-to-influxdb2`'s).
- [ ] **`ti/pg-limits`** (Artificial Shark): configurable `pg_max_rows`/`pg_max_bytes` (100,000 / 16 MiB default, streaming). Then rerun the Pi pg smoke (16/20 now) and Grafana Save & Test for M5 item 3.
- [ ] **Benchmark per-path counts gap:** 3 constant battery paths show 290 Influx buckets vs 289 Lume. Explain or fix.
- [x] Influx-vs-Lume 50-min Pi run (20 runs; see Critical path).
- [x] Plugin-managed SCRAM pgwire + Grafana provisioning (`1bbbac2`).
- [ ] **M5 item 2 agent run** (lead): a Lume-MCP-only agent answers the 20 scripted questions; grade against oracle results.
- [ ] **M6 release runs** (Artificial Shark): the 5-vessel release fleet run, then the 50-vessel release run and the lossy-HTTP results to the lead. Then the benchmark report and the go/no-go. (HTTP sync merged in `ddd6398`.)
- [x] `lume sql` and `--docs-index` (`c130bc1`).
- [x] `croaring` evaluation (M4 item 3): rejected, D40.
- [ ] **`count_paths` host acceptance** (`ti/count-paths`): approved contract implemented; local count-path unit and engine tests pass, including finite overflow, retained value aggregates and skipped-magnitude status. The empty-list 65-hash comparison, enabled-list boat totals and unchanged DuckDB oracles are pending the lead's host run. See [the contract](design/count-paths.md) and `tests/golden/count_paths_oracle.py`.
- [ ] **`qx-003`**: keep the join-form `qx-013`, or revisit on a DataFusion upgrade.
- [ ] pgwire TLS. SCRAM landed (D42, `7c4cb23`); D13's aarch64 smoke: SCRAM login works on the Pi (`tests/pg_smoke.sh`, 16/20 so far).
- [ ] **User licence decision** on `signalk_paths.json` (user item 16).
- [ ] Meridian VHF transcripts. Not done. (Notes and logbook polling landed in `db2e777`, D41.)
- [ ] Pi 5 throughput/RSS run for M2 item 3.
- [ ] `tests/golden/README.md` still describes the W10 rule as `for 5m`, but the oracle and test now use a 30 s hold (`6d51df5`). Owner to fix (outside docs-keeper scope).
- [x] W10 alerts accepted (118/118/118; `match(alerts,'battery')` 357 buckets).
- [x] W9 accepted (robots 14/14 DuckDB, verify 14/0/0; boat 65/65 hashes, 58/0/4; 174,769 rows/s).
- [x] Sealed-data repair (`b23514a`); D38 regression 65/65, 58/0/4 (`c4e3a63`).
- [x] Live ingest service, plugin, pgwire, ti-sync, D30 end to end.
- [x] Grafana-compatible pgwire + SCRAM (D42), History API provider (`e09bb87`), loopback-only tests (`0c169c2`).
- [ ] Verify [signalk-formats §1.3](design/signalk-formats.md) against a **real** signalk-parquet `.parquet` file with DuckDB `DESCRIBE`.
- [ ] Strict TI CI job is in `ci.yml` (`ec48673`) but has **never run on GitHub**: `plan/lume-ti` is local only, and CI triggers on `main`. Containers lacking rustfmt or clippy still rely on the lead's host run.
- [ ] Toolchain drift between the host and the containers (item 15).

Also undecided: the repo-fit items in [repo-fit.md](repo-fit.md), such as the W7a/W7b split (§6). These are lead decisions unless escalated (unconfirmed).
