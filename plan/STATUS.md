# Lume TI — Status board

Last updated: **2026-10-06 08:28 UTC** (docs keeper, after `b4e8da6`: corpus-expected, W3 and W4 merges; host runs in progress)

Integration branch `plan/lume-ti` is at `b4e8da6`. **`ti-contracts` is frozen** (`96ac45d`). Root tests: 46.
Workspace members: `ti-contracts`, `ti-core`, `ti-store`, `ti-sql`, `ti-ingest`, `ti-bench`. Next free decision: **D32** (ask the lead before taking it).
Setup and workflow: [SETUP.md](SETUP.md).

## Critical path right now

- **Host runs in progress (by the lead, on Compact Echidna):**
  1. Full-set M2 **backfill idempotence** in release on `.lanes/data/correctness`.
  2. `gen_expected.py` writing the 61 expected JSONs to `.lanes/data/expected/`. Artificial Shark will commit them. At 08:28 the directory was still empty and the log showed DuckDB setting up `raw`. The host does this run because container DuckDB through the bind mount took 126 s per query.
  3. Queued: the **release Q4 benchmark at W = 10 s** (5 vessels × 365 days), to confirm M4 item 2.
- **The expected outputs block M0 item 4 and M3.** Once the JSONs are committed, M0 can close.
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
| `b4e8da6` | Merge `ti/w4-sql` `8e4fdd0`: the Q4 benchmark is configurable through the `TI_Q4_WIDTH_SECONDS`, `TI_Q4_VESSELS`, `TI_Q4_DAYS` and `TI_Q4_RUNS` env vars. Host-verified: fmt and 1.96 clippy clean for `ti-ingest` and `ti-sql` |
| `2c8ba1c` | Merge `ti/w3-ingest` `5605127`: the M2 data tests are now `#[ignore = "needs TI_DATA_DIR"]` and **fail fast** if the dir is missing. The replay day is auto-detected (`day=060`) or set with `TI_REPLAY_DAY`. RSS shows "n/a" on non-Linux. **DuckDB cross-check on `day=060`: 460,112/460,112 matched** against the Rust oracle |
| `c88fb75` | Merge `ti/corpus-expected` `895b085`: `raw_view.sql` now uses DuckDB **TABLE MACROs** `read_raw`/`read_docs`. The previous `CREATE FUNCTION` form **never parsed in DuckDB**, a defect the lead found once DuckDB was on the host. Adds `tests/golden/gen_expected.py` |
| `6c29ea2` | D31 reserved for W6 (`h3o` 0.11; geo also approved, pinned to h3o's version). Next free: **D32** |
| `d93d547` | Docs refresh for W4 part 2, the lint fixes, the README rewrite and the M4 gate table |
| `7dd6be4`, `6af4a75` | **Root `README.md` rewritten** as a professional DeepBlue Dynamics front page: features, a CLI table, a Lume TI section with measured write and query benchmarks, and Credits (Steve Harris). The backstory was dropped. It also fixes the MCP default port in the docs (**5863**, not 8080). No `plan/` changes |
| `c92c325` | Lead fixes for W4 part 2, because Long Horse's container lacks rustfmt and clippy: rustfmt plus rustc 1.96 clippy (`collapsible_if`, useless `into_iter`, 2× `is_multiple_of`). `ti-sql` strict clippy and fmt are clean on the host, and all tests pass |
| `8b89e48` | Merge **W4 part 2** (`2f6086c`): the `intervals()` table function, `BitmapAggregateExec`, and `EXPLAIN` chosen/fallback reasons. 20 `ti-sql` tests. Benchmark: Q4 max wind per day over 50 vessel-years (W = 60 s, **debug**) has a bitmap median of **3.01 s vs 47.87 s** materialized, **15.9×** (target ≥ 10×), with identical results. That's an early signal, not the M4 gate |
| `d1e1080` | Docs refresh for the window fix, data ready, D30 and the M2 harness |
| `4c55f60` | Merge the **W3 M2 harness** (`af9cc7c`, `crates/ti-ingest/tests/m2_gate.rs`, with an independent Rust oracle). The lead re-ran it on the host against `w3-smoke` with `TI_DATA_DIR`. Results: **153,374/153,374** records matched, the backfill manifest hash `be11a34b…` was identical across runs, and **181,783 values/s** in release. Known issues went back to Artificial Shark: tests pass **vacuously** when data is absent (fix: `#[ignore]`), peak RSS reads 0.00 MB on non-Linux, and a one-off DuckDB cross-check is requested |
| `6ee96b9` | Docs refresh for the corpus takeover, `ti-bench` usage and the window issue |
| `571cc2b` | **D30**: user-approved 1 s high-resolution store `telemetry_hr` (navigation `@last`, wind `@mean`+`@max`, depth `@min`). Retention is configurable per store and defaults to 90 days, with a separate `shore_retention`. Design: [design/hi-res-store.md](design/hi-res-store.md). Scheduled **after M3**. It resolves spec/11's bucket-width question. Next free: D31 |
| `71b7fcd` | **`ti-bench` window fix**: START/END are now 2026-03-01/06-01 UTC (they were Feb 10 16:00 / May 12 16:00). Adds the `tests/window.rs` pin test, logs D22 properly, and removes the stray `.lanes/data/ti-bench-days-flags.patch` |
| `c65e515` | Corpus lane merged by the lead: `ti-bench` generator, `--days` flags, exact oracles, `raw_view.sql`, review fixes. **M0 item 3 done** |
| `33c67b1` | Merge `ti/w4-sql`: `ti-store` list/sourceRef fix + durable SQL Store adapter |
| `46d69f4` | Merge the `ti/w3-ingest` foundation |
| `98816b5` | Merge the `ti/w4-sql` foundation (DataFusion `=55.1.0`) |

Earlier: `cf98c61` W2 store (**M1 complete**), `922fc07` D27, `f7faf5f` W1 core, `1297968` search library, `96ac45d` contracts freeze, `b0ea0e1` [signalk-formats](design/signalk-formats.md), `d8b2a88` corpus part 1, `06dd5c5` workspace. Full list: `git log --oneline --first-parent plan/lume-ti`.

## Agents

| Agent (pane name) | Pane | Lane/scope | Branch | Clone | Last known state |
|---|---|---|---|---|---|
| Industrial Pike | `ee764a09` | Lead and integrator. Owns the merged corpus lane and the host runs (M2 full-set backfill, `gen_expected.py`, release Q4 bench). Runs fmt and 1.96 clippy on the host for agents whose containers lack them | `plan/lume-ti` | shared tree | `b4e8da6` |
| Long Horse (formerly Rigid Roadrunner) | `888bff45` | **W6 geo** ([lanes/W6](lanes/W6-geo.md)), D31 `h3o`. Approved design: Inexact pushdown = `OR(GeoCover, BSI lat/lon envelope)`, then an exact residual | `ti/w6-geo` | `.lanes/w4` (same clone as W4) | `ti/w6-geo` is at `6c29ea2`, no commits yet. Uncommitted work in progress: new `crates/ti-geo/`, `ti-sql/src/geo.rs` and `tests/geo.rs`, plus changes to `ti-sql` (`classifier.rs`, `lib.rs`, `session.rs`, `Cargo.toml`), root `Cargo.toml`/`Cargo.lock`, **and `ti-ingest`** (`Cargo.toml`, `parquet.rs`, `watermark.rs`, `tests/m2_gate.rs`) |
| Artificial Shark (formerly Romantic Pike) | `fd91f4b1` | **D30 config contracts PR** on `ti/d30-config`, approved with changes: typed width/retention, validated aggs, a required `"default"` store when stores are present, width must divide 3600, per-store root. Then the **ingest fan-out**. Also commits the expected JSONs when the host run finishes | `ti/d30-config` | `.lanes/w3` | Clone is checked out on `ti/corpus-expected` (`895b085`, merged), clean. `ti/d30-config` doesn't exist in the clone yet |
| Zygomorphic Prawn | `eccaf836` | — | — | `.lanes/corpus` (retired) | **Retired.** Work merged in `c65e515` |
| Compact Echidna (formerly Regular Pheasant) | `6914c38e` | Host build pane (PowerShell 7, rustc 1.96.1). Not an agent | — | — | Bulk data generation runs here. Don't start parallel cold `--features ti` builds |

How to refresh the last column: `git -C .lanes/<x> log --oneline -5` and `git -C .lanes/<x> status --short`.

## Milestones

Week numbers count from kickoff. A milestone closes only when every gate test passes in CI ([spec/12](spec/12-milestones.md)).

| Milestone | Lanes | Weeks | Gate status |
|---|---|---|---|
| M0 Contracts | W0 | 1 | **In progress.** 3/4 done. Item 4 waits on expected outputs |
| M1 Core and store | W1, W2 | 2–4 | **Complete** (`cf98c61`) |
| M2 Ingest | W3 | 2–5 | **In progress.** Replay oracle cross-checked with DuckDB (460,112/460,112). Full-set backfill idempotence running on the host |
| M3 SQL and pushdown | W4 | 2–6 | **In progress, blocked** on expected outputs (being generated on the host) |
| M4 Text, geo, intervals | W4, W5, W6 | 6–8 | **In progress.** `intervals()` and `BitmapAggregateExec` merged (15.9× provisional). **W6 geo started** (Long Horse). W5 not assigned. `croaring` evaluation not started |
| M5 Agent surface | W7 | 7–9 | Not started |
| M6 Fleet and benchmarks | W8, integrator | 9–12 | Not started |

After M3: the D30 high-resolution store ([design/hi-res-store.md](design/hi-res-store.md)) and `lume sql` over plain indexes.

### M0 gate detail

| # | Gate item | State |
|---|---|---|
| 1 | `ti-contracts` merged with types, traits, doc comments | **Done** (`96ac45d`) |
| 2 | `ti.toml` schema with defaults | **Done** (`96ac45d`) |
| 3 | `ti-bench gen` reproduces the correctness set byte-identically from a seed | **Done** (`c65e515`). Window fixed and pinned (`71b7fcd`) |
| 4 | ≥ 60 golden queries with oracle twins and expected output | In progress. 61 queries + exact oracles merged. `raw_view.sql` fixed to DuckDB table macros (`c88fb75`). Expected JSONs being generated on the host into `.lanes/data/expected/`, then committed by Artificial Shark |

### M2 gate detail

| # | Gate item | State |
|---|---|---|
| 1 | Replaying a recorded 24 h delta log yields `BucketRecord`s equal to oracle bucketing | Rust oracle matched on the smoke set (153,374/153,374), and the **DuckDB cross-check on `day=060` matched 460,112/460,112** (`2c8ba1c`) |
| 2 | Parquet backfill of the correctness set is idempotent | Smoke set green. **Full-set release run in progress on the host** |
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
| 1 | Full golden corpus green, including `match()`, `in_bbox`, `within_nm` and `intervals()` | Open. `intervals()` done (`8b89e48`). Geo in progress (W6, Long Horse). `match()` (W5) not started |
| 2 | `BitmapAggregateExec` ≥ 10× faster than the materializing path on Q4 at shore scale | **Provisionally met**: 15.9× (debug, W = 60 s). Release W = 10 s run (5 vessels × 365 d) queued on the host |
| 3 | `croaring` frozen-view evaluation written up in the decisions log, adopt or reject | Not started |

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
- [ ] Commit the 61 expected JSONs (Artificial Shark) from the host run in `.lanes/data/expected/`.
- [ ] **Check `.lanes/w4`:** the W6 working tree has uncommitted changes in `crates/ti-ingest/` (W3's crate). Confirm they are intended before the W6 commit.
- [ ] Verify [signalk-formats §1.3](design/signalk-formats.md) against a **real** signalk-parquet `.parquet` file with DuckDB `DESCRIBE`. Generator output doesn't count.
- [ ] Long Horse's container has no rustfmt or clippy. The lead runs both on the host at merge (as in `c92c325`).
- [ ] Strict TI checks are not in `ci.yml` yet.
- [ ] Toolchain drift between the host and the containers (item 15).

Also undecided: the repo-fit items in [repo-fit.md](repo-fit.md), such as the W7a/W7b split (§6). These are lead decisions unless escalated (unconfirmed).
