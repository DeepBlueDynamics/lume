# Lume TI benchmark report against spec/13 (M6 item 3)

Written by the lead on 2026-10-07 at `5fe23f1`. Every number cites its source; nothing is
extrapolated. Statuses:
- **PASS:** a measured value meets the target.
- **MISS:** a measured value does not meet it.
- **PENDING:** never measured as specified. The entry names what is missing.

**Data used:**
- **Correctness store:** the deterministic 5-vessel × 90-day fleet (`store-full`, 95.9 M values).
- **Pi:** live Signal K on a Raspberry Pi 5 (8 GB, SD card).
- **Not built:** the spec's 50-vessel × 365-day performance fleet and the 1-vessel × 1-year edge set.

## 1. Correctness

| Check | Result | Source |
|---|---|---|
| Golden corpus against the DuckDB oracle | **PASS**: 61 passed, 0 failed, 1 excluded | `lume ti verify --corpus tests/golden`, last at `bc92c4f` |
| 24 h stream replay against an independent oracle | **PASS**: 153,374 / 153,374 bucket records | `crates/ti-ingest/tests/m2_gate.rs` |
| Re-ingesting 95.9 M values | **PASS**: all 65 sealed shards (8,060 files) byte-identical | `m2_gate.rs`; re-backfill at `7ea727d` |
| 1,000 seeded `kill -9` runs during ingest | **PASS**: 0 lost, 0 duplicated | `crates/ti-store/tests/crash_recovery.rs` |

## 2. Query classes, edge targets (1 vessel, 1 year)

**Host p95**, Rust 1.96 release, 7 iterations, `store-full` (5 vessels × 90 days), sealed-shard cache 256 MiB. Artifact: `.lanes/data/query-cache/native-256-distinct-2/` at `cebf5ea`. Each class lists its slowest query.

| Class | Target | Warm, cache on | Cache off | Cold, cache on | Status |
|---|---:|---:|---:|---:|---|
| Q1 point lookup | ≤ 20 ms | 1.0 ms | 130.6 ms | 30.6 ms | **PASS** warm; **MISS** cold and cache off |
| Q2 selective multi-predicate | ≤ 150 ms | 2.4 ms | 910 ms | 129 ms | **PASS** warm and cold; **MISS** cache off |
| Q3 count with filters | ≤ 50 ms | 2.0 ms | 737 ms | 126 ms | **PASS** warm; **MISS** cold and cache off |
| Q4 windowed aggregate | ≤ 400 ms | 4.9 ms | 3,494 ms | 301 ms | **PASS** warm and cold; **MISS** cache off |
| Q5 intervals | ≤ 150 ms | 8.6 ms | 4,349 ms | 1,589 ms | **PASS** warm; **MISS** cold and cache off |
| Q6 text + telemetry | ≤ 200 ms | not in this run | — | — | **PENDING**: rerun with the q6 queries |
| Q7 geo + telemetry | ≤ 300 ms | 59.1 ms | 725 ms | 179 ms | **PASS** warm and cold; **MISS** cache off |
| Q8 broad scan | DuckDB parity ±50 % | 24.6 ms | 681 ms | 125 ms | **PENDING**: no DuckDB timing (`duckdb_baseline: null`) |

Caveats:
- **Hardware:** host x86, not the edge HALPI2 or Pi 5.
- **Data:** 5 vessels × 90 days, not 1 vessel × 1 year.
- **Cold timings:** "cold" clears only Lume's decoded cache; the OS page cache is not controlled.
- **What passes:** the targets hold for a warm, resident server, which is how the Signal K plugin runs. The first query after a restart and a cache-disabled build miss Q1, Q3 and Q5.

**On the Pi**, this is the only per-query evidence. Same live Signal K data in both stores, 20 runs each, warm p50 (`docs/performance-comparisons.md` §1):

| Query | InfluxDB | Lume |
|---|---:|---:|
| Hourly max depth | 21.4 ms | 6.1 ms |
| 1-minute mean SOG | 15.2 ms | 9.3 ms |
| Minutes with SOG > 3.2 and depth < 35 m | 53.2 ms | 9.5 ms |
| Minimum depth and its position | 156.8 ms | 5.6 ms |
| Per-path counts | 4,080 ms | 57 ms |
| Raw scan of all rows | 42.9 ms | 43.7 ms |

These are below every edge target, but they are not the spec's class definitions and not p95. **Edge p95 per class: PENDING.**

