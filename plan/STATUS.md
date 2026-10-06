# Lume TI — Status board

Last updated: **2026-10-06 03:54 UTC** (docs keeper, first pass)

Integration branch `plan/lume-ti` is at `510a2df` (gitignore: per-agent lane clones under `.lanes/`).
Setup and workflow: [SETUP.md](SETUP.md).

## Agents

| Agent (pane name) | Pane | Lane/scope | Branch | Clone | Last known state |
|---|---|---|---|---|---|
| Industrial Pike | `ee764a09` | Lead and integrator. Fetches lane branches and merges them into `plan/lume-ti` | `plan/lume-ti` | shared tree | `510a2df`, clean |
| Rigid Roadrunner | `d58ca1b1` | W0 contracts part 1 (workspace, `ti-contracts`, decisions D8+) | `ti/w0-contracts` | `.lanes/w0` | 0 commits ahead. Uncommitted work in progress: `Cargo.toml` (workspace + `ti` feature), `Cargo.lock`, new `crates/ti-contracts/`, new `plan/spec/14-semantics.md`, decisions D8–D15 in `plan/spec/11-risks-decisions.md`. Cut from `baacfbd`, so it is 1 commit behind `plan/lume-ti` |
| Romantic Pike | `90fc608c` | Search library extraction ([design/search-api.md](design/search-api.md)) | `ti/search-api` | `.lanes/search` | 1 commit ahead (`46153fa` tests: golden search output capture + baselines). Clean tree. Cut from `baacfbd`, so it is 1 commit behind `plan/lume-ti` |
| Zygomorphic Prawn | `eccaf836` | W0 golden corpus part 1. Dependency survey done | `ti/w0-corpus` | `.lanes/corpus` | 0 commits ahead, clean tree. Survey results were relayed by mail rather than committed (they show up as D8–D15 in the W0 clone) |
| Regular Pheasant | `364a3fc7` | Host build pane (PowerShell 7, Windows Rust toolchain). Not an agent | — | — | Agents may split it once each |

How to refresh the last column: `git -C .lanes/<x> log --oneline plan/lume-ti..HEAD` and `git -C .lanes/<x> status --short`.

## Milestones

Week numbers count from kickoff. A milestone closes only when every gate test passes in CI ([spec/12](spec/12-milestones.md)).

| Milestone | Lanes | Weeks | Gate status |
|---|---|---|---|
| M0 Contracts | W0 | 1 | **In progress.** Workspace + `ti-contracts` uncommitted in `.lanes/w0`. Corpus not started. 0/4 gate items done |
| M1 Core and store | W1, W2 | 2–4 | Not started |
| M2 Ingest | W3 | 2–5 | Not started |
| M3 SQL and pushdown | W4 | 2–6 | Not started |
| M4 Text, geo, intervals | W4, W5, W6 | 6–8 | Not started |
| M5 Agent surface | W7 | 7–9 | Not started |
| M6 Fleet and benchmarks | W8, integrator | 9–12 | Not started |

Pre-work outside the milestones: search library extraction is in progress (golden baselines captured). `lume sql` over plain indexes waits until after M3.

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

Also undecided: the repo-fit items in [repo-fit.md](repo-fit.md), such as the W7a/W7b split (§6) and the CI strictness scope (§7). These are lead decisions unless escalated (unconfirmed).
