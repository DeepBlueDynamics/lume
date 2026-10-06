# Lume TI — Status board

Last updated: **2026-10-06 05:06 UTC** (docs keeper, after `8e87a11`: W2 ti-store merged, M1 complete)

Integration branch `plan/lume-ti` is at `8e87a11`. **`ti-contracts` is frozen** (`96ac45d`). Root tests: 46. Workspace members: `ti-contracts`, `ti-core`, `ti-store`. Next free decision: **D28** (ask the lead before taking it).
Setup and workflow: [SETUP.md](SETUP.md).

## Recent merges into `plan/lume-ti`

| Commit | What |
|---|---|
| `8e87a11` | Lead clippy fix (`nonminimal_bool`) in `ti-store` for rustc 1.96 on the host, since the containers use 1.99. W2 lane notes: plain-read path, no mmap |
| `cf98c61` | Merge `ti/w2-store`: `crates/ti-store` with a WAL using D16 group commit (sync on tick, shutdown and Drop), an open shard, deterministic seal, repair versions, catalog/manifest with directory fsync, and a plain-read path (no mmap, no `unsafe`). D24 (`bincode`) and D25 (`crc32fast`). One review round strengthened the crash test: seeded random kill points (332 mid-apply, 344 mid-flush, 324 between flush and truncate), value verification, a mixed workload checked against a reference model, and D21 validation after each recovery. **M1 gate items 3 and 4 done, so M1 is complete** |
| `922fc07` | **D27**: accept `zstd-sys` (C), which DataFusion's `arrow-ipc` forces in. Amends D11. This deviates from spec/10's "C bindings only for croaring" and is flagged to the user (below). Also the docs refresh for the W1 merge. Next free decision: **D28** |
| `f7faf5f` | Merge `ti/w1-core`: `crates/ti-core` with bitmap rows (presence, set, BSI, count), BSI algorithms, a three-valued evaluator and a `MemorySource` fixture ([README](../crates/ti-core/README.md)). The lead re-verified it on the host: 46 root tests, `--features ti`, ti-core 8 edge + 1 golden + 4 property suites at 10,000 cases (38.6 s), strict clippy, fmt, 4 default deps. Baseline: a depth-16 compare over a full 65,536-column shard takes 41 µs (x86, not Pi). D23 (proptest) logged. **M1 gate items 1 and 2 done** |
| `00ff2f9` | Decisions: D22–D25 reservations recorded (D22 Prawn `ti-bench`, D23 proptest, D24/D25 Romantic Pike `bincode`/`crc32fast`). The next free number was **D26**, now assigned to W4 DataFusion |
| `682e66e` | Docs refresh (STATUS/SETUP) for the search merge and the W2 start |
| `1297968` | Merge `ti/search-api`: the in-process `lume::search` library. It adds `src/search.rs`, `main.rs` `handle_search` is now a thin adapter, and `agent.rs` `lume_search` runs in-process instead of shelling out. All 10 golden outputs in `tests/search_golden/` are byte-identical (verified in-container). Root tests are now 46. **Unblocks** W5 `match()`, W7 `ti_resolve` and `lume sql` |
| `a1ff6ab` | Docs refresh (STATUS/SETUP) for the contracts freeze and the W1 start |
| `48d0db1` | Decisions D20 (`raw` is a normalizing view over the real signalk-parquet layout) and D21 (ordinary set fields are single-valued per bucket). D18/D19 (`serde`, `toml` in `ti-contracts`) came with the freeze. New decisions continue at **D22+** |
| `96ac45d` | Merge `ti/w0-contracts` part 2, the **contracts freeze**. Adds `catalog.rs`, `config.rs` (`ti.toml` schema), `engine.rs` (`TiEngine` facade), `envelopes.rs` and `schemas.rs`, plus 27 contract tests. spec/10 now mirrors the crate source and spec/14 is fully filled in. The lead re-verified it on the host: 42 root and 27 contract tests, strict clippy, fmt, and the default build's 4 deps |
| `b09ac71` | Docs refresh (STATUS) for signalk-formats and the raw view |
| `b0ea0e1` | [design/signalk-formats.md](design/signalk-formats.md): Signal K delta, signalk-parquet, InfluxDB and History API formats, verified from source. Also [repo-fit §10](repo-fit.md): spec errata and the `raw` normalizing-view decision (now D20) |
| `d8b2a88` | Merge `ti/w0-corpus`: golden corpus part 1, 61 queries with DuckDB oracle twins in `tests/golden/` |
| `06dd5c5` | Merge `ti/w0-contracts` part 1: Cargo workspace, `ti` feature (off by default), `ti-contracts`, spec/14, D8–D15 |

## Agents

