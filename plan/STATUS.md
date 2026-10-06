# Lume TI — Status board

Last updated: **2026-10-06** (docs keeper, after `39c0096`: **HTTP `/ti` and `ti_resolve` merged**, **M5 item 1 met** at 93/100; full corpus 58/0/4)

Integration branch `plan/lume-ti` is at `39c0096`. **`ti-contracts` is frozen** (`96ac45d`). Root tests: 46 at `8e87a11`. Host checks on `39c0096`: `cargo test --features ti` **55 passed**, strict clippy clean, default build green.
Workspace members: `ti-contracts`, `ti-core`, `ti-store`, `ti-sql`, `ti-ingest`, `ti-bench`, `ti-geo`. Next free decision: **D37** (ask the lead before taking it).
Setup and workflow: [SETUP.md](SETUP.md).

## Critical path right now

- **✅ FULL GOLDEN CORPUS: 58 passed, 0 failed, 4 excluded** (`cb39a4d`). `lume ti verify` now always runs geo and `intervals()`; text is skipped only when no document index is registered. The q7 oracles are D35-rounded (lat/lon at scale 7). The 4 exclusions are contract questions, with reasons in `tests/golden/corpus.json`: `q1-007`, `q6-006` and `q2-001` (count-path semantics; need a `count_paths` contract) and `qx-003` (DataFusion 55 cannot decorrelate an expression-keyed correlated scalar subquery; `qx-013` verifies the same result in join form).
- **M4 is still open on item 3**: the `croaring` frozen-view evaluation has not started. Item 1 is met except the 4 contract exclusions, and item 2 (Q4 72.4×) is met.
- **✅ M5 item 1 MET** (`39c0096`): `ti_resolve` returns the right column in the top 3 for **93/100** phrases in `tests/golden/resolve.json`, over 488 columns (fixture accuracy). The MCP tools are live in `lume serve`, and HTTP `/ti` is merged (`bce7779`).
- **✅ M2 item 2 CLOSED** (`11c0edc`, Artificial Shark): full-set backfill idempotence on one store. M2 stays open on item 3 (Pi 5 throughput/RSS).
- **Cold open is fast on the host.** The earlier ~70 s was the container's bind mount. On the Windows host (`4626591` `open_timing` example): `Store::open` 0.07 s, `session_from_store` 1.78 s, `lume ti status` 2.17 s wall, `count(*)` over `telemetry` 2.99 s.
- **🟡 Licence decision needed (user, before publishing):** `src/ti_resolve/signalk_paths.json` (150 KB) is extracted from SignalK/specification 1.8.4 @ `fb628fb4` under **CC-BY-SA 2.0**. It ships with its own `LICENSE` and `README.md` in `src/ti_resolve/`. See user item 16.
- **In flight:**
  - Long Horse: read-only **pgwire** (`--pg-port`, off by default, loopback), then **M5 item 2** (20 scripted questions).
  - Artificial Shark: rebasing D30 end to end (`5ac2971`) onto `39c0096` (conflict in `engine.rs`). Content: multi-store backfill and stream, the `telemetry_hr` SQL table, retention that drops only sealed shards.
