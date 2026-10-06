# Lume TI — Status board

Last updated: **2026-10-06 08:44 UTC** (docs keeper, after `fe5ea3a`: D30 multi-store config merged; expected outputs generated)

Integration branch `plan/lume-ti` is at `fe5ea3a`. **`ti-contracts` is frozen** (`96ac45d`). Root tests: 46.
Workspace members: `ti-contracts`, `ti-core`, `ti-store`, `ti-sql`, `ti-ingest`, `ti-bench`. Next free decision: **D32** (ask the lead before taking it).
Setup and workflow: [SETUP.md](SETUP.md).

## Critical path right now

- **Expected outputs: generated, being corrected.** `gen_expected.py` ran on the host in **3 min 48 s** (about 2 h was projected in the container) and wrote 61 JSONs to `.lanes/data/expected/`. They aren't committed yet. **16 are zero-row:** `q1-007`, `q2-001`, `q2-002`, `q3-002`, `q5-002`, `q5-003`, `q6-001`, `q6-003`, `q6-005`, `q7-001`…`q7-006`, `qx-006`. Artificial Shark is adjusting query literals to the generated data (frozen at `edfef2d8…`), marking legitimately empty queries `expect_empty` and listing exclusions. Long Horse found that `q7-006` lacks `GROUP BY vessel`. Next, the lead reruns on the host and Shark commits on `ti/corpus-expected`. **M0 item 4 and M3 wait on this.**
- **Full-set M2 backfill (host, release) is still running.** About 50 min in at 08:44, steady at ~80 MB RSS.
- Queued on the host: the release Q4 benchmark at W = 10 s (5 vessels × 365 d), to confirm M4 item 2.
- **The correctness set is READY** (1.8 GB, 11,508 files, manifest `edfef2d8…` in `.lanes/data/correctness.sha256`).

## Pane changes (about 06:00 UTC)

| Role | Now | Formerly |
|---|---|---|
| W4 SQL owner | **Long Horse** `888bff45`, nemesis8/n8-hazy-badger | Rigid Roadrunner `d58ca1b1`, n8-sly-viper |
| W3 ingest + corpus part 3 owner | **Artificial Shark** `fd91f4b1`, nemesis8/n8-quiet-crane | Romantic Pike `90fc608c`, n8-keen-kiwi |
| Corpus and generator | Lead took it over and merged it (`c65e515`) | Zygomorphic Prawn `eccaf836`, n8-noble-toad: **retired** (permanently offline, per the user) |
| Host build pane | **Compact Echidna** `6914c38e` | Regular Pheasant `364a3fc7` (gone) |

Commit messages, the decisions log and older docs use the former names.

## Recent merges into `plan/lume-ti`

