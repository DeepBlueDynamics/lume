# Lume TI — Status board

Last updated: **2026-10-06 07:25 UTC** (docs keeper, after `33c67b1`: W4 store fix + SQL Store adapter merged)

Integration branch `plan/lume-ti` is at `33c67b1`. **`ti-contracts` is frozen** (`96ac45d`). Root tests: 46.
Workspace members: `ti-contracts`, `ti-core`, `ti-store`, `ti-sql`, `ti-ingest`. Next free decision: **D30** (ask the lead before taking it).
Setup and workflow: [SETUP.md](SETUP.md).

## Critical path right now

- **Corpus and generator work has no owner.** Zygomorphic Prawn is **offline**. Its generator is committed in `.lanes/corpus` at `86f0ffb` (clean tree, not merged), and the `--start/--end/--days` flags sit on side branch `ti/bench-days` (`d3e8f66`) in `.lanes/w3`. **M0 items 3 and 4, M2 and M3 all wait on it.**
- **`.lanes/data/correctness` is INCOMPLETE. DO NOT USE IT.** Generation was interrupted at about 1.5 of ~1.9 GB.

## Pane changes (about 06:00 UTC)

The original agent panes closed. Their sessions were restored into new panes with new container identities.

| Role | Now | Formerly |
|---|---|---|
| W4 SQL owner | **Long Horse** `888bff45`, nemesis8/n8-hazy-badger | Rigid Roadrunner `d58ca1b1`, n8-sly-viper |
| W3 ingest owner | **Artificial Shark** `fd91f4b1`, nemesis8/n8-quiet-crane | Romantic Pike `90fc608c`, n8-keen-kiwi |
| Corpus and generator | **offline** | Zygomorphic Prawn `eccaf836`, n8-noble-toad |
| Host build pane | **Compact Echidna** `6914c38e` | Regular Pheasant `364a3fc7` (gone) |

Commit messages, the decisions log and older docs use the former names.

## Recent merges into `plan/lume-ti`

| Commit | What |
|---|---|
| `33c67b1` | Merge `ti/w4-sql`. `2b89b96`, the **ti-store fix**: non-null source/geo list items, sorted distinct sourceRefs per spec/14, dictionary errors propagate, plus a `read_lists` regression test. `0469f1d`, the **durable SQL Store adapter**: catalogs, open and sealed shard metadata, canonical projection normalization, and a `--store` verifier mode. `5631f86` merges W3 `46d69f4` into W4, with the W3 integration tests passing against the patched store. Host-verified on rustc 1.96: 46 root tests; ti-store 10 unit + crash (36 s) + list/schema + seal; ti-sql 13; ti-ingest 21; strict clippy and fmt clean on all three |
| `bcfddca` | Docs refresh for the pane changes and the W3 foundation merge |
| `46d69f4` | Merge the `ti/w3-ingest` **foundation** (`b175642`): decode (source label delegating to `ti_contracts::normalized_source_label`, with a parity test), normalize, classify, bucketer, watermark, derived fields, WebSocket/Parquet/InfluxDB sources, recorder/replay. Host-verified: 46 root tests, 21 `ti-ingest` tests, rustc 1.96 strict clippy, fmt, `--features ti` build (3 min 9 s), no bzip2/lzma/liblzma. `ring` is present only through root lume's existing `ureq`/rustls, not TI. **M2 is not complete** |
| `3ac046d` | Docs refresh for the W4 foundation merge and the shared data dir |
| `98816b5` | Merge the `ti/w4-sql` **foundation**: DataFusion `=55.1.0` (defaults off), telemetry/docs/raw/catalog providers, pushdown classifier, W1-backed materializer, plan-level timestamp rewrite (prepared `$1` stays Exact), stored-output verify harness. 12 `ti-sql` tests. **M3 is not complete** |
| `20d1b96` | D28 (`tungstenite`) and D29 (`parquet`) reserved for W3. Next free: D30 |
| `cf98c61` | Merge `ti/w2-store` (WAL group commit, deterministic seal, repair, 1,000-run crash test). **M1 complete** |
| `922fc07` | **D27**: accept `zstd-sys` (C), forced in by DataFusion's `arrow-ipc`. Amends D11. Awaiting the user |
| `f7faf5f` | Merge `ti/w1-core` (bitmap rows, BSI, three-valued evaluator; [README](../crates/ti-core/README.md)) |

