# Lume TI — Status board

Last updated: **2026-10-06** (docs keeper, after `dd2db2f`: **W9 and W10 accepted**, pgwire, ti-sync, live ingest service, Signal K plugin, D37/D38; Pi 5 deployment under way)

Integration branch `plan/lume-ti` is at `dd2db2f`. **`ti-contracts` is frozen** (`96ac45d`). Root tests: 46 at `8e87a11`; `cargo test --features ti` was 55 at `39c0096` (not recounted since).
Workspace members: `ti-contracts`, `ti-core`, `ti-store`, `ti-sql`, `ti-ingest`, `ti-bench`, `ti-geo`, `ti-sync`. Signal K plugin: `plugins/signalk-lume-ti/`. Next free decision: **D39** (ask the lead before taking it).
Setup and workflow: [SETUP.md](SETUP.md).

## Critical path right now

- **🚢 Deploying to the user's Raspberry Pi 5** (HaLOS Marine RPI, `192.168.68.61`, Signal K v2.31.1 in a container, Ubuntu 24.04 with glibc 2.39).
  - KIP and Freeboard-SK are installed. InfluxDB, Grafana, QuestDB and OpenCPN are installed but **paused during the build**.
  - Lume is being built **natively on the Pi**. The fat-LTO final link needed swap: rustc reached about 6 GB RSS, and 10 GB of temporary swap files were added.
  - **Next:** install `signalk-lume-ti` with the arm64 binary ([SETUP §3](SETUP.md)).
  - **The Pi runs hot without an Active Cooler** (82–86 °C under load).
- **✅ W10 alerts ACCEPTED** (`a8f32bd` + `6d51df5`). Signal K notifications become `alerts` documents (`d656dd4`). Bitmap alert rules run on closed buckets, with history, hold, caps and live document refresh. Host DuckDB oracle: **118/118/118** ranges, and `match(alerts, 'battery')` covers 357 buckets.
- **✅ W9 generic Parquet ACCEPTED** (`167f391` + `dd2db2f`). Mapped long/wide Parquet and document import, common backfill drivers, D38 entity identity, and a robot-fleet golden set. Robot DuckDB oracle: **14/14** non-empty and matching. `lume ti verify` on robots: **14/0/0**. Boat regression on the new backfill path: **65/65 seal hashes identical**, corpus **58/0/4**. Backfill ran at 174,769 rows/s.
- **✅ Sealed-data repair fixed** (`b23514a`). An append to a sealed shard used to lose data. Now the shard is restored and resealed as a new version, and identical content keeps the same hash. The host suite passes, including the 1,000-run kill -9 test.
- **Merged surfaces:**
  - pgwire, read-only and simple-query (`--pg`, D37, `451bfc7`). The 20 scripted fleet questions pass as a regression (20/20), but **the M5 item 2 agent run is still open**; the lead does it.
  - `lume ti ingest` live service (`75a1a4f`).
  - `lume ti backfill --parquet|--signalk`.
  - `lume ti rules list|test`.
  - `lume ti repl` (`f1bb15a`).
  - The `signalk-lume-ti` plugin (`31841c3`).
- **M6 in progress:** ti-sync merged (`13c58c0`), with the lossy two-node test (20 % chunk loss, 30 min outage) passing in process. HTTP transport and the 50-vessel shore test are in flight.
- **In flight:**
  - Long Horse: `lume sql` (SQL over ordinary Lume indexes) plus `lume ti query --docs-index`. Its clone has `f711cfc`, not merged yet.
  - Artificial Shark: M6 HTTP sync transport with bearer auth, the 50-vessel shore fleet test, and the lossy test over HTTP.
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
| Industrial Pike | `ee764a09` | Lead and integrator. Host runs and oracles (DuckDB, `lume ti verify`, rules and robots oracles, D38 regression), host fmt/clippy at merge, disk policy. **Pi 5 deployment.** Owns the M5 item 2 agent run | `plan/lume-ti` | shared tree | `dd2db2f` |
| Long Horse (formerly Rigid Roadrunner) | `888bff45` | **`lume sql`**: SQL over ordinary Lume indexes, plus `lume ti query --docs-index`. Delivered pgwire, W9 and W10 | `ti/w7-surfaces` | `.lanes/w4` | `f711cfc` "read-only SQL over Lume indexes and shared TI sessions", not merged yet |
| Artificial Shark (formerly Romantic Pike) | `fd91f4b1` | **M6**: HTTP sync transport with bearer auth, the 50-vessel shore fleet test, the lossy test over HTTP. Delivered ti-sync, the live ingest service, the plugin, `--hz` and the bench harness | `ti/w3-ingest` | `.lanes/w3` | At `c4e3a63` with uncommitted changes (`Cargo.toml`/`.lock`, `ti-bench` `gen.rs`/`model.rs`/`write.rs`, …) |
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
| M4 Text, geo, intervals | W4, W5, W6 | 6–8 | **In progress.** Item 1 met except 4 contract exclusions (58/0/4). Item 2 met (72.4×). **Item 3 (`croaring` evaluation) not started** |
| M5 Agent surface | W7 | 7–9 | **In progress.** Item 1 met (93/100). Item 2: scripted questions pass as a regression (20/20), but **the agent run is still open** (lead). Item 3: plugin and pgwire merged; Pi install and psql/Grafana smoke set open |
| M6 Fleet and benchmarks | W8, integrator | 9–12 | **In progress.** ti-sync and the lossy two-node test merged (`13c58c0`); HTTP transport and the 50-vessel shore test in flight; benchmark harness merged (`2159fa6`), report not written |