| Commit | What |
|---|---|
| `fe5ea3a` | Merge `ti/d30-config` `cf6602f`: the **D30 `[stores.*]` multi-store config contract** (a lead-approved contracts PR). Typed width/retention (`s/m/h/d`, `"forever"`), validated aggs and paths, a required `"default"` store, width must divide 3600, per-store `resolved_root` with no duplicates, and empty `stores` = legacy single store. Host: 28 `ti-contracts` tests, 1.96 clippy, fmt, 46 root tests, `--features ti` check. Follow-ups from Shark: the spec/10 mirror for `config.rs`, spec/14 multi-store rules, and a D30 amendment line |
| `8afa646` | Docs refresh, plus the **D14 amendment**: DuckDB as an out-of-process oracle (CLI or the `duckdb` Python package 1.5.6) |
| `b4e8da6` | Merge `ti/w4-sql` `8e4fdd0`: the Q4 benchmark is configurable through the `TI_Q4_WIDTH_SECONDS`, `TI_Q4_VESSELS`, `TI_Q4_DAYS` and `TI_Q4_RUNS` env vars. Host-verified: fmt and 1.96 clippy clean for `ti-ingest` and `ti-sql` |
| `2c8ba1c` | Merge `ti/w3-ingest` `5605127`: the M2 data tests are now `#[ignore = "needs TI_DATA_DIR"]` and **fail fast** if the dir is missing. The replay day is auto-detected (`day=060`) or set with `TI_REPLAY_DAY`. RSS shows "n/a" on non-Linux. **DuckDB cross-check on `day=060`: 460,112/460,112 matched** against the Rust oracle |
| `c88fb75` | Merge `ti/corpus-expected` `895b085`: `raw_view.sql` now uses DuckDB **TABLE MACROs** `read_raw`/`read_docs`. The previous `CREATE FUNCTION` form **never parsed in DuckDB**, a defect the lead found once DuckDB was on the host. Adds `tests/golden/gen_expected.py` |
| `6c29ea2` | D31 reserved for W6 (`h3o` 0.11; geo also approved, pinned to h3o's version). Next free: **D32** |
| `d93d547` | Docs refresh for W4 part 2, the lint fixes, the README rewrite and the M4 gate table |

Earlier: `7dd6be4` root README rewrite (MCP port 5863), `c92c325` + `8b89e48` W4 part 2 (`intervals()`, `BitmapAggregateExec`), `4c55f60` M2 harness, `571cc2b` D30, `71b7fcd` window fix, `c65e515` corpus lane merged by the lead, `33c67b1` store fix + SQL Store adapter, `46d69f4` W3 foundation, `98816b5` W4 foundation, `cf98c61` W2 store (**M1 complete**), `922fc07` D27, `f7faf5f` W1 core, `1297968` search library, `96ac45d` contracts freeze, `b0ea0e1` [signalk-formats](design/signalk-formats.md), `d8b2a88` corpus part 1, `06dd5c5` workspace. Docs refreshes omitted. Full list: `git log --oneline --first-parent plan/lume-ti`.

## Agents

| Agent (pane name) | Pane | Lane/scope | Branch | Clone | Last known state |
|---|---|---|---|---|---|
| Industrial Pike | `ee764a09` | Lead and integrator. Owns the merged corpus lane and the host runs (M2 full-set backfill, `gen_expected.py`, release Q4 bench). Runs fmt and 1.96 clippy on the host for agents whose containers lack them | `plan/lume-ti` | shared tree | `b4e8da6` |
| Long Horse (formerly Rigid Roadrunner) | `888bff45` | **W6 geo** ([lanes/W6](lanes/W6-geo.md)), D31: Inexact `OR(GeoCover, BSI lat/lon envelope)` pushdown, then an exact residual. **W5 design approved** (below) | `ti/w6-geo` | `.lanes/w4` (same clone as W4) | **Green so far:** H3 Covers proptest at 600 cases, res-9 false-positive rate **16.4 %** (target ≤ 30 %), and the bbox/radius/NOT/antimeridian/pole/zero-radius/legacy-missing-cell tests pass. `geo` 0.33.1 is exact-pinned with `h3o` 0.11 (MIT OR Apache-2.0). Real H3 cells in ingest will be a **separate ingest commit**, which explains the `ti-ingest` edits in its tree. Uncommitted, on base `8afa646`: `ti-geo`, `ti-sql` geo, `ti-ingest`, root `Cargo.*`, spec/11 and spec/14 |
| Artificial Shark (formerly Romantic Pike) | `fd91f4b1` | **Corpus expected outputs**: literal adjustments, `expect_empty`, exclusions, the `q7-006` fix. Then the **D30 follow-ups** (spec/10 mirror, spec/14 multi-store, D30 amendment) and the **ingest fan-out** | `ti/corpus-expected` | `.lanes/w3` | D30 config merged (`fe5ea3a`). Checked out on `ti/corpus-expected` at `b4e8da6`, with an untracked `tests/golden/expected/` |
| Zygomorphic Prawn | `eccaf836` | — | — | `.lanes/corpus` (retired) | **Retired.** Work merged in `c65e515` |
| Compact Echidna (formerly Regular Pheasant) | `6914c38e` | Host build pane (PowerShell 7, rustc 1.96.1). Not an agent | — | — | Bulk data generation runs here. Don't start parallel cold `--features ti` builds |

How to refresh the last column: `git -C .lanes/<x> log --oneline -5` and `git -C .lanes/<x> status --short`.

## Milestones

Week numbers count from kickoff. A milestone closes only when every gate test passes in CI ([spec/12](spec/12-milestones.md)).

| Milestone | Lanes | Weeks | Gate status |
|---|---|---|---|
| M0 Contracts | W0 | 1 | **In progress.** 3/4 done. Item 4: 61 JSONs generated, 16 zero-row being corrected |
| M1 Core and store | W1, W2 | 2–4 | **Complete** (`cf98c61`) |
| M2 Ingest | W3 | 2–5 | **In progress.** Replay oracle cross-checked with DuckDB (460,112/460,112). Full-set backfill idempotence running on the host |
| M3 SQL and pushdown | W4 | 2–6 | **In progress, blocked** on the corrected expected outputs |
| M4 Text, geo, intervals | W4, W5, W6 | 6–8 | **In progress.** `intervals()` and `BitmapAggregateExec` merged (15.9× provisional). **W6 geo green so far** (uncommitted). W5 design approved, owner not assigned. `croaring` evaluation not started |
| M5 Agent surface | W7 | 7–9 | Not started |
| M6 Fleet and benchmarks | W8, integrator | 9–12 | Not started |

After M3: the D30 high-resolution store ([design/hi-res-store.md](design/hi-res-store.md)) and `lume sql` over plain indexes.

### M0 gate detail

| # | Gate item | State |
|---|---|---|
| 1 | `ti-contracts` merged with types, traits, doc comments | **Done** (`96ac45d`) |
| 2 | `ti.toml` schema with defaults | **Done** (`96ac45d`) |
| 3 | `ti-bench gen` reproduces the correctness set byte-identically from a seed | **Done** (`c65e515`). Window fixed and pinned (`71b7fcd`) |
| 4 | ≥ 60 golden queries with oracle twins and expected output | In progress. 61 JSONs generated on the host (3 min 48 s), **16 zero-row** being fixed (literals, `expect_empty`, exclusions, `q7-006` `GROUP BY`). Then a host rerun and a commit on `ti/corpus-expected` |

### M2 gate detail

| # | Gate item | State |
|---|---|---|
| 1 | Replaying a recorded 24 h delta log yields `BucketRecord`s equal to oracle bucketing | Rust oracle matched on the smoke set (153,374/153,374), and the **DuckDB cross-check on `day=060` matched 460,112/460,112** (`2c8ba1c`) |
| 2 | Parquet backfill of the correctness set is idempotent | Smoke set green. **Full-set release run in progress on the host** (~50 min at 08:44, ~80 MB RSS) |
| 3 | Pi 5 sustains 20,000 values/s for 1 h within the CPU and RSS budget | 181,783 values/s in release on the **host (x86), not a Pi 5**. RSS shows "n/a" on non-Linux. A Pi 5 run is still needed (unconfirmed whether planned) |

### M3 gate detail

| # | Gate item | State |
|---|---|---|
| 1 | All non-text, non-geo golden queries match the oracle (fixture, then real store) | Open. Waits on expected outputs |
| 2 | `EXPLAIN` shows Exact pushdown for every expression marked Exact | Open (unconfirmed whether this gate has been checked) |
| 3 | `raw` table queries the same Parquet and matches DuckDB exactly | Open. Data ready, and DuckDB is available in Artificial Shark's container |

### M4 gate detail

| # | Gate item | State |
|---|---|---|
| 1 | Full golden corpus green, including `match()`, `in_bbox`, `within_nm` and `intervals()` | Open. `intervals()` done. Geo green in W6 (uncommitted). `match()` (W5) design approved, not started. Corpus expected outputs still being corrected |
| 2 | `BitmapAggregateExec` ≥ 10× faster than the materializing path on Q4 at shore scale | **Provisionally met**: 15.9× (debug, W = 60 s). Release W = 10 s run (5 vessels × 365 d) queued on the host |
| 3 | `croaring` frozen-view evaluation written up in the decisions log, adopt or reject | Not started |

**W5 text design (approved):** `ti-text` owns docs, the mapping and an LRU behind an object-safe `LexicalBackend`. The root `src/ti_text.rs` wires in `lume::search` `SearchMode::LexicalOnly`, so there's no dependency cycle. Owner not yet assigned (unconfirmed).

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

9. Boat computer: exact HALPI model, CM5 RAM size, OS image (HaLOS Marine or desktop + OpenCPN), and whether OpenCPN runs on the same HALPI.
10. Final NMEA 2000 equipment list: MFD brand, instrument vendor, engine gateway.
11. Whether Victron stays on 2.0, and whether a Cerbo GX or Venus OS device is aboard.
12. Consent to record 90 days of data for the benchmark dataset, and what may be shared publicly.
13. Satellite link (Starlink or Viasat) and the data budget `ti-sync` may use.

Spec deviations and proposals needing the user:

14. **D27: accept `zstd-sys` (C)**, forced in by DataFusion 55.1.0's `arrow-ipc`. Amends D11 and deviates from spec/10 (C bindings only for `croaring`). Awaiting spec-owner confirmation.
15. **Pinned `rust-toolchain.toml`**, proposed because the host (rustc 1.96.1) and the containers (1.99) report different clippy lints.

## Open items (team, not user)

- [x] `ti-bench` window mismatch. Fixed in `71b7fcd`, with a pin test.
- [x] Full correctness set on the host, with `.lanes/data/correctness.sha256` (`edfef2d8…`). Ready.
- [x] M2 harness follow-ups: `#[ignore]` + fail-fast without data, RSS "n/a" on non-Linux, DuckDB cross-check. Merged `2c8ba1c`.
- [ ] M2 full-set backfill idempotence (host run in progress), and a Pi 5 throughput/RSS run.
- [ ] Corpus expected outputs: fix the 16 zero-row queries, rerun on the host, and commit on `ti/corpus-expected` (Artificial Shark).
- [x] ~~Check the `ti-ingest` edits in `.lanes/w4`.~~ They are for real H3 cells in ingest, and will be a separate commit (per the lead).
- [ ] D30 follow-ups (Artificial Shark): spec/10 mirror for `config.rs`, spec/14 multi-store rules, D30 amendment line.
- [ ] Verify [signalk-formats §1.3](design/signalk-formats.md) against a **real** signalk-parquet `.parquet` file with DuckDB `DESCRIBE`. Generator output doesn't count.
- [ ] Long Horse's container has no rustfmt or clippy. The lead runs both on the host at merge (as in `c92c325`).
- [ ] Strict TI checks are not in `ci.yml` yet.
- [ ] Toolchain drift between the host and the containers (item 15).

Also undecided: the repo-fit items in [repo-fit.md](repo-fit.md), such as the W7a/W7b split (§6). These are lead decisions unless escalated (unconfirmed).
