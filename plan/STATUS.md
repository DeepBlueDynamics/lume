# Lume TI — Status board

Last updated: **2026-10-06 09:07 UTC** (docs keeper, after `19ab9fb`: corpus calibrated, D30 follow-ups, W6 geo merged; disk incident)

Integration branch `plan/lume-ti` is at `19ab9fb`. **`ti-contracts` is frozen** (`96ac45d`). Root tests: 46.
Workspace members: `ti-contracts`, `ti-core`, `ti-store`, `ti-sql`, `ti-ingest`, `ti-bench`, `ti-geo`. Next free decision: **D32** (ask the lead before taking it).
Setup and workflow: [SETUP.md](SETUP.md).

## Critical path right now

- **⚠ Disk: host C: is nearly full.** It hit 0.1 GB free, and the full-set M2 backfill-idempotence run **failed after 43 min with `StorageFull`**, because the test builds two full Stores in temp. The lead freed 13.6 GB by deleting retired clones' `target/` (w0, w1, w2, search, corpus) and the root `target/debug/incremental`. **About 13 GB free at 09:07** (99 % used). Remaining big users: `.lanes/w4/target` 48.6 GB, `.lanes/w3/target` 12.9 GB, root `target/debug` 32 GB. Agents have been told to shrink theirs and use `CARGO_INCREMENTAL=0`. See the Disk budget section in [SETUP §8](SETUP.md).
- **Expected outputs: final set generated, commit pending.** The corpus literals are calibrated (`0fe2960`). The host rerun of `gen_expected.py` took 5 min 3 s and produced 61 JSONs. Only `qx-006` is zero-row, and it is intentionally `expect_empty`. That gives **60 non-empty golden queries + 1 `expect_empty`.** Artificial Shark is copying them into `tests/golden/expected/` on `ti/corpus-expected`. **Committing them closes M0 item 4 and unblocks M3.**
- **M2 item 2 (full-set backfill idempotence) is NOT passed.** Shark is reworking the test to build one store at a time and report the store size. The index size is still unmeasured.
- Queued on the host: the release Q4 benchmark at W = 10 s (5 vessels × 365 d), to confirm M4 item 2. Mind the free disk.
- **The correctness set is READY** (1.8 GB, 11,508 files, manifest `edfef2d8…` in `.lanes/data/correctness.sha256`).

## Pane changes (about 06:00 UTC)

| Role | Now | Formerly |
|---|---|---|
| W4/W6/W5 SQL owner | **Long Horse** `888bff45`, nemesis8/n8-hazy-badger | Rigid Roadrunner `d58ca1b1`, n8-sly-viper |
| W3 ingest + corpus part 3 owner | **Artificial Shark** `fd91f4b1`, nemesis8/n8-quiet-crane | Romantic Pike `90fc608c`, n8-keen-kiwi |
| Corpus and generator | Lead took it over and merged it (`c65e515`) | Zygomorphic Prawn `eccaf836`, n8-noble-toad: **retired** (permanently offline, per the user) |
| Host build pane | **Compact Echidna** `6914c38e` | Regular Pheasant `364a3fc7` (gone) |

Commit messages, the decisions log and older docs use the former names.

## Recent merges into `plan/lume-ti`

| Commit | What |
|---|---|
| `19ab9fb` | Lead rustfmt for W6 (`ti-geo`, `ti-ingest`, `ti-sql`), because the container lacks rustfmt. Host: rustc 1.96 clippy clean with no fixes needed, tests pass, 46 root tests |
| `8931877` | Merge **W6 geo**. `54da5d6`: `crates/ti-geo` (`h3o` 0.11 + `geo` 0.33.1 under D31), conservative Covers, typed `in_bbox`/`within_nm`, and Inexact `OR(H3, BSI envelope)` pushdown plus an exact residual. `e73ea12`: real H3 cells at res 5/7/9 in live and backfill ingest. Real-data M2 replay with real cells: **460,112/460,112**. Res-9 false-positive rate **16.4 %** (target ≤ 30 %). The Q7 corpus gate is implemented, pending the committed expected outputs. `q7-005` waits on W5 |
| `47b2515` | Merge `ti/d30-followups` (`e20ebf5`): spec/10 mirror of `config.rs`, spec/14 multi-store rules, the D30 amendment |
| `0fe2960` | Merge `ti/corpus-expected` (`a64e27c`, incl. `6863eff`): **corpus literals calibrated** for all 16 zero-row queries. Geo points and bboxes moved onto the real tracks (36.619, −122.389), and text keywords and thresholds moved into the data ranges. `qx-006` is `expect_empty`. `GROUP BY vessel` added to `q3-001`…`q3-007`, `q7-006` and `q8-005` `ti_sql` |
| `fe5ea3a` | Merge `ti/d30-config`: the D30 `[stores.*]` multi-store config contract (contracts PR) |
| `8afa646` | D14 amendment: DuckDB via the CLI or the Python package |