| Agent (pane name) | Pane | Lane/scope | Branch | Clone | Last known state |
|---|---|---|---|---|---|
| Industrial Pike | `ee764a09` | Lead and integrator. Fetches lane branches and merges them into `plan/lume-ti` | `plan/lume-ti` | shared tree | `8e87a11` |
| Rigid Roadrunner | `d58ca1b1` | **W4 `ti-sql`** ([lanes/W4](lanes/W4-sql.md)), the **critical path toward M3**. D26 (DataFusion `=55.1.0`) | `ti/w4-sql` | `.lanes/w4` | Interim report: 10 `ti-sql` tests green on the fixture. Its timestamp-literal fix is moving from SQL text rewriting to the plan level, for pgwire parameters. Clone: no commits on `f7faf5f` yet. Uncommitted work in progress: new `crates/ti-sql/`, plus changes to `Cargo.toml`, `Cargo.lock`, `src/main.rs` and spec/11 |
| Zygomorphic Prawn | `eccaf836` | **W0 corpus part 2**: exact geo/intervals oracles, 5 oracle-bug fixes from Roadrunner's peer review, `tests/golden/raw_view.sql`, new `crates/ti-bench` generator that writes the real signalk-parquet layout (D22), determinism test, all 61 oracles executed against generated data. Expected outputs come in part 3, after the signalk-parquet layout is verified. **M3 waits on these expected outputs** | `ti/w0-corpus` | `.lanes/corpus` | Unchanged since last pass: 4 commits ahead of `48d0db1` (`bedb057`, `b146deb`, `13739dc`, `681440c`). Uncommitted work in progress: new `crates/ti-bench/`, plus changes to `Cargo.toml` and `Cargo.lock` |
| Romantic Pike | `90fc608c` | **W3 `ti-ingest`** ([lanes/W3](lanes/W3-ingest.md)) | `ti/w3-ingest` | `.lanes/w3` | Cut from `8e87a11`. No commits yet, clean tree. W2 merged (`cf98c61`). Its old `.lanes/w2` clone is retired |
| Regular Pheasant | `364a3fc7` | Host build pane (PowerShell 7, Windows Rust toolchain). Not an agent | — | — | Agents may split it once each |

How to refresh the last column: `git -C .lanes/<x> log --oneline -5` and `git -C .lanes/<x> status --short`.

## Milestones

Week numbers count from kickoff. A milestone closes only when every gate test passes in CI ([spec/12](spec/12-milestones.md)).

| Milestone | Lanes | Weeks | Gate status |
|---|---|---|---|
| M0 Contracts | W0 | 1 | **In progress.** 2/4 gate items done (see below) |
| M1 Core and store | W1, W2 | 2–4 | **Complete** (`cf98c61`). All 4 gate items done in week 1, against a week 2–4 plan |
| M2 Ingest | W3 | 2–5 | **W3 started** (Romantic Pike) |
| M3 SQL and pushdown | W4 | 2–6 | **In progress** (Rigid Roadrunner, critical path). 10 `ti-sql` fixture tests green (interim). Waits on corpus expected outputs |
| M4 Text, geo, intervals | W4, W5, W6 | 6–8 | Not started |
| M5 Agent surface | W7 | 7–9 | Not started |
| M6 Fleet and benchmarks | W8, integrator | 9–12 | Not started |

### M0 gate detail

| # | Gate item | State |
|---|---|---|
| 1 | `ti-contracts` merged with types, traits, doc comments | **Done.** Frozen in `96ac45d` |
| 2 | `ti.toml` schema with defaults | **Done.** `config.rs` in `96ac45d` |
| 3 | `ti-bench gen` reproduces the correctness set byte-identically from a seed | In progress (Zygomorphic Prawn, corpus part 2) |
| 4 | ≥ 60 golden queries with oracle twins and expected output | In progress. 61 queries merged. Oracle fixes are committed in the lane (`bedb057`, `13739dc`, `681440c`) but not merged yet. Expected outputs come in part 3 |

### M1 gate detail

| # | Gate item | Owner | State |
|---|---|---|---|
| 1 | BSI compare, sum, min, max agree with a naive model on 10,000 proptest cases, including negatives and depth growth | W1 | **Done** (`f7faf5f`) |
| 2 | `Predicate::eval` agrees with the naive model for random AND/OR/NOT trees of depth ≤ 4 | W1 | **Done** (`f7faf5f`) |
| 3 | Crash test: 1,000 kill -9 runs during flush, zero lost or duplicated records after replay | W2 | **Done** (`cf98c61`). Seeded kill points: 332 mid-apply, 344 mid-flush, 324 between flush and truncate |
| 4 | Seal produces byte-identical files and hashes from identical input | W2 | **Done** (`cf98c61`) |

Pre-work outside the milestones: search library extraction is **done** (`1297968`). `lume sql` over plain indexes now waits only on the W4 DataFusion pin (after M3).

## Open decisions waiting on the user

From [spec/11-risks-decisions.md](spec/11-risks-decisions.md) (open questions):

1. Default bucket width: 10 s, or 1 s with 1-min rollups? (1 s makes the index 10× larger)
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

Spec deviations needing spec-owner confirmation:

14. **D27: accept `zstd-sys` (C)**, which DataFusion 55.1.0 forces in through `arrow-ipc` even with default features off. This amends D11 and deviates from spec/10, which allows C bindings only for `croaring` after M4. Flagged to the user by the lead.

## Open items (team, not user)

- [ ] Verify [signalk-formats §1.3](design/signalk-formats.md) (raw-tier Parquet schema) against a real `.parquet` file with DuckDB `DESCRIBE`. The doc comes from source reading only, and expected outputs (corpus part 3) wait on this.
- [x] Assign a D-number to the `raw` normalizing-view decision. It is D20 (`48d0db1`).
- [ ] Strict TI checks (`clippy -D warnings`, `fmt --check` per `crates/ti-*`) are not in `ci.yml` yet.
- [ ] **Toolchain drift:** the host runs rustc 1.96.1 and the containers run 1.99, and their clippy lints differ (`8e87a11` fixed a lint only 1.96 reported). The lead is proposing a pinned `rust-toolchain.toml` to the user.

Also undecided: the repo-fit items in [repo-fit.md](repo-fit.md), such as the W7a/W7b split (§6). These are lead decisions unless escalated (unconfirmed).