Earlier: `1297968` search library, `96ac45d` contracts freeze, `48d0db1` D20/D21, `b0ea0e1` [signalk-formats](design/signalk-formats.md), `d8b2a88` corpus part 1 (61 queries), `06dd5c5` workspace + `ti-contracts` part 1. Full list: `git log --oneline --first-parent plan/lume-ti`.

## Agents

| Agent (pane name) | Pane | Lane/scope | Branch | Clone | Last known state |
|---|---|---|---|---|---|
| Industrial Pike | `ee764a09` | Lead and integrator. Fetches lane branches and merges them into `plan/lume-ti`. For Long Horse's merges, it also runs fmt and 1.96 clippy on the host | `plan/lume-ti` | shared tree | `46d69f4` |
| Long Horse (formerly Rigid Roadrunner) | `888bff45` | **W4 part 2**: the M4 items that don't depend on W5/W6. That is the `intervals()` table function, a `BitmapAggregateExec` optimizer rule, `EXPLAIN` reporting and the ≥ 10× benchmark ([lanes/W4](lanes/W4-sql.md)) | `ti/w4-sql` | `.lanes/w4` | Part 1 fully merged (`33c67b1`). Clone at `33c67b1`, clean tree. Its container lacks rustfmt and clippy, so the lead runs them on the host at merge |
| Artificial Shark (formerly Romantic Pike) | `fd91f4b1` | **W3 `ti-ingest`** ([lanes/W3](lanes/W3-ingest.md)): the M2 harness | `ti/w3-ingest` | `.lanes/w3` | Foundation merged. Uncommitted work in progress: the M2 harness (new `tests/m2_gate.rs`, plus changes to `decode.rs`, `lib.rs`, `parquet.rs` and `recorder.rs`), running against `.lanes/data/w3-smoke`. Also carries side branch **`ti/bench-days`** (`d3e8f66`, on top of `86f0ffb`), which adds `ti-bench --start/--end/--days` flags. It merges together with the corpus lane |
| Zygomorphic Prawn | `eccaf836` | W0 corpus parts 2–3 and the `ti-bench` generator | `ti/w0-corpus` | `.lanes/corpus` | **OFFLINE.** Clone is clean at `86f0ffb` (generator committed, not merged). Artificial Shark restored it (see note below) |
| Compact Echidna (formerly Regular Pheasant) | `6914c38e` | Host build pane (PowerShell 7, Windows Rust toolchain, rustc 1.96.1). Not an agent | — | — | Agents may split it once each. Cold `--features ti` builds take a long time (~10 min cold, 3 min 9 s at the W3 merge), so don't start parallel cold builds |

Note: Artificial Shark restored `.lanes/corpus` to a clean `86f0ffb`. The uncommitted `ti-bench` change that was there is preserved as `d3e8f66` on `ti/bench-days` in `.lanes/w3`.

How to refresh the last column: `git -C .lanes/<x> log --oneline -5` and `git -C .lanes/<x> status --short`.

## Milestones

Week numbers count from kickoff. A milestone closes only when every gate test passes in CI ([spec/12](spec/12-milestones.md)).

