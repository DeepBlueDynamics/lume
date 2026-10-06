# 12. Milestones and acceptance

Spec pp. 24–26.

12 weeks from kickoff. W0 freezes contracts in week 1; core, ingest and SQL build
in parallel from week 2 against fixtures. Week counts are a proposal sized for one
agent per lane plus an integrator. A milestone closes only when every gate test
passes in CI on `main`.

```
            Wk1 Wk2 Wk3 Wk4 Wk5 Wk6 Wk7 Wk8 Wk9 Wk10 Wk11 Wk12
M0 Contracts [=]◆
M1 Core+store    [=========]◆
M2 Ingest        [=============]◆
M3 SQL+push      [=================]◆
M4 Text/geo/int                  [=========]◆
M5 Agent surface                     [=========]◆
M6 Fleet+bench                               [=============]◆
```

## M0 Contracts (W0) — week 1

- [ ] `ti-contracts` merged with the types and traits from [10-contracts](10-contracts.md), plus doc comments
- [ ] `ti.toml` schema with defaults (W = 10 s, profiles, allow/deny lists, unit-to-scale table)
- [ ] `ti-bench gen` produces the 5-vessel × 90-day correctness set deterministically from a seed
- [ ] ≥ 60 golden queries covering Q1–Q8, each with a DuckDB oracle twin and stored expected output

## M1 Core and store (W1, W2) — weeks 2–4

- [ ] BSI compare, sum, min, max agree with a naive model on 10,000 proptest cases, incl. negatives and depth growth
- [ ] `Predicate::eval` agrees with the naive model for random AND/OR/NOT trees of depth ≤ 4
- [ ] Crash test: 1,000 kill -9 runs during flush, zero lost or duplicated records after replay
- [ ] Seal produces byte-identical files and hashes from identical input

## M2 Ingest (W3) — weeks 2–5

- [ ] Replaying a recorded 24 h delta log yields `BucketRecord`s equal to oracle bucketing
- [ ] Parquet backfill of the correctness set is idempotent: running twice gives identical manifest hashes
- [ ] Pi 5 sustains 20,000 values/s for 1 h within the CPU and RSS budget

## M3 SQL and pushdown (W4) — weeks 2–6

- [ ] All non-text, non-geo golden queries match the oracle, first on the in-memory fixture, then on a real store
- [ ] `EXPLAIN` shows Exact pushdown for every expression the pushdown table marks Exact
- [ ] `raw` table queries the same Parquet and matches DuckDB exactly

## M4 Text, geo, intervals (W4, W5, W6) — weeks 6–8

- [ ] Full golden corpus green, incl. `match()`, `in_bbox`, `within_nm` and `intervals()`
- [ ] `BitmapAggregateExec` ≥ 10× faster than the materializing path on Q4 at shore scale
- [ ] `croaring` frozen-view evaluation written up in the decisions log, adopt or reject

## M5 Agent surface (W7) — weeks 7–9

- [ ] MCP tools live in `lume serve`, and `ti_resolve` returns the right column in the top 3 for ≥ 90 % of a 100-phrase test set
- [ ] A nemesis8 agent with only Lume MCP answers 20 scripted fleet questions; integrator grades against oracle results
- [ ] Signal K plugin installs on a HALPI2 from the HaLOS Marine container store, and on OpenPlotter from the Signal K App Store (Pi 4 4 GB and Pi 5), obtains a token and supervises ingest. psql and Grafana connect over Postgres wire and pass a 20-query smoke set

## M6 Fleet and benchmarks (W8, integrator) — weeks 9–12

- [ ] Two-node sync converges with 20 % chunk loss and a 30-minute link outage
- [ ] Shore node answers fleet queries across 50 synthetic vessels
- [ ] Benchmark report against every target. Go / no-go recorded in the decisions log
