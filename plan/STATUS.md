# Lume TI — Status board

Last updated: **2026-10-06** (docs keeper, after `cd9bb3e`: **W5 text landed**, verify 47/0/15; Docker still down)

Integration branch `plan/lume-ti` is at `cd9bb3e`. **`ti-contracts` is frozen** (`96ac45d`). Root tests: 46.
Workspace members: `ti-contracts`, `ti-core`, `ti-store`, `ti-sql`, `ti-ingest`, `ti-bench`, `ti-geo`. Next free decision: **D37** (ask the lead before taking it).
Setup and workflow: [SETUP.md](SETUP.md).

## Critical path right now

- **🛑 BLOCKER: Docker Desktop is down** ("Docker Desktop is unable to start"). **Both agent containers are down**: Long Horse (`888bff45`) and Artificial Shark (`fd91f4b1`, M2 single-store rework and D30 ingest fan-out) have been frozen since about **08:51 UTC**. Their work is safe on disk in `.lanes/w4` and `.lanes/w3`. **The lead (Industrial Pike) is carrying the work on the host** until Docker comes back.
- **✅ M3 CLOSED 2026-10-06** (`711d2c4`). `lume ti verify` over the full store (5 vessels × 90 d, 95.9M raw rows): **42 passed, 0 failed, 20 excluded**. 18 exclusions are M4 (text, geo, intervals). 2 are open contract questions, with reasons in the `exclude` fields of `tests/golden/corpus.json`: `q1-007` (count-path semantics; needs a `count_paths` contract) and `qx-003` (DataFusion 55 cannot decorrelate an expression-keyed correlated scalar subquery; the same result is verified by the new `qx-013` in join form).
- **✅ W5 text LANDED** (`c1d4941`, `ad05f94`, `11d961a`, `cd9bb3e`), done by the lead because Long Horse is frozen. `match()` is lexical Lume BM25 (**D36**). Verify over the full store with text enabled: **47 passed, 0 failed, 15 excluded**: 11 geo/intervals, 3 count-path (`q1-007`, `q6-006`, `q2-001`), and `qx-003` (DataFusion limit). Not done yet: Meridian VHF transcripts and live notes polling, which go with W7 and the ingest service.
- **In progress (lead):** running the geo (q7) and intervals (q5) entries against the store to close M4 item 1.
- **Index size measured** (`TI_OPT_IN=last`): **729.3 MB** vs 1,892.7 MB raw Parquet (**0.39×**). Backfill 436.5 s (219,779 rows/s), 65 shards sealed in 11.6 s. Without the `@last` opt-in it was 554 MB (0.29×). To reproduce, see [SETUP §3](SETUP.md).
- **Next:** finish M4 (geo/intervals verify, `croaring` evaluation), W7 surfaces, the D30 1 s store, `lume sql`, W8.
- **M0 closed** (`312f6a0`). The correctness set is ready (1.8 GB, 11,508 files, manifest `edfef2d8…` in `.lanes/data/correctness.sha256`).
- **Disk:** still tight. See the Disk budget section in [SETUP §8](SETUP.md). Last measured big users (09:07): `.lanes/w4/target` 48.6 GB, `.lanes/w3/target` 12.9 GB, root `target/debug` 32 GB.

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
| `cd9bb3e` | Text oracles cover `[ts_start, ts_end)` (spec/14, D36). `q2-001` and `q6-006` join the count-path exclusion. `q6-004` sort adds `sog`. Verify **47/0/15** |
| `11d961a` | **W5:** `ti-sql` `DocsProvider` (`docs` table): `match(body, q)` with Exact pushdown and the BM25 score. `open_store_with_documents`, `run_cli_with`. `lume ti` uses `LumeText` |
| `ad05f94` | **W5:** root `src/ti_text.rs` `LumeText` (`TextIndex` + `DocumentIndex` on Lume's `Bm25Index`, `--features ti`) for `match()` over notes/logbook/alerts. Adds `Bm25Index::search_quiet` |
| `c1d4941` | **W5:** `ti-store` `DocStore` at `<store>/docs/documents.json`. `ti-ingest` imports docs Parquet with content-addressed ids. `backfill_store` imports `<ancestor>/docs`. New example `import_docs` |
| `6dc19c5` | Docs refresh after M3 close |
| `711d2c4` | **Closes M3.** D35: golden oracles round per-bucket aggregates (`min`/`max`/`avg`/`arg_max`) to the path's catalog scale before any filter, join or roll-up, which matches TI's fixed-point predicates. `q3-002/003/004/007` and `qx-009` now equal TI exactly. Corpus entries take an optional `exclude` reason (`q1-007`, `qx-003`), and `qx-013` verifies `qx-003` in join form. Full store: **42/0/20** |
| `f6c47b4` | Ingest emits `profiles.opt_in` aggregates (spec/05), so `@last` exists. `backfill_store` pre-registers vessel names/MMSIs from `catalog/vessels` and takes `TI_OPT_IN=last` |
| `b07ec05` | `ti-sql`: re-associate `IS [NOT] DISTINCT FROM` over an AND/OR chain that sqlparser 0.62 swallows (precedence fix) |
| `f5806c0` | D34: verify tolerance is **±1 × 10^−scale** (one unit in the last place). `backfill_store` seeds path scales from `catalog/paths` |
| `bfd7373` | D33: fixed default aggregate profile (`@mean/@min/@max`) per numeric path; lat/lon scale 7 without `meta.units`. The first M3 run went from 5 passed / 38 failed to 21 / 22 / 18 excluded |
| `8f776da`, `82a8aa5`, `829c771` | Backfill speed: stage only touched fields and apply per shard (34×), 50k-record apply chunks, batched flushes. Adds the `backfill_store` example (the index-size tool until `lume ti backfill` exists) |
| `5632aad` | `m2_gate` idempotence compares sealed content (key, range, bytes, hash), not version, because repair bumps the version by spec |
| `b4a1879` | **Q4 benchmark, release, W = 10 s, 5 vessel-years: bitmap 40.3 ms vs 2.92 s materialized (72.4×)** |
| `be6ffdb` | Docs refresh for M0 closed |
| `312f6a0` | **Corpus part 3, closes M0.** 61 golden expected JSONs (DuckDB 1.5.6, correctness set `edfef2d8…`). 10 large-result queries narrowed to 2026-05-07 00:00–06:00 UTC under D32 |

Earlier: `443a4f2` docs, `19ab9fb` W6 rustfmt, `8931877` **W6 geo**, `47b2515` D30 follow-ups, `0fe2960` corpus calibration, `fe5ea3a` D30 config, `8afa646` D14 amendment, `b4e8da6` Q4 bench env vars, `2c8ba1c` M2 `#[ignore]` + DuckDB cross-check, `c88fb75` DuckDB table macros + `gen_expected.py`, `6c29ea2` D31, `7dd6be4` root README rewrite (MCP port 5863), `8b89e48` W4 part 2, `4c55f60` M2 harness, `571cc2b` D30, `71b7fcd` window fix, `c65e515` corpus lane, `33c67b1` store fix + SQL Store adapter, `46d69f4` W3 foundation, `98816b5` W4 foundation, `cf98c61` W2 (**M1 complete**), `922fc07` D27, `f7faf5f` W1, `1297968` search library, `96ac45d` contracts freeze, `06dd5c5` workspace. Docs refreshes omitted. Full list: `git log --oneline --first-parent plan/lume-ti`.

## Agents

| Agent (pane name) | Pane | Lane/scope | Branch | Clone | Last known state |
|---|---|---|---|---|---|
| Industrial Pike | `ee764a09` | Lead and integrator. Owns the host runs (data generation, `gen_expected.py`, store backfill, `lume ti verify`, Q4 bench) and disk cleanup. **Carrying the agents' work while Docker is down** (closed M3 with changes in the W2, W3 and W4 crates, and landed W5 text). Now on M4 item 1 (q7 geo, q5 intervals). Runs fmt and 1.96 clippy on the host when a container lacks them | `plan/lume-ti` | shared tree | `cd9bb3e` |
| Long Horse (formerly Rigid Roadrunner) | `888bff45` | Was W5 text. **W5 was landed by the lead** (`c1d4941`…`cd9bb3e`, D36), so its next assignment is open (unconfirmed) | — | `.lanes/w4` | **Container DOWN since ~08:51 UTC (Docker Desktop unable to start); frozen.** W6 merged (`8931877`). Clone is on `ti/w6-geo` at `e73ea12`, clean. Its `target/` is **48.6 GB**, and it has been asked to shrink it |
| Artificial Shark (formerly Romantic Pike) | `fd91f4b1` | Next: the **M2 single-store idempotence rework** plus the store size, then the **D30 ingest fan-out**. The expected JSONs were committed by the lead (`312f6a0`) | `ti/corpus-expected` (stale) | `.lanes/w3` | **Container DOWN since ~08:51 UTC (Docker Desktop unable to start); frozen.** Clone on `ti/corpus-expected` at `a64e27c` with an **untracked `tests/golden/expected/`**, which now collides with the committed files (see Open items). `target/` 12.9 GB |
| Zygomorphic Prawn | `eccaf836` | — | — | `.lanes/corpus` (retired) | **Retired.** Its `target/` has been deleted |
| Compact Echidna (formerly Regular Pheasant) | `6914c38e` | Host build pane (PowerShell 7, rustc 1.96.1). Not an agent | — | — | Bulk data and DuckDB jobs run here. **Check free disk before big builds** |

How to refresh the last column: `git -C .lanes/<x> log --oneline -5` and `git -C .lanes/<x> status --short`.

## Milestones

Week numbers count from kickoff. A milestone closes only when every gate test passes in CI ([spec/12](spec/12-milestones.md)).

| Milestone | Lanes | Weeks | Gate status |
|---|---|---|---|
| M0 Contracts | W0 | 1 | **✅ Complete** (`312f6a0`). All 4 gate items done in week 1 |
| M1 Core and store | W1, W2 | 2–4 | **Complete** (`cf98c61`) |
| M2 Ingest | W3 | 2–5 | **In progress.** Replay done on real data (460,112/460,112 with real H3 cells). Full-set idempotence has not been re-run since the rework fixes (`5632aad`) (unconfirmed) |
| M3 SQL and pushdown | W4 | 2–6 | **✅ Closed 2026-10-06** (`711d2c4`). Full-store verify: 42 passed, 0 failed, 20 excluded (18 M4, 2 contract questions) |
| M4 Text, geo, intervals | W4, W5, W6 | 6–8 | **In progress.** **W5 text landed** (`cd9bb3e`, D36); verify 47/0/15. Geo and `intervals()` are implemented, and their 11 entries are being run against the store (lead). `BitmapAggregateExec` **72.4×** in release (`b4a1879`). `croaring` evaluation not started |
| M5 Agent surface | W7 | 7–9 | Not started |
| M6 Fleet and benchmarks | W8, integrator | 9–12 | Not started |

With M3 closed, next come W7 surfaces, the D30 high-resolution store ([design/hi-res-store.md](design/hi-res-store.md); config contract merged in `fe5ea3a`/`47b2515`) and `lume sql` over plain indexes, then W8.

### M0 gate detail

| # | Gate item | State |
|---|---|---|
| 1 | `ti-contracts` merged with types, traits, doc comments | **Done** (`96ac45d`) |
| 2 | `ti.toml` schema with defaults | **Done** (`96ac45d`; multi-store extension `fe5ea3a`) |
| 3 | `ti-bench gen` reproduces the correctness set byte-identically from a seed | **Done** (`c65e515`; window pinned `71b7fcd`) |
| 4 | ≥ 60 golden queries with oracle twins and expected output | **Done** (`312f6a0`). 61 JSONs: 60 with rows + `qx-006` `expect_empty`. 10 queries narrowed to a 6 h window under D32 |

### M2 gate detail

| # | Gate item | State |
|---|---|---|
| 1 | Replaying a recorded 24 h delta log yields `BucketRecord`s equal to oracle bucketing | **Passing on real data**: 460,112/460,112 against both the Rust oracle and DuckDB (`day=060`), and again with real H3 cells (`8931877`) |
| 2 | Parquet backfill of the correctness set is idempotent | **Not passed yet.** The first full-set run died with `StorageFull`. Since then the assertion compares sealed content, not version (`5632aad`), and backfill is much faster (`8f776da`, `82a8aa5`, `829c771`). No full-set re-run has been reported (unconfirmed) |
| 3 | Pi 5 sustains 20,000 values/s for 1 h within the CPU and RSS budget | 181,783 values/s in release on the **host (x86), not a Pi 5**. RSS is "n/a" on non-Linux. A Pi 5 run is still needed (unconfirmed whether planned) |

### M3 gate detail

**Closed by the lead on 2026-10-06** (`711d2c4`). Reproduce with the commands in [SETUP §3](SETUP.md).

| # | Gate item | State |
|---|---|---|
| 1 | All non-text, non-geo golden queries match the oracle (fixture, then real store) | **Done.** Full store (5 vessels × 90 d, 95.9M raw rows, `TI_OPT_IN=last`): **42 passed, 0 failed, 20 excluded**. Exclusions: 18 are M4 (text, geo, intervals). `q1-007` needs a `count_paths` contract. `qx-003` hits a DataFusion 55 limit (it cannot decorrelate an expression-keyed correlated scalar subquery) and is verified instead as `qx-013` in join form. Tolerance is D34, oracle quantization D35 |
| 2 | `EXPLAIN` shows Exact pushdown for every expression marked Exact | Closed with the gate. No separate run is recorded here (unconfirmed) |
| 3 | `raw` table queries the same Parquet and matches DuckDB exactly | Closed with the gate. No separate run is recorded here (unconfirmed) |

### M4 gate detail

| # | Gate item | State |
|---|---|---|
| 1 | Full golden corpus green, including `match()`, `in_bbox`, `within_nm` and `intervals()` | Open. **`match()` done** (W5, `cd9bb3e`): full-store verify **47 passed, 0 failed, 15 excluded**. The 11 geo (q7) and intervals (q5) entries are being run against the store now (lead). Geo is implemented in `8931877` and `intervals()` in `8b89e48`. The q7 geo oracles (`arg_max … FILTER`) are not D35-rounded yet. Also open for a fully green corpus: the 3 count-path entries (`q1-007`, `q6-006`, `q2-001`) and `qx-003` |
| 2 | `BitmapAggregateExec` ≥ 10× faster than the materializing path on Q4 at shore scale | **Met in release**: 40.3 ms vs 2.92 s = **72.4×** (W = 10 s, 5 vessel-years, `b4a1879`). The earlier debug run gave 15.9× |
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

- [ ] **Restart Docker Desktop and relaunch Long Horse and Artificial Shark** (asked of the user; "Docker Desktop is unable to start"). The lead is carrying the work meanwhile.
- [ ] **Disk space on the host.** Shrink `.lanes/w4/target` (48.6 GB), `.lanes/w3/target` (12.9 GB) and root `target/debug` (32 GB). Use `CARGO_INCREMENTAL=0`. About 13 GB free at 09:07. The full store `.lanes/data/store-full` (~0.7 GB) now lives there too.
- [ ] **M2 idempotence full-set re-run** (Artificial Shark, or the lead while it is down), one store at a time.
- [x] Measure the index size: 729.3 MB vs 1,892.7 MB raw (0.39×) with `TI_OPT_IN=last`; 554 MB (0.29×) without.
- [x] M3 corpus run on the full store: 42/0/20 (`711d2c4`).
- [x] W5 text: `match()` lexical BM25 (D36), docs table, docs import (`c1d4941`…`cd9bb3e`). Verify 47/0/15.
- [ ] Meridian VHF transcripts and live notes polling (`GET /signalk/v2/api/resources/notes` every 60 s). Not done; they go with W7 and the ingest service.
- [ ] Geo (q7) and intervals (q5) entries against the store, to close M4 item 1 (lead, **in progress**).
- [ ] **`count_paths` contract** for count-path semantics (`q1-007`, `q6-006`, `q2-001`; spec/05).
- [ ] **`qx-003`**: DataFusion 55 cannot decorrelate it. Keep the join-form `qx-013`, or revisit on a DataFusion upgrade.
- [ ] D35-round the q7 geo oracles before the M4 corpus run.
- [x] Commit the 61 expected JSONs. Done by the lead in `312f6a0` (M0 closed).
- [ ] **Artificial Shark's clone has an untracked `tests/golden/expected/`** that collides with the now-committed files. It must delete or move it before merging `plan/lume-ti`, or git will refuse to overwrite it.
- [x] Release Q4 benchmark at W = 10 s: 72.4× (`b4a1879`).
- [ ] Pi 5 throughput/RSS run for M2 item 3.
- [ ] Verify [signalk-formats §1.3](design/signalk-formats.md) against a **real** signalk-parquet `.parquet` file with DuckDB `DESCRIBE`.
- [ ] Strict TI checks are not in `ci.yml` yet. Containers lacking rustfmt or clippy rely on the lead's host run.
- [ ] Toolchain drift between the host and the containers (item 15).
- [x] D30 follow-ups (spec/10 mirror, spec/14 multi-store, D30 amendment). Merged in `47b2515`.
- [x] Corpus zero-row queries calibrated (`0fe2960`).

Also undecided: the repo-fit items in [repo-fit.md](repo-fit.md), such as the W7a/W7b split (§6). These are lead decisions unless escalated (unconfirmed).