| Milestone | Lanes | Weeks | Gate status |
|---|---|---|---|
| M0 Contracts | W0 | 1 | **In progress, blocked.** 2/4 done. Items 3 and 4 need the corpus/generator owner |
| M1 Core and store | W1, W2 | 2–4 | **Complete** (`cf98c61`) |
| M2 Ingest | W3 | 2–5 | **In progress.** Foundation merged (`46d69f4`). 0/3 gate items passed |
| M3 SQL and pushdown | W4 | 2–6 | **In progress, blocked.** Foundation and durable Store adapter merged (`98816b5`, `33c67b1`). Waits on corpus expected outputs |
| M4 Text, geo, intervals | W4, W5, W6 | 6–8 | **W4 part 2 started** (Long Horse): `intervals()`, `BitmapAggregateExec`, `EXPLAIN`, ≥ 10× bench. W5 and W6 not assigned |
| M5 Agent surface | W7 | 7–9 | Not started |
| M6 Fleet and benchmarks | W8, integrator | 9–12 | Not started |

### M0 gate detail

| # | Gate item | State |
|---|---|---|
| 1 | `ti-contracts` merged with types, traits, doc comments | **Done** (`96ac45d`) |
| 2 | `ti.toml` schema with defaults | **Done** (`96ac45d`) |
| 3 | `ti-bench gen` reproduces the correctness set byte-identically from a seed | **Blocked.** Generator at `86f0ffb` (not merged). Owner offline. Correctness set incomplete |
| 4 | ≥ 60 golden queries with oracle twins and expected output | **Blocked.** 61 queries merged. The oracle fixes in the lane aren't merged. No expected outputs |

### M2 gate detail

| # | Gate item | State |
|---|---|---|
| 1 | Replaying a recorded 24 h delta log yields `BucketRecord`s equal to oracle bucketing | Open. Recorder/replay merged. Harness in progress on `.lanes/data/w3-smoke` |
| 2 | Parquet backfill of the correctness set is idempotent | Open. Needs the real correctness set (blocked) |
| 3 | Pi 5 sustains 20,000 values/s for 1 h within the CPU and RSS budget | Open. No throughput number yet |

### M3 gate detail

| # | Gate item | State |
|---|---|---|
| 1 | All non-text, non-geo golden queries match the oracle (fixture, then real store) | Open. Verify harness merged. Waits on expected outputs |
| 2 | `EXPLAIN` shows Exact pushdown for every expression marked Exact | Open. Classifier merged (unconfirmed whether this gate has been checked) |
| 3 | `raw` table queries the same Parquet and matches DuckDB exactly | Open. Waits on the data |

M1: all 4 gate items done (`f7faf5f`, `cf98c61`). Pre-work: search library done (`1297968`). `lume sql` is planned for after M3.

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

14. **D27: accept `zstd-sys` (C)**, which DataFusion 55.1.0 forces in through `arrow-ipc` even with default features off. This amends D11 and deviates from spec/10 (C bindings only for `croaring`). Flagged by the lead for spec-owner confirmation.
15. **Pinned `rust-toolchain.toml`**, proposed because the host (rustc 1.96.1) and the containers (1.99) report different clippy lints.

## Open items (team, not user)

- [ ] **Restore Zygomorphic Prawn, or reassign the corpus/generator work.** Blocks M0 items 3–4, M2 and M3. When it is picked up, merge `ti/bench-days` (`d3e8f66`) with the corpus lane.
- [ ] Regenerate `.lanes/data/correctness` completely (currently 1.5 of ~1.9 GB, **do not use**).
- [ ] Verify [signalk-formats §1.3](design/signalk-formats.md) (raw-tier Parquet schema) against a real `.parquet` file with DuckDB `DESCRIBE`.
- [x] Long Horse: `ti-store` fix and SQL Store adapter as separate commits. Merged in `33c67b1` (`2b89b96`, `0469f1d`).
- [ ] Long Horse's container has no rustfmt or clippy. The lead runs both on the host at merge. Restore them in the container (unconfirmed whether planned).
- [ ] Strict TI checks (`clippy -D warnings`, `fmt --check` per `crates/ti-*`) are not in `ci.yml` yet.
- [ ] Toolchain drift between the host and the containers (item 15 above).

Also undecided: the repo-fit items in [repo-fit.md](repo-fit.md), such as the W7a/W7b split (§6). These are lead decisions unless escalated (unconfirmed).