- **Next:** the `croaring` evaluation (M4 item 3), the `count_paths` contract, pgwire and the rest of M5, `lume sql`, W8.
- **Agents are working again** (both lanes merged code in this round). Docker Desktop's earlier outage ("unable to start") froze them from about 08:51 UTC; the lead carried M3 and W5 in the meantime.
- **Disk: 42.9 GB free** on the host. Root `target/` is 8.7 GB. The dev profile uses `debug = "line-tables-only"` (none for dependencies) since `3354bd0`, and agents cap their `target/` at 15 GB. See [SETUP §8](SETUP.md).

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
| `39c0096` | Merge `ti/w7-surfaces` (`8caac42`, Long Horse). **`ti_resolve`** via MCP and `GET /ti/resolve?q=`: 93/100 top-3 on `tests/golden/resolve.json` over 488 columns. `lume serve --bind <IP>`: loopback `127.0.0.1` by default when `--ti-store` is set, plain `lume serve` keeps `0.0.0.0`. No wildcard CORS on `/ti` or the TI server's `/mcp`. Adds `src/ti_resolve/signalk_paths.json` (CC-BY-SA 2.0, see user item 16) |
| `0b3a17a` | Host rustfmt after the W7 HTTP merge |
| `400557c` | Root README: forward-looking Lume TI section with the current state and benchmarks |
| `bce7779` | Merge `ti/w7-surfaces` (`635fbd1`, Long Horse). **HTTP `/ti` on `lume serve --ti-store`**: `POST /ti/query` (Arrow IPC or JSON, chosen by `Accept`), `GET /ti/schema`, `POST /ti/explain`, `GET /ti/status`. One shared engine; one width header read per shard |
| `4626591` | `ti-sql` `open_timing` example (per-stage store/session/query timing) |
| `b95f890` | Docs refresh after the full corpus, M2 item 2, W7 CLI/MCP and D30 merges |
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
| Industrial Pike | `ee764a09` | Lead and integrator. Owns the host runs (data generation, `gen_expected.py`, store backfill, `lume ti verify`, Q4 bench) and disk cleanup. Carried M3 and W5 text during the Docker outage. Measured host cold-open timings (`4626591`; the ~70 s was the container bind mount). Merged W7 HTTP and `ti_resolve`. Runs fmt and 1.96 clippy on the host when a container lacks them | `plan/lume-ti` | shared tree | `39c0096` |
| Long Horse (formerly Rigid Roadrunner) | `a05c4a5e` | **W7 surfaces.** CLI and MCP tools (`31fd76a`), HTTP `/ti` (`bce7779`) and `ti_resolve` (`39c0096`) merged. Now: read-only pgwire, then M5 item 2 | `ti/w7-surfaces` | `.lanes/w4` | Last merged `8caac42` |
| Artificial Shark (formerly Romantic Pike) | `9be462c8` | **D30 end to end** (`5ac2971`): multi-store backfill and stream, the `telemetry_hr` SQL table, retention that drops only sealed shards. M2 idempotence and the D30 fan-out merged (`20117c4`) | `ti/w3-ingest` | `.lanes/w3` | **Rebasing** `5ac2971` onto `39c0096` (`engine.rs` conflict) |
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
| M5 Agent surface | W7 | 7–9 | **In progress.** **Item 1 met** (`39c0096`): MCP tools live in `lume serve`, `ti_resolve` 93/100 top-3. Item 2 (20 scripted questions) next, after pgwire. Item 3 (Signal K plugin, psql/Grafana) open |
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
| 1 | MCP tools live in `lume serve`, and `ti_resolve` returns the right column in the top 3 for ≥ 90 % of a 100-phrase test set | **✅ Met** (`39c0096`). `ti_query`/`ti_schema`/`ti_explain`/`ti_status`/`ti_resolve` are live in `lume serve --ti-store` (MCP and HTTP `/ti`). `ti_resolve`: **93/100** top-3 on `tests/golden/resolve.json` over 488 columns. This is fixture accuracy |
| 2 | A nemesis8 agent with only Lume MCP answers 20 scripted fleet questions; integrator grades against oracle results | Open. Long Horse takes it after pgwire |
| 3 | Signal K plugin installs on HALPI2 / OpenPlotter; psql and Grafana pass a 20-query smoke set | Open. Read-only pgwire (`--pg-port`, off by default, loopback) is in progress (Long Horse) |

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
16. **Licence of `src/ti_resolve/signalk_paths.json`** (150 KB, merged in `39c0096`). It is extracted from SignalK/specification 1.8.4 @ `fb628fb4` under **CC-BY-SA 2.0**, with its own `LICENSE` and `README.md` in `src/ti_resolve/`. Lume is BSD-3. Is shipping it acceptable? Decide before publishing.

## Open items (team, not user)

- [ ] **`croaring` frozen-view evaluation** (M4 item 3), written up in the decisions log.
- [ ] **`count_paths` contract** for count-path semantics (`q1-007`, `q6-006`, `q2-001`; spec/05).
- [ ] **`qx-003`**: DataFusion 55 cannot decorrelate it. Keep the join-form `qx-013`, or revisit on a DataFusion upgrade.
- [ ] **User licence decision** on `src/ti_resolve/signalk_paths.json` (CC-BY-SA 2.0) before publishing (user item 16).
- [ ] Read-only pgwire (`--pg-port`, off by default, loopback), then M5 item 2 (Long Horse).
- [x] W7 HTTP `/ti` on `lume serve --ti-store` (`bce7779`) and `ti_resolve` 93/100 (`39c0096`).
- [ ] D30 end to end (`5ac2971`, rebasing onto `39c0096`): multi-store backfill and stream, `telemetry_hr` SQL table, retention drops only sealed shards (Artificial Shark).
- [ ] Meridian VHF transcripts and live notes polling (`GET /signalk/v2/api/resources/notes` every 60 s). Not done; they go with W7 and the ingest service.
- [ ] Pi 5 throughput/RSS run for M2 item 3.
- [x] Full golden corpus: 58/0/4 (`cb39a4d`); q7 oracles D35-rounded.
- [x] M2 full-set idempotence on one store (`11c0edc`).
- [x] W7 CLI and in-process MCP tools (`31fd76a`); D30 multi-store fan-out and retention (`20117c4`).
- [x] W5 text: `match()` lexical BM25 (D36), docs table, docs import (`c1d4941`…`cd9bb3e`).
- [x] Docker back up; both agents working again.
- [x] Disk: 42.9 GB free, root `target/` 8.7 GB (line-tables-only debug info since `3354bd0`); agents cap `target/` at 15 GB.
- [x] Index size: 729.3 MB vs 1,892.7 MB raw (0.39×) via `backfill_store` with `TI_OPT_IN=last`; 554 MB (0.29×) without. The M2 gate's store measured 585.6 MB (`11c0edc`; its aggregate settings were not reported here).
- [ ] Verify [signalk-formats §1.3](design/signalk-formats.md) against a **real** signalk-parquet `.parquet` file with DuckDB `DESCRIBE`.
- [ ] Strict TI checks are not in `ci.yml` yet. Containers lacking rustfmt or clippy rely on the lead's host run (as in `26e535a`).
- [ ] Toolchain drift between the host and the containers (item 15).

Also undecided: the repo-fit items in [repo-fit.md](repo-fit.md), such as the W7a/W7b split (§6). These are lead decisions unless escalated (unconfirmed).