Also landed outside the original milestones: W9 generic Parquet (accepted) and W10 alerts (accepted). Remaining: `lume sql` (Long Horse), M6, the `croaring` evaluation and the Pi 5 deployment.

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
| 3 | `croaring` frozen-view evaluation written up in the decisions log, adopt or reject | **Open. Not started** |

### M5 gate detail

| # | Gate item | State |
|---|---|---|
| 1 | MCP tools live in `lume serve`, and `ti_resolve` returns the right column in the top 3 for ≥ 90 % of a 100-phrase test set | **✅ Met** (`39c0096`): 93/100 (fixture accuracy) |
| 2 | A nemesis8 agent with only Lume MCP answers 20 scripted fleet questions; integrator grades against oracle results | **Open.** The 20 questions exist (`tests/golden/fleet_questions.json`) and pass as a regression (20/20, `451bfc7`). The agent-only run and grading are still to do (lead) |
| 3 | Signal K plugin installs on a HALPI2 from the HaLOS Marine container store, and on OpenPlotter from the Signal K App Store (Pi 4 4 GB and Pi 5), obtains a token and supervises ingest. psql and Grafana pass a 20-query smoke set | **Open.** Plugin merged (`31841c3`) with access-request auth and supervision. pgwire merged (`451bfc7`; SCRAM/TLS pending). Pi 5 install is next. Store-based installs, Pi 4, OpenPlotter and the psql/Grafana smoke set are not done |

### M6 gate detail

| # | Gate item | State |
|---|---|---|
| 1 | Two-node sync converges with 20 % chunk loss and a 30-minute link outage | **In progress.** Passes in the in-process lossy two-node test (`13c58c0`). The rerun over the HTTP transport with bearer auth is in flight (Artificial Shark) |
| 2 | Shore node answers fleet queries across 50 synthetic vessels | **In progress** (Artificial Shark) |
| 3 | Benchmark report against every target; go/no-go in the decisions log | **Open.** W8 bench harness and `ti-bench --hz` merged (`2159fa6`); no report yet |

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

- [ ] **Pi 5 deployment** (lead): finish the native build, install `signalk-lume-ti` with the arm64 binary, then resume InfluxDB/Grafana/QuestDB/OpenCPN.
- [ ] **M5 item 2 agent run** (lead): a Lume-MCP-only agent answers the 20 scripted questions; grade against oracle results.
- [ ] **`lume sql`** and `lume ti query --docs-index` (Long Horse; `f711cfc` in the clone).
- [ ] **M6**: HTTP sync transport with bearer auth, 50-vessel shore fleet test, lossy test over HTTP (Artificial Shark). Then the benchmark report and the go/no-go.
- [ ] **`croaring` frozen-view evaluation** (M4 item 3).
- [ ] **`count_paths` contract** (`q1-007`, `q6-006`, `q2-001`; spec/05).
- [ ] **`qx-003`**: keep the join-form `qx-013`, or revisit on a DataFusion upgrade.
- [ ] pgwire SCRAM/TLS (D37 notes it as pending). Keep `--pg` on loopback until then.
- [ ] **User licence decision** on `signalk_paths.json` (user item 16).
- [ ] Meridian VHF transcripts and live notes polling (`GET /signalk/v2/api/resources/notes` every 60 s). Not done.
- [ ] Pi 5 throughput/RSS run for M2 item 3.
- [ ] `tests/golden/README.md` still describes the W10 rule as `for 5m`, but the oracle and test now use a 30 s hold (`6d51df5`). Owner to fix (outside docs-keeper scope).
- [x] W10 alerts accepted (118/118/118; `match(alerts,'battery')` 357 buckets).
- [x] W9 accepted (robots 14/14 DuckDB, verify 14/0/0; boat 65/65 hashes, 58/0/4; 174,769 rows/s).
- [x] Sealed-data repair (`b23514a`); D38 regression 65/65, 58/0/4 (`c4e3a63`).
- [x] Live ingest service, plugin, pgwire, ti-sync, D30 end to end.
- [ ] Verify [signalk-formats §1.3](design/signalk-formats.md) against a **real** signalk-parquet `.parquet` file with DuckDB `DESCRIBE`.
- [ ] Strict TI checks are not in `ci.yml` yet. Containers lacking rustfmt or clippy rely on the lead's host run.
- [ ] Toolchain drift between the host and the containers (item 15).

Also undecided: the repo-fit items in [repo-fit.md](repo-fit.md), such as the W7a/W7b split (§6). These are lead decisions unless escalated (unconfirmed).