Earlier: `b4e8da6` Q4 bench env vars, `2c8ba1c` M2 `#[ignore]` + DuckDB cross-check, `c88fb75` DuckDB table macros + `gen_expected.py`, `6c29ea2` D31, `7dd6be4` root README rewrite (MCP port 5863), `8b89e48` W4 part 2, `4c55f60` M2 harness, `571cc2b` D30, `71b7fcd` window fix, `c65e515` corpus lane, `33c67b1` store fix + SQL Store adapter, `46d69f4` W3 foundation, `98816b5` W4 foundation, `cf98c61` W2 (**M1 complete**), `922fc07` D27, `f7faf5f` W1, `1297968` search library, `96ac45d` contracts freeze, `06dd5c5` workspace. Docs refreshes omitted. Full list: `git log --oneline --first-parent plan/lume-ti`.

## Agents

| Agent (pane name) | Pane | Lane/scope | Branch | Clone | Last known state |
|---|---|---|---|---|---|
| Industrial Pike | `ee764a09` | Lead and integrator. Owns the host runs (data generation, `gen_expected.py`, full-set M2, Q4 bench) and disk cleanup. Runs fmt and 1.96 clippy on the host when a container lacks them | `plan/lume-ti` | shared tree | `19ab9fb` |
| Long Horse (formerly Rigid Roadrunner) | `888bff45` | **W5 text** next, on a new branch. Approved design: `ti-text` with an object-safe `LexicalBackend`, and root `src/ti_text.rs` wiring `lume::search` `LexicalOnly`. It reuses `serde`/`serde_json` 1 and `arrow-array` 59.2, so **no new D-number** is needed | W5 branch (not created yet) | `.lanes/w4` | W6 merged (`8931877`). Clone is on `ti/w6-geo` at `e73ea12`, clean. Its `target/` is **48.6 GB**, and it has been asked to shrink it |
| Artificial Shark (formerly Romantic Pike) | `fd91f4b1` | **Commit the expected JSONs** on `ti/corpus-expected`. **Rework M2 idempotence** to build one store at a time and report the store size. Then the D30 ingest fan-out | `ti/corpus-expected` | `.lanes/w3` | On `ti/corpus-expected` at `a64e27c` (merged), with an untracked `tests/golden/expected/`. Its `target/` is **12.9 GB** |
| Zygomorphic Prawn | `eccaf836` | — | — | `.lanes/corpus` (retired) | **Retired.** Its `target/` has been deleted |
| Compact Echidna (formerly Regular Pheasant) | `6914c38e` | Host build pane (PowerShell 7, rustc 1.96.1). Not an agent | — | — | Bulk data and DuckDB jobs run here. **Check free disk before big builds** |

How to refresh the last column: `git -C .lanes/<x> log --oneline -5` and `git -C .lanes/<x> status --short`.

## Milestones

Week numbers count from kickoff. A milestone closes only when every gate test passes in CI ([spec/12](spec/12-milestones.md)).

| Milestone | Lanes | Weeks | Gate status |
|---|---|---|---|
| M0 Contracts | W0 | 1 | **In progress.** 3/4 done. Item 4 needs only the expected-JSON commit |
| M1 Core and store | W1, W2 | 2–4 | **Complete** (`cf98c61`) |
| M2 Ingest | W3 | 2–5 | **In progress.** Replay done on real data (460,112/460,112 with real H3 cells). Full-set idempotence **failed on disk space**, and the test is being reworked |
| M3 SQL and pushdown | W4 | 2–6 | **In progress, blocked** on the committed expected outputs |
| M4 Text, geo, intervals | W4, W5, W6 | 6–8 | **In progress.** Geo and `intervals()` done (corpus run pending). `BitmapAggregateExec` 15.9× provisional. **W5 text next** (Long Horse). `croaring` evaluation not started |
| M5 Agent surface | W7 | 7–9 | Not started |
| M6 Fleet and benchmarks | W8, integrator | 9–12 | Not started |

