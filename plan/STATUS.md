# Lume TI — Status board

Last updated: **2026-10-06 05:54 UTC** (docs keeper, after `98816b5`: W4 SQL foundation merged)

Integration branch `plan/lume-ti` is at `98816b5`. **`ti-contracts` is frozen** (`96ac45d`). Root tests: 46.
Workspace members: `ti-contracts`, `ti-core`, `ti-store`, `ti-sql`. Next free decision: **D30** (ask the lead before taking it).
Setup and workflow: [SETUP.md](SETUP.md).

## Critical path right now

**The regenerated correctness set blocks M0 items 3 and 4, M2 and M3.** Zygomorphic Prawn committed the generator (`86f0ffb`, not merged yet) and is regenerating the set (~1.9 GB) into the shared data dir `.lanes/data/correctness` (`/workspace/lume/.lanes/data/correctness` in containers).

## Recent merges into `plan/lume-ti`

| Commit | What |
|---|---|
| `98816b5` | Merge the `ti/w4-sql` **foundation** (`705508d`): `crates/ti-sql` with DataFusion `=55.1.0` (default features off), `telemetry`/`docs`/`raw`/catalog providers, a pushdown classifier, the W1-backed materializer, a plan-level timestamp `FunctionRewrite` (prepared `$1` parameters stay Exact, and the internal UDF is hidden), and a stored-output verify harness. Host-verified on rustc 1.96: 12 `ti-sql` tests, strict clippy, fmt. No bzip2/lzma/liblzma in the `--features ti` graph, and the default build keeps 4 deps. A cold `--features ti` build takes **~10 min** on the host. **M3 is not complete.** It waits on corpus expected outputs |
| `20d1b96` | D28 (`tungstenite`) and D29 (`parquet`) reserved for W3. Next free: D30 |
| `1d77ff7` | Docs refresh for the W2 merge, M1 complete, the W3 start and D27 |
| `8e87a11` | Lead clippy fix in `ti-store` for rustc 1.96 on the host (the containers use 1.99) |
| `cf98c61` | Merge `ti/w2-store`: WAL with D16 group commit, open shard, deterministic seal, repair versions, catalog/manifest with dir fsync, plain-read path (no mmap, no `unsafe`). The 1,000-run crash test uses seeded kill points, a reference model and D21 checks. **M1 complete** |
| `922fc07` | **D27**: accept `zstd-sys` (C), which DataFusion's `arrow-ipc` forces in. Amends D11. Awaiting the user (below) |
| `f7faf5f` | Merge `ti/w1-core`: bitmap rows, BSI algorithms, three-valued evaluator, `MemorySource` ([README](../crates/ti-core/README.md)). 10,000-case property suites. The depth-16 compare over a 65,536-column shard takes 41 µs (x86) |

Earlier: `1297968` search library (`lume::search`, 10/10 byte-identical goldens), `96ac45d` contracts freeze, `48d0db1` D20/D21, `b0ea0e1` [signalk-formats](design/signalk-formats.md) + repo-fit §10, `d8b2a88` corpus part 1 (61 queries), `06dd5c5` workspace + `ti-contracts` part 1. Full list: `git log --oneline --first-parent plan/lume-ti`.

## Agents

| Agent (pane name) | Pane | Lane/scope | Branch | Clone | Last known state |
|---|---|---|---|---|---|
| Industrial Pike | `ee764a09` | Lead and integrator. Fetches lane branches and merges them into `plan/lume-ti` | `plan/lume-ti` | shared tree | `98816b5` |
| Rigid Roadrunner | `d58ca1b1` | **W4 `ti-sql`** ([lanes/W4](lanes/W4-sql.md)), the critical path to M3. D26 | `ti/w4-sql` | `.lanes/w4` | Foundation merged. Real-Store SQL path green (13 tests). Patching a `ti-store` bug it found (non-null list items, sorted distinct sourceRefs) as a separate `ti-store`-only commit, approved by the lead. Clone: uncommitted changes in `crates/ti-sql/` (several files), `crates/ti-store/src/shard.rs` and `Cargo.lock` |
| Romantic Pike | `90fc608c` | **W3 `ti-ingest`** ([lanes/W3](lanes/W3-ingest.md)). D28, D29 | `ti/w3-ingest` | `.lanes/w3` | Foundation done at `058d226` (20 tests). It has merged `plan/lume-ti` at `98816b5` (`22519a9`) ahead of its own merge. Uncommitted change in `crates/ti-ingest/src/decode.rs`. The M2 gate is still open (see below) |
| Zygomorphic Prawn | `eccaf836` | **W0 corpus part 2/3**: exact oracles and review fixes, `raw_view.sql`, the `ti-bench` generator (D22), determinism test, oracles run on generated data, then expected outputs | `ti/w0-corpus` | `.lanes/corpus` | Generator committed (`86f0ffb`, not merged). Lane has merged `plan/lume-ti` at `20d1b96` (`de13bdc`). Clean tree. Regenerating the correctness set into `.lanes/data/correctness` |
| Regular Pheasant | `364a3fc7` | Host build pane (PowerShell 7, Windows Rust toolchain, rustc 1.96.1). Not an agent | — | — | Agents may split it once each. Cold `--features ti` builds take ~10 min, so don't start parallel cold builds |