## 3. Query classes, shore targets (50 vessels, 1 year)

**PENDING.** No 50-vessel × 365-day fleet has been generated or queried.
- The "≥ 5× DuckDB" requirement for Q2, Q3, Q5 and Q6 has no DuckDB timing.
- What exists:
  - fleet sync of 50 vessels (§6);
  - a year-long "max wind per vessel per day" over 5 vessel-years: 40.3 ms, against 2.92 s for DataFusion materialising the same rows, with identical results (`crates/ti-sql/tests/m4.rs`).

## 4. Ingest, CPU and RSS on the Pi (spec/06, spec/03)

| Target | Measured | Status | Source |
|---|---|---|---|
| ≥ 20,000 values/s for 1 h, ≤ 25 % of one core, ≤ 400 MB RSS | 19,916 values/s mean for 60 min; 14.2 % mean / 24.2 % peak CPU; 65.6 MB peak RSS; 0 rejected | **PASS** | `docs/bench/pi5-load-20k-2026-10-07.md` (`78c913d`) |
| Headroom | 38,508 values/s at 25.1 % CPU and 75.3 MB | measured | same run, 40k step |

Caveats: the Pi had no fan and was soft-throttling (75–82 °C). A fan has since been fitted (62–66 °C), and the run has not been repeated with it.

## 5. Index size

| Target | Measured | Status | Source |
|---|---|---|---|
| Index bytes per vessel-year against Parquet | 730.8 MB for 1,892.7 MB of raw Parquet (0.39×), current backfill. An earlier count reported 554 MB (0.29×), about 450 MB per vessel-year | measured; spec/13 sets no numeric target | `docs/performance-comparisons.md` §2; README |

## 6. Fleet sync

| Item | Result | Status | Source |
|---|---|---|---|
| M6 item 1: converges with 20 % chunk loss and a 30-minute outage | Byte-identical manifests and shard hashes. A mid-shard outage resumes within the shore session TTL and restarts past it. Time-compressed at the 30:60 outage-to-TTL ratio | **PASS** | `crates/ti-sync/tests/two_node_sync_http.rs` (`3024cb9`) |
| M6 item 2: shore answers fleet queries across 50 synthetic vessels | 50 vessels synced and verified in 67.32 s (release) | **PASS** | `tests/fleet_sync_m6.rs` (`4400327`) |
| Sync bytes per day per vessel | not measured | **PENDING** | — |

## 7. Single-box contention test (gates M6)

**PENDING.** This test has not been run:
- Signal K latency +< 10 %;
- no dropped deltas;
- no OpenCPN stutter, with Q4 and Q8 back to back for 10 min.

What exists: the 1-hour load run kept the production Signal K, InfluxDB and Lume plugin running on the same Pi. It did not measure their latency, and no OpenCPN pan-and-zoom load was applied.

## 8. Reporting and CI

| Spec requirement | Status |
|---|---|
| p50/p95/p99 per class, cold and warm | **PASS** for the host store (§2) |
| `bench/results/<date>-<sha>.json` plus a markdown summary | partial: written under `.lanes/data/query-cache/...`, not committed to `bench/results/` |
| CI fails on a > 15 % p95 regression | **PENDING**: not implemented |

## 9. Go / no-go (proposed D48, for the user to approve)

**Proposed decision: GO for the single-boat pilot; NO-GO, for now, on shore-scale and contention claims.**

**Why GO for the pilot (one vessel, Signal K on a Pi 5 or HALPI2, the plugin as History API provider):**
- Correctness gates pass, including crash safety and byte-identical reseals.
- Live ingest passes the Pi budget with headroom.
- Warm-server latencies beat every measured edge target by 2.5–20×, and InfluxDB on the same Pi by up to 72×.
- Fleet sync tolerates loss and outages.

**Why NO-GO beyond the pilot until these are measured:**
1. **Shore-scale latencies:** generate the 50-vessel × 365-day fleet and add the DuckDB timing that the "≥ 5×" rule needs.
2. **The contention test**, with OpenCPN on a HALPI2 or Pi 5.
3. **The cold start:** the first query after a restart misses Q1, Q3 and Q5. Either warm the cache at serve start or accept it in writing.
4. **Edge p95 per class** on the Pi, ideally against a 1-vessel × 1-year store.
5. **The CI p95-regression gate and committed `bench/results/`.**

Recording D48 in `plan/spec/11-risks-decisions.md` waits for the user's approval.
