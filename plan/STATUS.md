# Lume TI — Status board

Last updated: **2026-10-06 08:08 UTC** (docs keeper, after `7dd6be4`: W4 part 2 merged, README rewrite)

Integration branch `plan/lume-ti` is at `7dd6be4`. **`ti-contracts` is frozen** (`96ac45d`). Root tests: 46.
Workspace members: `ti-contracts`, `ti-core`, `ti-store`, `ti-sql`, `ti-ingest`, `ti-bench`. Next free decision: **D31** (ask the lead before taking it).
Setup and workflow: [SETUP.md](SETUP.md).

## Critical path right now

- **Corpus expected outputs** (61 × `tests/golden/expected/*.json`) block M0 item 4 and M3. Artificial Shark is generating them with **DuckDB 1.5.6** in its container, on `ti/corpus-expected`.
- **The correctness set is READY.** It was regenerated on the host from `71b7fcd` (the fixed window): 1.8 GB, 11,508 files, in 2 min 31 s. Manifest sha256 `edfef2d8089e5fda112c1fac4340479981b7beb4372b45ed8bd31b72fa9d7c8e` is in `.lanes/data/correctness.sha256`. The earlier "do not use" warning is resolved.
- **M2 on the full set** is running (Artificial Shark), with a DuckDB cross-check of the Rust oracle. Smoke results were green.

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
| Industrial Pike | `ee764a09` | Lead and integrator. Owns the merged corpus lane and host data generation. Runs fmt and 1.96 clippy on the host for agents whose containers lack them | `plan/lume-ti` | shared tree | `7dd6be4` |
| Long Horse (formerly Rigid Roadrunner) | `888bff45` | **W4**, next step being chosen: (a) a **release-build Q4 benchmark at W = 10 s** (confirms M4's ≥ 10×), or (b) prep for **`lume ti verify --store` on the full set** plus an index-size measurement | `ti/w4-sql` | `.lanes/w4` | Part 2 merged (`8b89e48`). Clone at `7dd6be4`, clean tree. Container lacks rustfmt and clippy |
| Artificial Shark (formerly Romantic Pike) | `fd91f4b1` | **M2 on the FULL correctness set** plus a **DuckDB cross-check** of its Rust oracle. Then **corpus part 3** (61 expected outputs, DuckDB 1.5.6, `ti/corpus-expected`) | `ti/w3-ingest` (checked out), `ti/corpus-expected` | `.lanes/w3` | Uncommitted change to `crates/ti-ingest/tests/m2_gate.rs` on base `4c55f60` |
| Zygomorphic Prawn | `eccaf836` | — | — | `.lanes/corpus` (retired) | **Retired.** Work merged in `c65e515` |
| Compact Echidna (formerly Regular Pheasant) | `6914c38e` | Host build pane (PowerShell 7, rustc 1.96.1). Not an agent | — | — | Bulk data generation runs here. Don't start parallel cold `--features ti` builds |

How to refresh the last column: `git -C .lanes/<x> log --oneline -5` and `git -C .lanes/<x> status --short`.

## Milestones

Week numbers count from kickoff. A milestone closes only when every gate test passes in CI ([spec/12](spec/12-milestones.md)).

| Milestone | Lanes | Weeks | Gate status |
|---|---|---|---|
| M0 Contracts | W0 | 1 | **In progress.** 3/4 done. Item 4 waits on expected outputs |
| M1 Core and store | W1, W2 | 2–4 | **Complete** (`cf98c61`) |
| M2 Ingest | W3 | 2–5 | **In progress.** Harness merged and green on the smoke set. **Full-set run in progress** |
| M3 SQL and pushdown | W4 | 2–6 | **In progress, blocked** on corpus expected outputs |
| M4 Text, geo, intervals | W4, W5, W6 | 6–8 | **In progress.** W4 part 2 merged (`8b89e48`): `intervals()`, `BitmapAggregateExec` at 15.9× (provisional), `EXPLAIN` reasons. W5 and W6 not assigned. `croaring` evaluation not started |
| M5 Agent surface | W7 | 7–9 | Not started |
| M6 Fleet and benchmarks | W8, integrator | 9–12 | Not started |

After M3: the D30 high-resolution store ([design/hi-res-store.md](design/hi-res-store.md)) and `lume sql` over plain indexes.

### M0 gate detail

| # | Gate item | State |
|---|---|---|
| 1 | `ti-contracts` merged with types, traits, doc comments | **Done** (`96ac45d`) |
| 2 | `ti.toml` schema with defaults | **Done** (`96ac45d`) |
| 3 | `ti-bench gen` reproduces the correctness set byte-identically from a seed | **Done** (`c65e515`). Window fixed and pinned (`71b7fcd`) |
| 4 | ≥ 60 golden queries with oracle twins and expected output | In progress. 61 queries + exact oracles merged. Expected outputs: Artificial Shark, DuckDB 1.5.6 |

### M2 gate detail

| # | Gate item | State |
|---|---|---|
| 1 | Replaying a recorded 24 h delta log yields `BucketRecord`s equal to oracle bucketing | Smoke set green (153,374/153,374, independent Rust oracle). Full set and a DuckDB cross-check pending |
| 2 | Parquet backfill of the correctness set is idempotent | Smoke set green (manifest `be11a34b…` identical across runs). Full set pending |
| 3 | Pi 5 sustains 20,000 values/s for 1 h within the CPU and RSS budget | 181,783 values/s in release on the **host (x86), not a Pi 5**. RSS is unmeasured on non-Linux (reads 0.00 MB). A Pi 5 run is still needed (unconfirmed whether that's planned) |

### M3 gate detail

| # | Gate item | State |
|---|---|---|
| 1 | All non-text, non-geo golden queries match the oracle (fixture, then real store) | Open. Waits on expected outputs |
| 2 | `EXPLAIN` shows Exact pushdown for every expression marked Exact | Open (unconfirmed whether this gate has been checked) |
| 3 | `raw` table queries the same Parquet and matches DuckDB exactly | Open. Data ready, and DuckDB is available in Artificial Shark's container |

### M4 gate detail

| # | Gate item | State |
|---|---|---|
| 1 | Full golden corpus green, including `match()`, `in_bbox`, `within_nm` and `intervals()` | Open. `intervals()` implemented (`8b89e48`). `match()` (W5) and geo (W6) not started. Expected outputs not done |
| 2 | `BitmapAggregateExec` ≥ 10× faster than the materializing path on Q4 at shore scale | **Provisionally met**: 15.9× (debug, W = 60 s, 50 vessel-years, identical results). Release build at W = 10 s still pending |
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
- [ ] **M2 harness follow-ups (Artificial Shark):** stop vacuous passes without data (`#[ignore]`), fix the RSS reading on non-Linux, and do a one-off DuckDB cross-check.
- [ ] M2 on the full correctness set, and a Pi 5 throughput/RSS run.
- [ ] Corpus expected outputs (Artificial Shark, `ti/corpus-expected`). That branch predates `71b7fcd`, so it should be rebased or merged before its outputs are trusted (unconfirmed whether that matters for the JSON outputs).
- [ ] Verify [signalk-formats §1.3](design/signalk-formats.md) against a **real** signalk-parquet `.parquet` file with DuckDB `DESCRIBE`. Generator output doesn't count.
- [ ] Long Horse's container has no rustfmt or clippy. The lead runs both on the host at merge (as in `c92c325`).
- [ ] Strict TI checks are not in `ci.yml` yet.
- [ ] Toolchain drift between the host and the containers (item 15).

Also undecided: the repo-fit items in [repo-fit.md](repo-fit.md), such as the W7a/W7b split (§6). These are lead decisions unless escalated (unconfirmed).
