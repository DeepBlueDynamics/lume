# Lume Telemetry Index (Lume TI) — Plan

Source: `inbox/Lume Telemetry Index — Build Spec (2).pdf` (31 pp, @Tom, 2026-10-05).
This directory breaks that spec into working documents. The PDF stays the
reference; if a file here disagrees with it, the PDF wins until a decision
in [spec/11-risks-decisions.md](spec/11-risks-decisions.md) says otherwise.

## Layout

| Path | What lives there |
|---|---|
| `inbox/` | Raw incoming material (specs, notes, owner answers) not yet broken down |
| `spec/` | The build spec, one file per section |
| `lanes/` | One work package per agent lane (W0–W8): scope, deliverables, gate |
| `repo-fit.md` | Where the spec meets (or collides with) the current Lume codebase |
| `design/` | Design proposals that come out of reviews (e.g. [search-api.md](design/search-api.md)) |

## One-line summary

A bitmap-indexed, read-only SQL telemetry index inside Lume: Signal K deltas and
signalk-parquet history are bucketed (default 10 s) into roaring-bitmap shards
(vessel × 2^16 buckets), queried through Apache DataFusion with filter pushdown,
exposed over MCP, HTTP, CLI and Postgres wire, running on a boat's Pi and
federating sealed shards to shore.

## Spec sections

1. [Purpose, goals, non-goals, done](spec/01-goals.md)
2. [Pilot vessel PV-1](spec/02-pilot-vessel.md)
3. [Single-box budget](spec/03-single-box-budget.md)
4. [System architecture](spec/04-architecture.md)
5. [Data model](spec/05-data-model.md)
6. [Ingest pipeline](spec/06-ingest.md)
7. [Query layer](spec/07-query.md)
8. [Interfaces](spec/08-interfaces.md)
9. [Install and fleet topology](spec/09-install-fleet.md)
10. [Contracts and PR rules](spec/10-contracts.md)
11. [Risks, open questions, decisions](spec/11-risks-decisions.md)
12. [Milestones](spec/12-milestones.md)
13. [Benchmarks and evaluation](spec/13-benchmarks.md)

## Lanes

| Lane | Crate | Depends on | Gate milestone |
|---|---|---|---|
| [W0](lanes/W0-contracts.md) | `ti-contracts` | — | M0 (wk 1) |
| [W1](lanes/W1-core.md) | `ti-core` | W0 | M1 (wk 2–4) |
| [W2](lanes/W2-store.md) | `ti-store` | W0 | M1 (wk 2–4) |
| [W3](lanes/W3-ingest.md) | `ti-ingest` | W0 | M2 (wk 2–5) |
| [W4](lanes/W4-sql.md) | `ti-sql` | W0 (+W1 via trait) | M3 (wk 2–6), M4 |
| [W5](lanes/W5-text.md) | `ti-text` | W0, Lume search API | M4 (wk 6–8) |
| [W6](lanes/W6-geo.md) | `ti-geo` | W0 | M4 (wk 6–8) |
| [W7](lanes/W7-serve.md) | `ti-serve` | W0 | M5 (wk 7–9) |
| [W8](lanes/W8-sync-bench.md) | `ti-sync` + `ti-bench` | W0, W2 | M6 (wk 9–12) |

## Status

- [x] Spec received and filed in `inbox/`
- [x] Spec split into sections and lanes
- [ ] Owner assumptions confirmed (see [spec/02-pilot-vessel.md](spec/02-pilot-vessel.md))
- [ ] Open questions resolved (see [spec/11-risks-decisions.md](spec/11-risks-decisions.md))
- [ ] Repo-fit issues decided (see [repo-fit.md](repo-fit.md))
- [ ] M0 kickoff
