# Lume TI — Status board

Last updated: **2026-10-06 04:04 UTC** (docs keeper, after `b0ea0e1`: Signal K formats + repo-fit §10)

Integration branch `plan/lume-ti` is at `b0ea0e1`.
Setup and workflow: [SETUP.md](SETUP.md).

## Recent merges into `plan/lume-ti`

| Commit | What |
|---|---|
| `b0ea0e1` | [design/signalk-formats.md](design/signalk-formats.md): Signal K delta, signalk-parquet, InfluxDB and History API formats, verified from plugin and server source. Also [repo-fit §10](repo-fit.md): spec errata, plus the lead decision that `raw` is a **normalizing DuckDB view** over the real layout (`context, ts TIMESTAMP, path, value DOUBLE, value_str VARCHAR, source VARCHAR` + flattened object keys). Corpus oracles target that view. It gets a D-number at the next merge (agents use D18+) |
| `898af2e` | Docs refresh (STATUS/SETUP) after the W0 part 1 and corpus merges |
| `d8b2a88` | Merge `ti/w0-corpus`: golden corpus part 1, 61 queries with DuckDB oracle twins in `tests/golden/`. Known issues are fixed in part 2 (below) |
| `fde14ba` | Decisions D16 (WAL group commit) and D17 (contract derives) |
| `06dd5c5` | Merge `ti/w0-contracts`: W0 part 1. Cargo workspace (root at `.`, member `crates/ti-contracts`, `ti` feature off by default), `ti-contracts` crate, `spec/14-semantics.md`, decisions D8–D15. The lead re-verified it on the host: build, 42 root tests, `--features ti`, 4 contracts tests, `clippy -D warnings -p ti-contracts` and `fmt -p ti-contracts` all pass, and the default build keeps 4 deps |

## Agents

| Agent (pane name) | Pane | Lane/scope | Branch | Clone | Last known state |
|---|---|---|---|---|---|
| Industrial Pike | `ee764a09` | Lead and integrator. Fetches lane branches and merges them into `plan/lume-ti` | `plan/lume-ti` | shared tree | `b0ea0e1` |
| Rigid Roadrunner | `d58ca1b1` | **W0 part 2, the contracts freeze**: D17 derives, `FieldValue::Clear`, semantics gaps 2/3/6 (catalog, Arrow schemas, versioned envelopes), `ti.toml` schema, path list agreed with Prawn | `ti/w0-contracts` | `.lanes/w0` | Part 1 merged. Clone moved up to `d8b2a88`. No part 2 commits yet. Uncommitted work in progress: `crates/ti-contracts/` `Cargo.toml` and `lib.rs`, plus new `catalog.rs`, `engine.rs`, `envelopes.rs` and `schemas.rs` |
| Zygomorphic Prawn | `eccaf836` | **W0 corpus part 2**: exact geo/intervals oracles, 5 oracle-bug fixes from Roadrunner's peer review, `tests/golden/raw_view.sql` (the normalizing `raw` view), new `crates/ti-bench` generator that writes the real signalk-parquet layout, determinism test, all 61 oracles executed against generated data. Expected outputs come in part 3, after the signalk-parquet layout is verified | `ti/w0-corpus` | `.lanes/corpus` | Part 1 merged. Clone moved up to `d8b2a88`. No part 2 commits, clean tree |
| Romantic Pike | `90fc608c` | Search library extraction ([design/search-api.md](design/search-api.md)). Unchanged | `ti/search-api` | `.lanes/search` | 1 commit ahead (`46153fa` golden search baselines). Uncommitted work in progress: `src/agent.rs`, `src/hybrid.rs`, `src/lib.rs`, new `src/search.rs`. Based on `baacfbd`, so it is behind `plan/lume-ti` |
| Regular Pheasant | `364a3fc7` | Host build pane (PowerShell 7, Windows Rust toolchain). Not an agent | — | — | Agents may split it once each |

How to refresh the last column: `git -C .lanes/<x> log --oneline -5` and `git -C .lanes/<x> status --short`.

## Milestones

Week numbers count from kickoff. A milestone closes only when every gate test passes in CI ([spec/12](spec/12-milestones.md)).

| Milestone | Lanes | Weeks | Gate status |
|---|---|---|---|
| M0 Contracts | W0 | 1 | **In progress.** 0/4 gate items passed (see below) |
| M1 Core and store | W1, W2 | 2–4 | Not started |
| M2 Ingest | W3 | 2–5 | Not started |
| M3 SQL and pushdown | W4 | 2–6 | Not started |
| M4 Text, geo, intervals | W4, W5, W6 | 6–8 | Not started |
| M5 Agent surface | W7 | 7–9 | Not started |
| M6 Fleet and benchmarks | W8, integrator | 9–12 | Not started |

### M0 gate detail

| # | Gate item | State |
|---|---|---|
| 1 | `ti-contracts` merged with types, traits, doc comments | **Partial.** Merged in `06dd5c5`. The freeze (D17 derives, `FieldValue::Clear`, gaps 2/3/6) is pending in W0 part 2 |
| 2 | `ti.toml` schema with defaults | In progress (W0 part 2) |
| 3 | `ti-bench gen` reproduces the correctness set byte-identically from a seed | In progress (corpus part 2: `crates/ti-bench` + determinism test) |
| 4 | ≥ 60 golden queries with oracle twins and expected output | Not passed. 61 queries merged. Oracles are being corrected (5 bugs, exact geo/intervals) in corpus part 2. Expected outputs come in part 3 |

Pre-work outside the milestones: search library extraction is in progress (golden baselines committed, extraction uncommitted). `lume sql` over plain indexes waits until after M3.

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
- [ ] Assign a D-number to the `raw` normalizing-view decision ([repo-fit §10](repo-fit.md)).

Also undecided: the repo-fit items in [repo-fit.md](repo-fit.md), such as the W7a/W7b split (§6) and the CI strictness scope (§7). These are lead decisions unless escalated (unconfirmed).
