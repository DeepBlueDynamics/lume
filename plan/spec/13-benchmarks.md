# 13. Benchmarks and evaluation

Spec pp. 26–28. Owner: [W8](../lanes/W8-sync-bench.md); corpus and generator [W0](../lanes/W0-contracts.md).

Correctness is defined by a DuckDB oracle over the same Parquet. Speed is judged
against that same DuckDB baseline on two reference machines. Targets are
proposals to confirm at M0, not measurements.

## Reference machines

- **Edge:** Hat Labs HALPI2 (CM5, 8 GB, built-in NVMe) — PV-1's computer, 1 vessel. Same SoC class as a Pi 5. Floor: Pi 4, 4 GB, USB SSD, which must pass the golden corpus with p95 targets relaxed 3×.
- **Shore:** 8-core x86-64, 64 GB RAM, NVMe, 50 vessels.

## Datasets

1. **Synthetic fleet (`ti-bench gen`).** 50 vessels × 365 days × 120 paths (10 nav paths at 1 Hz, rest at 0.1 Hz), ~33 billion raw values, signalk-parquet layout. Full set for performance only; correctness uses a 5-vessel × 90-day subset. Models passages, anchoring and dock time; engine on/off states, bilge cycles correlated with heel, wind fronts, notifications, notes with planted keywords. Fixed seed, reproducible.
2. **Real boat.** ≥ 90 days of recorded deltas from PV-1 (or another DeepBlue test vessel if her install slips). Catches path sprawl, source conflicts and timestamp skew that synthetic data hides.

## Oracle

DuckDB SQL over the same Parquet. Each golden query has an oracle twin that
buckets with `time_bucket(INTERVAL 'W', ts, TIMESTAMP '2020-01-01')`, then applies
the same aggregate and fixed-point rounding. `lume ti verify` runs both and diffs:

- Exact on keys, set values and counts.
- ±0.5 × 10^−scale on BSI values.

## Query classes and targets

| Class | Example | Edge p95 (1 vessel, 1 yr) | Shore p95 (50 vessels, 1 yr) |
|---|---|---|---|
| Q1 point lookup | wind at a timestamp | ≤ 20 ms | ≤ 20 ms |
| Q2 selective multi-predicate (4+ conjuncts, < 1 % selectivity) | motivating query | ≤ 150 ms | ≤ 100 ms |
| Q3 count with filters | buckets with SOG > 6 kn and engine off | ≤ 50 ms | ≤ 60 ms |
| Q4 windowed aggregate | max wind per day, 1 year | ≤ 400 ms | ≤ 300 ms |
| Q5 intervals | motoring in > 25 kn, runs ≥ 5 min | ≤ 150 ms | ≤ 150 ms |
| Q6 text + telemetry | `match(notes,'leak')` and heel > 20° | ≤ 200 ms | ≤ 150 ms |
| Q7 geo + telemetry | within 5 nm of a point, depth < 3 m | ≤ 300 ms | ≤ 250 ms |
| Q8 broad scan (selectivity > 30 %) | mean battery voltage per hour | DuckDB parity ±50 % | DuckDB parity ±50 % |

Q2, Q3, Q5 and Q6 must also beat DuckDB by ≥ 5× on shore. If not, the decisions
log records why and the fallback in [11-risks-decisions](11-risks-decisions.md) triggers.

## Single-box contention test (gates M6)

On the HALPI2, run OpenCPN with continuous chart pan and zoom while Signal K
ingests at full rate. Run Q4 and Q8 back to back for 10 minutes. Pass if:

- Signal K delta latency (sensor timestamp → WebSocket client) rises < 10 % vs the same run without Lume TI.
- No delta is dropped.
- OpenCPN shows no visible stutter, measured as 95th-percentile frame time.

## Reported per run

- p50, p95, p99 per class, plus cold and warm cache timings.
- Index bytes per vessel-year vs Parquet bytes.
- Ingest values/s, CPU % and RSS on the Pi.
- Sync bytes per day per vessel.
- Results to `bench/results/<date>-<git sha>.json` with a markdown summary; CI fails on a > 15 % p95 regression.