How to refresh the last column: `git -C .lanes/<x> log --oneline -5` and `git -C .lanes/<x> status --short`.

## Milestones

Week numbers count from kickoff. A milestone closes only when every gate test passes in CI ([spec/12](spec/12-milestones.md)).

| Milestone | Lanes | Weeks | Gate status |
|---|---|---|---|
| M0 Contracts | W0 | 1 | **In progress.** 2/4 done. Items 3 and 4 wait on the correctness data |
| M1 Core and store | W1, W2 | 2–4 | **Complete** (`cf98c61`). Done in week 1, against a week 2–4 plan |
| M2 Ingest | W3 | 2–5 | **In progress.** Foundation done (`058d226`). 0/3 gate items passed, all waiting on the correctness data or a measurement |
| M3 SQL and pushdown | W4 | 2–6 | **In progress.** Foundation merged (`98816b5`), real-Store path green. Waits on corpus expected outputs |
| M4 Text, geo, intervals | W4, W5, W6 | 6–8 | Not started |
| M5 Agent surface | W7 | 7–9 | Not started |
| M6 Fleet and benchmarks | W8, integrator | 9–12 | Not started |

### M0 gate detail

| # | Gate item | State |
|---|---|---|
| 1 | `ti-contracts` merged with types, traits, doc comments | **Done** (`96ac45d`) |
| 2 | `ti.toml` schema with defaults | **Done** (`96ac45d`) |
| 3 | `ti-bench gen` reproduces the correctness set byte-identically from a seed | In progress. Generator committed (`86f0ffb`, not merged). Regeneration running |
| 4 | ≥ 60 golden queries with oracle twins and expected output | In progress. 61 queries merged. Oracle fixes are in the lane, not merged yet. Expected outputs wait on the data |

### M1 gate detail

All 4 items done: BSI vs naive model and `Predicate::eval` vs naive model (`f7faf5f`, W1), and the 1,000-run kill -9 crash test and seal determinism (`cf98c61`, W2).

### M2 gate detail

| # | Gate item | State |
|---|---|---|
| 1 | Replaying a recorded 24 h delta log yields `BucketRecord`s equal to oracle bucketing | Open |
| 2 | Parquet backfill of the correctness set is idempotent (identical manifest hashes on a second run) | Open. Needs the real correctness set |
| 3 | Pi 5 sustains 20,000 values/s for 1 h within the CPU and RSS budget | Open. No throughput number yet |

### M3 gate detail

| # | Gate item | State |
|---|---|---|
| 1 | All non-text, non-geo golden queries match the oracle (fixture, then real store) | Open. Verify harness is merged. Waits on expected outputs |
| 2 | `EXPLAIN` shows Exact pushdown for every expression the pushdown table marks Exact | Open. Pushdown classifier merged (unconfirmed whether this gate has been checked) |
| 3 | `raw` table queries the same Parquet and matches DuckDB exactly | Open. `raw` provider merged. Waits on the data |

Pre-work outside the milestones: search library extraction is **done** (`1297968`). `lume sql` over plain indexes can now build on the merged DataFusion pin. It is still planned for after M3.

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

Spec deviations and proposals needing the user:

14. **D27: accept `zstd-sys` (C)**, which DataFusion 55.1.0 forces in through `arrow-ipc` even with default features off. This amends D11 and deviates from spec/10, which allows C bindings only for `croaring` after M4. Flagged by the lead for spec-owner confirmation.
15. **Pinned `rust-toolchain.toml`**, proposed by the lead because the host (rustc 1.96.1) and the containers (1.99) report different clippy lints.

## Open items (team, not user)

- [ ] Correctness set regeneration into `.lanes/data/correctness` (Zygomorphic Prawn). Blocks M0 3/4, M2 and M3.
- [ ] Verify [signalk-formats §1.3](design/signalk-formats.md) (raw-tier Parquet schema) against a real `.parquet` file with DuckDB `DESCRIBE`. The doc comes from source reading only, and expected outputs (corpus part 3) wait on this.
- [ ] `ti-store` bug fix from W4 (non-null list items, sorted distinct sourceRefs), as a separate commit.
- [ ] Strict TI checks (`clippy -D warnings`, `fmt --check` per `crates/ti-*`) are not in `ci.yml` yet.
- [ ] Toolchain drift between the host and the containers (see item 15 above).

Also undecided: the repo-fit items in [repo-fit.md](repo-fit.md), such as the W7a/W7b split (§6). These are lead decisions unless escalated (unconfirmed).
