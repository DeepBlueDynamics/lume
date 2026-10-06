# Lume TI — Status board

Last updated: **2026-10-06 04:15 UTC** (docs keeper, after `48d0db1`: contracts freeze + D20/D21)

Integration branch `plan/lume-ti` is at `48d0db1`. **`ti-contracts` is frozen** (`96ac45d`).
Setup and workflow: [SETUP.md](SETUP.md).

## Recent merges into `plan/lume-ti`

| Commit | What |
|---|---|
| `48d0db1` | Decisions D20 (`raw` is a normalizing view over the real signalk-parquet layout) and D21 (ordinary set fields are single-valued per bucket). D18/D19 (`serde`, `toml` in `ti-contracts`) came with the freeze. New decisions continue at **D22+** |
| `96ac45d` | Merge `ti/w0-contracts` part 2, the **contracts freeze**. Adds `catalog.rs`, `config.rs` (`ti.toml` schema), `engine.rs` (`TiEngine` facade), `envelopes.rs` and `schemas.rs`, plus 27 contract tests. spec/10 now mirrors the crate source and spec/14 is fully filled in. The lead re-verified it on the host: 42 root and 27 contract tests, strict clippy, fmt, and the default build's 4 deps |
| `b09ac71` | Docs refresh (STATUS) for signalk-formats and the raw view |
| `b0ea0e1` | [design/signalk-formats.md](design/signalk-formats.md): Signal K delta, signalk-parquet, InfluxDB and History API formats, verified from source. Also [repo-fit §10](repo-fit.md): spec errata and the `raw` normalizing-view decision (now D20) |
| `d8b2a88` | Merge `ti/w0-corpus`: golden corpus part 1, 61 queries with DuckDB oracle twins in `tests/golden/` |
| `06dd5c5` | Merge `ti/w0-contracts` part 1: Cargo workspace, `ti` feature (off by default), `ti-contracts`, spec/14, D8–D15 |

## Agents

| Agent (pane name) | Pane | Lane/scope | Branch | Clone | Last known state |
|---|---|---|---|---|---|
| Industrial Pike | `ee764a09` | Lead and integrator. Fetches lane branches and merges them into `plan/lume-ti` | `plan/lume-ti` | shared tree | `48d0db1` |
| Rigid Roadrunner | `d58ca1b1` | **W1 `ti-core`** ([lanes/W1](lanes/W1-core.md)): row types, BSI algorithms, three-valued predicate evaluator, naive model, proptests | `ti/w1-core` | `.lanes/w1` | Just cut from `48d0db1`. No commits yet, clean tree. Its old `.lanes/w0` clone is retired |
| Zygomorphic Prawn | `eccaf836` | **W0 corpus part 2**: exact geo/intervals oracles, 5 oracle-bug fixes from Roadrunner's peer review, `tests/golden/raw_view.sql`, new `crates/ti-bench` generator that writes the real signalk-parquet layout, determinism test, all 61 oracles executed against generated data. Expected outputs come in part 3, after the signalk-parquet layout is verified | `ti/w0-corpus` | `.lanes/corpus` | 1 commit ahead of `b0ea0e1`: `bedb057`, exact oracles + review fixes + `raw_view.sql`. Clean tree. No `ti-bench` yet. Behind `plan/lume-ti` (doesn't have the freeze) |
| Romantic Pike | `90fc608c` | Search library extraction ([design/search-api.md](design/search-api.md)). Unchanged | `ti/search-api` | `.lanes/search` | 3 lane commits (`46153fa` baselines, `a96ae8c` extract the in-process search API, `7cff93f` CLI parity + warning cleanup), plus `638c483`, a merge of `plan/lume-ti` at `b09ac71`. Clean tree. Behind `plan/lume-ti` (doesn't have the freeze) |
| Regular Pheasant | `364a3fc7` | Host build pane (PowerShell 7, Windows Rust toolchain). Not an agent | — | — | Agents may split it once each |

How to refresh the last column: `git -C .lanes/<x> log --oneline -5` and `git -C .lanes/<x> status --short`.

## Milestones

Week numbers count from kickoff. A milestone closes only when every gate test passes in CI ([spec/12](spec/12-milestones.md)).

| Milestone | Lanes | Weeks | Gate status |
|---|---|---|---|
| M0 Contracts | W0 | 1 | **In progress.** 2/4 gate items done (see below) |
| M1 Core and store | W1, W2 | 2–4 | **W1 started** (Rigid Roadrunner). W2 not assigned |
| M2 Ingest | W3 | 2–5 | Not started |
| M3 SQL and pushdown | W4 | 2–6 | Not started |
| M4 Text, geo, intervals | W4, W5, W6 | 6–8 | Not started |
| M5 Agent surface | W7 | 7–9 | Not started |
| M6 Fleet and benchmarks | W8, integrator | 9–12 | Not started |

### M0 gate detail

| # | Gate item | State |
|---|---|---|
| 1 | `ti-contracts` merged with types, traits, doc comments | **Done.** Frozen in `96ac45d` |
| 2 | `ti.toml` schema with defaults | **Done.** `config.rs` in `96ac45d` |
| 3 | `ti-bench gen` reproduces the correctness set byte-identically from a seed | In progress (Zygomorphic Prawn, corpus part 2) |
| 4 | ≥ 60 golden queries with oracle twins and expected output | In progress. 61 queries merged. Oracle fixes are committed in the lane (`bedb057`) but not merged yet. Expected outputs come in part 3 |

Pre-work outside the milestones: search library extraction is committed in its lane and not merged yet. `lume sql` over plain indexes waits until after M3.

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

## Open items (team, not user)

- [ ] Verify [signalk-formats §1.3](design/signalk-formats.md) (raw-tier Parquet schema) against a real `.parquet` file with DuckDB `DESCRIBE`. The doc comes from source reading only, and expected outputs (corpus part 3) wait on this.
- [x] Assign a D-number to the `raw` normalizing-view decision. It is D20 (`48d0db1`).
- [ ] Strict TI checks (`clippy -D warnings`, `fmt --check` per `crates/ti-*`) are not in `ci.yml` yet.

Also undecided: the repo-fit items in [repo-fit.md](repo-fit.md), such as the W7a/W7b split (§6). These are lead decisions unless escalated (unconfirmed).