After M3: the D30 high-resolution store ([design/hi-res-store.md](design/hi-res-store.md); config contract merged in `fe5ea3a`/`47b2515`) and `lume sql` over plain indexes.

### M0 gate detail

| # | Gate item | State |
|---|---|---|
| 1 | `ti-contracts` merged with types, traits, doc comments | **Done** (`96ac45d`) |
| 2 | `ti.toml` schema with defaults | **Done** (`96ac45d`; multi-store extension `fe5ea3a`) |
| 3 | `ti-bench gen` reproduces the correctness set byte-identically from a seed | **Done** (`c65e515`; window pinned `71b7fcd`) |
| 4 | ≥ 60 golden queries with oracle twins and expected output | **Ready to close.** 60 non-empty + 1 `expect_empty` generated on the host (5 min 3 s). Waiting on Shark's commit to `tests/golden/expected/` |

### M2 gate detail

| # | Gate item | State |
|---|---|---|
| 1 | Replaying a recorded 24 h delta log yields `BucketRecord`s equal to oracle bucketing | **Passing on real data**: 460,112/460,112 against both the Rust oracle and DuckDB (`day=060`), and again with real H3 cells (`8931877`) |
| 2 | Parquet backfill of the correctness set is idempotent | **Not passed.** The full-set release run failed after 43 min with `StorageFull` (two full Stores in temp). The test is being reworked to build one store at a time |
| 3 | Pi 5 sustains 20,000 values/s for 1 h within the CPU and RSS budget | 181,783 values/s in release on the **host (x86), not a Pi 5**. RSS is "n/a" on non-Linux. A Pi 5 run is still needed (unconfirmed whether planned) |

### M3 gate detail

| # | Gate item | State |
|---|---|---|
| 1 | All non-text, non-geo golden queries match the oracle (fixture, then real store) | Open. Waits on the committed expected outputs |
| 2 | `EXPLAIN` shows Exact pushdown for every expression marked Exact | Open (unconfirmed whether this gate has been checked) |
| 3 | `raw` table queries the same Parquet and matches DuckDB exactly | Open. Data and DuckDB are ready |

### M4 gate detail

| # | Gate item | State |
|---|---|---|
| 1 | Full golden corpus green, including `match()`, `in_bbox`, `within_nm` and `intervals()` | Open. **Geo done** (`8931877`) and **`intervals()` done** (`8b89e48`). The Q7 gate is implemented but pending the committed outputs. `match()` (W5) is next, and `q7-005` waits on it |
| 2 | `BitmapAggregateExec` ≥ 10× faster than the materializing path on Q4 at shore scale | **Provisionally met**: 15.9× (debug, W = 60 s). Release W = 10 s run queued |
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

- [ ] **Disk space on the host.** Shrink `.lanes/w4/target` (48.6 GB), `.lanes/w3/target` (12.9 GB) and root `target/debug` (32 GB). Use `CARGO_INCREMENTAL=0`. About 13 GB free at 09:07.
- [ ] **M2 idempotence rework** (Artificial Shark): one store at a time, report the store size, then rerun on the full set.
- [ ] **Measure the index size** (still unmeasured).
- [ ] **Commit the 61 expected JSONs** (Artificial Shark, `ti/corpus-expected`). This closes M0.
- [ ] Release Q4 benchmark at W = 10 s (host, queued).
- [ ] Pi 5 throughput/RSS run for M2 item 3.
- [ ] Verify [signalk-formats §1.3](design/signalk-formats.md) against a **real** signalk-parquet `.parquet` file with DuckDB `DESCRIBE`.
- [ ] Strict TI checks are not in `ci.yml` yet. Containers lacking rustfmt or clippy rely on the lead's host run.
- [ ] Toolchain drift between the host and the containers (item 15).
- [x] D30 follow-ups (spec/10 mirror, spec/14 multi-store, D30 amendment). Merged in `47b2515`.
- [x] Corpus zero-row queries calibrated (`0fe2960`).

Also undecided: the repo-fit items in [repo-fit.md](repo-fit.md), such as the W7a/W7b split (§6). These are lead decisions unless escalated (unconfirmed).
