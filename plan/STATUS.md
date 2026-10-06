# Lume TI — Status board

Last updated: **2026-10-06** (docs keeper, after `26e535a`: **full corpus 58/0/4**, **M2 item 2 closed**, W7 surfaces and D30 fan-out merged)

Integration branch `plan/lume-ti` is at `26e535a`. **`ti-contracts` is frozen** (`96ac45d`). Root tests: 46 at `8e87a11` (not recounted since).
Workspace members: `ti-contracts`, `ti-core`, `ti-store`, `ti-sql`, `ti-ingest`, `ti-bench`, `ti-geo`. Next free decision: **D37** (ask the lead before taking it).
Setup and workflow: [SETUP.md](SETUP.md).

## Critical path right now

- **✅ FULL GOLDEN CORPUS: 58 passed, 0 failed, 4 excluded** (`cb39a4d`). `lume ti verify` now always runs geo and `intervals()`; text is skipped only when no document index is registered. The q7 oracles are D35-rounded (lat/lon at scale 7). The 4 exclusions are contract questions, with reasons in `tests/golden/corpus.json`: `q1-007`, `q6-006` and `q2-001` (count-path semantics; need a `count_paths` contract) and `qx-003` (DataFusion 55 cannot decorrelate an expression-keyed correlated scalar subquery; `qx-013` verifies the same result in join form).
- **M4 is still open on item 3**: the `croaring` frozen-view evaluation has not started. Item 1 is met except the 4 contract exclusions, and item 2 (Q4 72.4×) is met.
- **✅ M2 item 2 CLOSED** (`11c0edc`, Artificial Shark): full-set backfill idempotence on one store. M2 stays open on item 3 (Pi 5 throughput/RSS).
- **Known issue: cold store open on `store-full` takes ~70 s** (85.5 s before Long Horse's width-header fix). The lead is profiling `session_from_store` / `Store` open.
- **In flight:**
  - Long Horse: W7 HTTP `/ti` on `lume serve --ti-store` (integration green, rebasing).
  - Artificial Shark: D30 end to end (wire the fan-out into backfill and live ingest, the `telemetry_hr` SQL table, a retention-vs-WAL edge case).
- **Next:** the `croaring` evaluation (M4 item 3), the `count_paths` contract, the rest of W7 / M5, `lume sql`, W8.
- **Agents are working again** (both lanes merged code in this round). Docker Desktop's earlier outage ("unable to start") froze them from about 08:51 UTC; the lead carried M3 and W5 in the meantime.
- **Disk: 48.9 GB free** on the host. Root `target/` is 7.6 GB. The dev profile uses `debug = "line-tables-only"` (none for dependencies) since `3354bd0`, and agents cap their `target/` at 15 GB. See [SETUP §8](SETUP.md).

## Pane changes (about 06:00 UTC)

| Role | Now | Formerly |
|---|---|---|
| W4/W6/W5/W7 SQL and surfaces owner | **Long Horse** `a05c4a5e`, nemesis8/hyperia | Rigid Roadrunner `d58ca1b1`, n8-sly-viper |
| W3 ingest + corpus part 3 owner | **Artificial Shark** `9be462c8`, nemesis8/n8-spry-tapir | Romantic Pike `90fc608c`, n8-keen-kiwi |
| Corpus and generator | Lead took it over and merged it (`c65e515`) | Zygomorphic Prawn `eccaf836`, n8-noble-toad: **retired** (permanently offline, per the user) |
| Host build pane | **Compact Echidna** `6914c38e` | Regular Pheasant `364a3fc7` (gone) |

Commit messages, the decisions log and older docs use the former names. Pane ids changed when the containers came back after the Docker outage (user-confirmed 2026-10-06): Long Horse `a05c4a5e`, Artificial Shark `9be462c8`.

## Recent merges into `plan/lume-ti`

| Commit | What |
|---|---|
| `26e535a` | Host fmt of the W7 CLI/engine/MCP code (container lacks rustfmt), 2 strict-clippy fixes (`field_reassign_with_default` in `retention.rs` and `multi_store_fanout.rs`), and `surfaces.rs` scratch under `CARGO_TARGET_TMPDIR` so the tests run on the host |
| `20117c4` | Merge `ti/w3-ingest` (`11c0edc`, `7d4a081`, Artificial Shark). **M2 full-set idempotence** on one store, with throughput and index size. **D30** `MultiStoreBucketer` fan-out, per-store aggregates, `Store::enforce_retention`, `StoreSet` |
| `31fd76a` | Merge `ti/w7-surfaces` (`6afb2ab`, Long Horse). `lume ti query|explain|status|import-docs` on a shared `TiEngine`. In-process MCP tools `ti_query`, `ti_schema`, `ti_explain`, `ti_status` (`src/ti_mcp.rs`), capped at 500 rows and 64 KiB. Bucket width is read from the store |
| `cb39a4d` | **Verify always runs geo and `intervals()`**; text is skipped only without a document index. q7 oracles D35-rounded; `q7-004`/`q7-005` match TI exactly. Fixture placeholder entries carry explicit `exclude` reasons. Full store **58/0/4** |
| `3354bd0` | Cargo: `line-tables-only` debug info, none for dependencies (full DataFusion debug info had grown build caches to ~81 GB) |
| `b0ba6b0` | Docs refresh after W5 text |
| `cd9bb3e` | Text oracles cover `[ts_start, ts_end)` (spec/14, D36). `q2-001` and `q6-006` join the count-path exclusion. `q6-004` sort adds `sog`. Verify 47/0/15 |
| `11d961a` | **W5:** `ti-sql` `DocsProvider` (`docs` table): `match(body, q)` with Exact pushdown and the BM25 score. `open_store_with_documents`, `run_cli_with`. `lume ti` uses `LumeText` |
| `ad05f94` | **W5:** root `src/ti_text.rs` `LumeText` (`TextIndex` + `DocumentIndex` on Lume's `Bm25Index`, `--features ti`) for `match()` over notes/logbook/alerts. Adds `Bm25Index::search_quiet` |
| `c1d4941` | **W5:** `ti-store` `DocStore` at `<store>/docs/documents.json`. `ti-ingest` imports docs Parquet with content-addressed ids. `backfill_store` imports `<ancestor>/docs`. New example `import_docs` |
| `711d2c4` | **Closes M3.** D35: oracles round per-bucket aggregates to the path's catalog scale before filtering. Corpus `exclude` field; `qx-013`. Full store 42/0/20 |

Earlier: `6dc19c5` docs, `f6c47b4` ingest `profiles.opt_in` + `TI_OPT_IN`, `b07ec05` `IS [NOT] DISTINCT FROM` precedence, `f5806c0` D34 tolerance, `bfd7373` D33 default profile, `8f776da`/`82a8aa5`/`829c771` backfill speed + `backfill_store`, `5632aad` idempotence compares content, `b4a1879` **Q4 72.4×**, `be6ffdb` docs, `312f6a0` **corpus part 3, closes M0**, `443a4f2` docs, `19ab9fb` W6 rustfmt, `8931877` **W6 geo**, `47b2515` D30 follow-ups, `0fe2960` corpus calibration, `fe5ea3a` D30 config, `8afa646` D14 amendment, `b4e8da6` Q4 bench env vars, `2c8ba1c` M2 `#[ignore]` + DuckDB cross-check, `c88fb75` DuckDB table macros + `gen_expected.py`, `6c29ea2` D31, `7dd6be4` root README rewrite (MCP port 5863), `8b89e48` W4 part 2, `4c55f60` M2 harness, `571cc2b` D30, `71b7fcd` window fix, `c65e515` corpus lane, `33c67b1` store fix + SQL Store adapter, `46d69f4` W3 foundation, `98816b5` W4 foundation, `cf98c61` W2 (**M1 complete**), `922fc07` D27, `f7faf5f` W1, `1297968` search library, `96ac45d` contracts freeze, `06dd5c5` workspace. Full list: `git log --oneline --first-parent plan/lume-ti`.

## Agents

| Agent (pane name) | Pane | Lane/scope | Branch | Clone | Last known state |
|---|---|---|---|---|---|
| Industrial Pike | `ee764a09` | Lead and integrator. Owns the host runs (data generation, `gen_expected.py`, store backfill, `lume ti verify`, Q4 bench) and disk cleanup. Carried M3 and W5 text during the Docker outage. Now **profiling the ~70 s cold store open**. Runs fmt and 1.96 clippy on the host when a container lacks them | `plan/lume-ti` | shared tree | `26e535a` |
| Long Horse (formerly Rigid Roadrunner) | `a05c4a5e` | **W7 surfaces.** CLI and MCP tools merged (`31fd76a`). Now: HTTP `/ti` on `lume serve --ti-store` | `ti/w7-surfaces` | `.lanes/w4` | Integration green, **rebasing**. Clone at `20117c4` with uncommitted changes (`engine.rs`, `session.rs`, `surfaces.rs`, `W7-serve.md`, `src/agent.rs`) |
| Artificial Shark (formerly Romantic Pike) | `9be462c8` | **D30 end to end**: fan-out into backfill and live ingest, the `telemetry_hr` SQL table, the retention-vs-WAL edge case. M2 idempotence and the D30 fan-out merged (`20117c4`) | `ti/w3-ingest` | `.lanes/w3` | Clone at `20117c4`, clean |
| Zygomorphic Prawn | `eccaf836` | — | — | `.lanes/corpus` (retired) | **Retired.** Its `target/` has been deleted |
| Compact Echidna (formerly Regular Pheasant) | `6914c38e` | Host build pane (PowerShell 7, rustc 1.96.1). Not an agent | — | — | Bulk data and DuckDB jobs run here. **Check free disk before big builds** |

How to refresh the last column: `git -C .lanes/<x> log --oneline -5` and `git -C .lanes/<x> status --short`.

## Milestones

Week numbers count from kickoff. A milestone closes only when every gate test passes in CI ([spec/12](spec/12-milestones.md)).

| Milestone | Lanes | Weeks | Gate status |
|---|---|---|---|
| M0 Contracts | W0 | 1 | **✅ Complete** (`312f6a0`). All 4 gate items done in week 1 |
| M1 Core and store | W1, W2 | 2–4 | **Complete** (`cf98c61`) |
| M2 Ingest | W3 | 2–5 | **In progress.** Items 1 (replay) and 2 (full-set idempotence, `11c0edc`) passed. Item 3 (Pi 5 throughput/RSS) is open |
| M3 SQL and pushdown | W4 | 2–6 | **✅ Closed 2026-10-06** (`711d2c4`). Full-store verify 42/0/20 at close |
| M4 Text, geo, intervals | W4, W5, W6 | 6–8 | **In progress.** Item 1 met except 4 contract exclusions (full corpus **58/0/4**, `cb39a4d`). Item 2 met (72.4×). **Item 3 (`croaring` evaluation) not started** |
| M5 Agent surface | W7 | 7–9 | **In progress.** `lume ti` CLI and in-process MCP `ti_*` tools merged (`31fd76a`); HTTP `/ti` on `lume serve` in flight. No gate item met yet (`ti_resolve` top-3 test, 20 scripted questions, plugin/pgwire) (unconfirmed) |
| M6 Fleet and benchmarks | W8, integrator | 9–12 | Not started |

Remaining after M4: the D30 high-resolution store ([design/hi-res-store.md](design/hi-res-store.md); fan-out merged in `20117c4`, end-to-end wiring in flight), `lume sql` over plain indexes, the rest of W7, then W8.

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
| 2 | Parquet backfill of the correctness set is idempotent | **✅ Passed** (`11c0edc`, merged `20117c4`), on one store: 11,500 files, 95,924,426 rows. Initial backfill 785.87 s (122,061 rows/s in the test harness), rerun 633.44 s. All **65 sealed shards identical** (key, from, to, bytes, hash). Index 585.6 MB |
| 3 | Pi 5 sustains 20,000 values/s for 1 h within the CPU and RSS budget | **Open.** 181,783 values/s in release on the **host (x86), not a Pi 5**. RSS is "n/a" on non-Linux. A Pi 5 run is still needed (unconfirmed whether planned) |

### M3 gate detail

**Closed by the lead on 2026-10-06** (`711d2c4`). Reproduce with the commands in [SETUP §3](SETUP.md).

| # | Gate item | State |
|---|---|---|
| 1 | All non-text, non-geo golden queries match the oracle (fixture, then real store) | **Done.** Full store (5 vessels × 90 d, 95.9M raw rows, `TI_OPT_IN=last`): 42 passed, 0 failed, 20 excluded at close. Tolerance is D34, oracle quantization D35 |
| 2 | `EXPLAIN` shows Exact pushdown for every expression marked Exact | Closed with the gate. No separate run is recorded here (unconfirmed) |
| 3 | `raw` table queries the same Parquet and matches DuckDB exactly | Closed with the gate. No separate run is recorded here (unconfirmed) |

### M4 gate detail

| # | Gate item | State |
|---|---|---|
| 1 | Full golden corpus green, including `match()`, `in_bbox`, `within_nm` and `intervals()` | **Met except 4 contract exclusions** (`cb39a4d`): full store **58 passed, 0 failed, 4 excluded**. Text (W5, D36), geo (q7, D35-rounded oracles) and `intervals()` (q5) all verify. Excluded: `q1-007`, `q6-006`, `q2-001` (need a `count_paths` contract) and `qx-003` (DataFusion 55 decorrelation limit; `qx-013` covers it in join form) |
| 2 | `BitmapAggregateExec` ≥ 10× faster than the materializing path on Q4 at shore scale | **Met in release**: 40.3 ms vs 2.92 s = **72.4×** (W = 10 s, 5 vessel-years, `b4a1879`) |
| 3 | `croaring` frozen-view evaluation written up in the decisions log, adopt or reject | **Open. Not started** |

### M5 gate detail

| # | Gate item | State |
|---|---|---|
| 1 | MCP tools live in `lume serve`; `ti_resolve` top-3 for ≥ 90 % of a 100-phrase test set | Open. In-process MCP `ti_query`/`ti_schema`/`ti_explain`/`ti_status` merged (`31fd76a`); HTTP `/ti` on `lume serve --ti-store` in flight. `ti_resolve` and its test set not reported (unconfirmed) |
| 2 | A nemesis8 agent with only Lume MCP answers 20 scripted fleet questions | Open |
| 3 | Signal K plugin installs on HALPI2 / OpenPlotter; psql and Grafana pass a 20-query smoke set | Open |

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

- [ ] **Cold store open on `store-full` takes ~70 s** (85.5 s before the width-header fix). Lead profiling `session_from_store` / `Store` open.
- [ ] **`croaring` frozen-view evaluation** (M4 item 3), written up in the decisions log.
- [ ] **`count_paths` contract** for count-path semantics (`q1-007`, `q6-006`, `q2-001`; spec/05).
- [ ] **`qx-003`**: DataFusion 55 cannot decorrelate it. Keep the join-form `qx-013`, or revisit on a DataFusion upgrade.
- [ ] W7 HTTP `/ti` on `lume serve --ti-store` (Long Horse, rebasing).
- [ ] D30 end to end: fan-out into backfill and live ingest, `telemetry_hr` SQL table, retention vs WAL (Artificial Shark).
- [ ] Meridian VHF transcripts and live notes polling (`GET /signalk/v2/api/resources/notes` every 60 s). Not done; they go with W7 and the ingest service.
- [ ] Pi 5 throughput/RSS run for M2 item 3.
- [x] Full golden corpus: 58/0/4 (`cb39a4d`); q7 oracles D35-rounded.
- [x] M2 full-set idempotence on one store (`11c0edc`).
- [x] W7 CLI and in-process MCP tools (`31fd76a`); D30 multi-store fan-out and retention (`20117c4`).
- [x] W5 text: `match()` lexical BM25 (D36), docs table, docs import (`c1d4941`…`cd9bb3e`).
- [x] Docker back up; both agents working again.
- [x] Disk: 48.9 GB free after `3354bd0` (line-tables-only debug info); agents cap `target/` at 15 GB.
- [x] Index size: 729.3 MB vs 1,892.7 MB raw (0.39×) via `backfill_store` with `TI_OPT_IN=last`; 554 MB (0.29×) without. The M2 gate's store measured 585.6 MB (`11c0edc`; its aggregate settings were not reported here).
- [ ] Verify [signalk-formats §1.3](design/signalk-formats.md) against a **real** signalk-parquet `.parquet` file with DuckDB `DESCRIBE`.
- [ ] Strict TI checks are not in `ci.yml` yet. Containers lacking rustfmt or clippy rely on the lead's host run (as in `26e535a`).
- [ ] Toolchain drift between the host and the containers (item 15).

Also undecided: the repo-fit items in [repo-fit.md](repo-fit.md), such as the W7a/W7b split (§6). These are lead decisions unless escalated (unconfirmed).
