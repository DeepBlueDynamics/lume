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

**Host p95**, Rust 1.96.1 release (fat LTO, CGU=1), 7 warm iterations plus one cold query, `store-full` (5 vessels × 90 days), sealed-shard cache off / 256 MiB. Rerun on 2026-10-08 at `0eff8df`, adding all six `q6-*` queries to the previous 20-query selection. Committed artifacts: [JSON](../../bench/results/2026-10-08-0eff8df.json) and [Markdown](../../bench/results/2026-10-08-0eff8df.md). The previous run is retained under `.lanes/data/query-cache/native-256-distinct-2/` at `cebf5ea`. Each class reports the maximum per-query metric, not a pooled percentile.

| Class | Target | Warm, cache on | Cache off | Cold, cache on | Status |
|---|---:|---:|---:|---:|---|
| Q1 point lookup | ≤ 20 ms | 2.35 ms | 174.76 ms | 29.32 ms | **PASS** warm; **MISS** cold and cache off |
| Q2 selective multi-predicate | ≤ 150 ms | 4.61 ms | 1222.88 ms | 126.59 ms | **PASS** warm and cold; **MISS** cache off |
| Q3 count with filters | ≤ 50 ms | 1.99 ms | 972.15 ms | 122.62 ms | **PASS** warm; **MISS** cold and cache off |
| Q4 windowed aggregate | ≤ 400 ms | 6.25 ms | 4559.72 ms | 323.35 ms | **PASS** warm and cold; **MISS** cache off |
| Q5 intervals | ≤ 150 ms | 15.35 ms | 5079.14 ms | 1993.69 ms | **PASS** warm; **MISS** cold and cache off |
| Q6 text + telemetry | ≤ 200 ms | 78.36 ms | 1345.87 ms | 485.29 ms | **PASS** warm; **MISS** cold and cache off |
| Q7 geo + telemetry | ≤ 300 ms | 82.34 ms | 958.09 ms | 194.27 ms | **PASS** warm and cold; **MISS** cache off |
| Q8 broad scan | DuckDB parity ±50 % | 26.04 ms | 910.67 ms | 154.99 ms | **PENDING**: no DuckDB timing (`duckdb_baseline: null`) |

Q6's slowest query is `q6-004` (alerts joined to telemetry, 231 rows): warm cache-on p50 **73.62 ms**, p95/p99 **78.36 ms**. The other five Q6 queries have warm cache-on p95 **1.90–2.60 ms**. All 26 queries have identical row counts and answer fingerprints across cache modes and cold/warm runs. This is a same-binary consistency check, not a new DuckDB correctness run.

Caveats:
- **Hardware:** native Windows x64 host, not the edge HALPI2 or Pi 5. No compiler processes or release-profile environment overrides were active at measurement start.
- **Data:** 5 vessels × 90 days, not 1 vessel × 1 year; 1,610 documents (920 notes, 460 logbook entries, 230 alerts).
- **Cold timings:** "cold" clears only Lume's sealed bitmap cache; the OS page cache is not controlled, and document index/query caches keep their normal lifecycle.
- **Sampling:** seven warm samples per query; p95 and p99 both equal the maximum sample. The committed artifacts include one cold measurement and warm p50/p95/p99 per query with both cache settings.
- **What passes:** Q1–Q7 meet their warm cache-on targets on this host. Cold cache-on and warm cache-off miss Q1, Q3, Q5 and Q6; warm cache-off also misses Q2, Q4 and Q7.

### Cold after optional startup warm-up (2026-10-08)

Native Rust 1.96.1 release at `0fb40e8`, same `store-full` and 26-query
selection, cache cleared then the default newest-first/all-field preload awaited
before each first query. Seven subsequent warm iterations are retained in the
[JSON](../../bench/results/2026-10-08-0fb40e8.json) and
[Markdown](../../bench/results/2026-10-08-0fb40e8.md).
These are maximum per-query first-query observations, not cold p95 distributions;
OS cache is uncontrolled. Server startup never awaits the worker, so requests
arriving before completion can remain cold.

| Class | Edge target ms | After warm-up 256 MiB ms | Status | After warm-up 64 MiB ms | Status |
|---|---:|---:|---|---:|---|
| Q1 | 20 | 35.45 | **MISS** | 31.65 | **MISS** |
| Q2 | 150 | 136.40 | **PASS** | 121.71 | **PASS** |
| Q3 | 50 | 125.18 | **MISS** | 174.18 | **MISS** |
| Q4 | 400 | 350.88 | **PASS** | 319.79 | **PASS** |
| Q5 | 150 | 1921.38 | **MISS** | 1561.78 | **MISS** |
| Q6 | 200 | 336.72 | **MISS** | 305.73 | **MISS** |
| Q7 | 300 | 202.51 | **PASS** | 189.82 | **PASS** |
| Q8 | DuckDB parity | 211.58 | **PENDING** | 115.95 | **PENDING** |

Fresh-process warm-only measurements (engine + cache, not the complete server or
Pi RSS): 256 MiB admits **255.93 MiB**, 814 fields across 7 shards (including
possibly partial last shard), in **417.40 ms**, peak Windows working set **92.50 MiB**;
64 MiB admits **63.69 MiB**, 197 fields across 2 shards, in **165.46 ms**,
peak working set **33.03 MiB**. Both stop at the byte budget with no warm-up
evictions. The peak includes engine startup; warm-phase RSS is also sampled every
50 ms. Per-query preload timings and conservative cache charges are in the JSON.

All 26 row counts/fingerprints match the prior cache A/B baseline at both budgets,
including every subsequent warm repetition. No new DuckDB correctness run is
claimed. No benchmark-specific fields or dates were selected. Newest-first
warming improves Q6's observed cold time (485.29 → 336.72 / 305.73 ms) but
**does not close the historical-query cold gap**: Q1, Q3, Q5 and Q6 remain MISS.
For example, Q1 asks for May 1 and Q3 for May, whereas the preload visits the
newest sealed shards. The manifest's newest shard (308) starts May 25; the
next (307) starts May 17. The 256 MiB worker admits seven vessel/shard entries
from that prefix and the 64 MiB worker two, so neither reaches May 1. Q5 spans
history, and Q6's alert join covers older buckets.
The larger budget is not automatically faster; these are single observations.

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

Run the A6 measurement harness on the Pi while the user pans and zooms OpenCPN on
its screen. The first 120 seconds sample Signal K without queries; the following
600 seconds run the committed Q4/Q8 query selection back-to-back (concurrency 1).
Keep the same vessel, sources and ingest settings throughout both phases:

```sh
ti-query-bench contention --ti-url http://127.0.0.1:5863 --signalk 'ws://127.0.0.1:3000/signalk/v1/stream?subscribe=self' --baseline-secs 120 --duration 600 --queries Q4,Q8 --window last:7d --out bench/results/2026-10-08-contention-SHA.json
```

Replace SHA with the binary's commit; optionally add `--token-file P` for the same
bearer on HTTP and WebSocket, or `--concurrency N` for a separate stress run.
Before baseline sampling, the harness selects the vessel with the most telemetry
rows (override with `--vessel URN`) and resolves `last:7d` from that vessel's
newest bucket, including its full bucket. Use `--window START/END` for explicit
UTC bounds. It rewrites the committed vessel/time predicates without changing
aggregates, preflights each query, and exits 2 on any empty result unless
`--allow-empty` is set. Probes and preflight happen before baseline sampling.
The JSON records resolved vessel/window, preflight row counts, original and
rewritten SQL, and each query's p50/p95/p99 and errors, plus per
context/source/path receipt gaps, signed timestamp latency and baseline/load
changes. "drops" means gaps strictly over twice the phase's median period,
not independently confirmed lost deltas. Timestamp latency depends on exporter
and receiver clock synchronization; negative values are reported, not clamped.
Missing observations/null percentiles do not establish a pass. Record the user's
OpenCPN stutter observation separately. This tool does not change the PENDING
status until the real Pi run and visual observation are available. It uses plain
HTTP/WS for loopback measurement and rejects embedded URL credentials.

## 8. Reporting and CI

| Spec requirement | Status |
|---|---|
| p50/p95/p99 per class, cold and warm | warm **PASS** on the host (§2); one cold measurement per query, so cold percentile distributions remain **PENDING** |
| `bench/results/<date>-<sha>.json` plus a markdown summary | **PASS**: `bench/results/2026-10-08-0eff8df.json` and `.md`, complete Q1–Q8 cache off/on results |
| CI fails on a > 15 % p95 regression | **PENDING**: not implemented |

## 9. Go / no-go (D48, approved by the user 2026-10-08)

**Decision (D48): GO for the single-boat pilot; NO-GO, for now, on shore-scale and contention claims.**

**Why GO for the pilot (one vessel, Signal K on a Pi 5 or HALPI2, the plugin as History API provider):**
- Correctness gates pass, including crash safety and byte-identical reseals.
- Live ingest passes the Pi budget with headroom.
- Warm-server latencies beat every measured edge target by 2.5–20×, and InfluxDB on the same Pi by up to 72×.
- Fleet sync tolerates loss and outages.

**Why NO-GO beyond the pilot until these are measured:**
1. **Shore-scale latencies:** generate the 50-vessel × 365-day fleet and add the DuckDB timing that the "≥ 5×" rule needs.
2. **The contention test**, with OpenCPN on a HALPI2 or Pi 5.
3. **The cold cache — MISS after default warming:** optional, default-on, budget-bounded startup preload is implemented at `0fb40e8`, with first-query cache-hit/no-full-load and budget-stop tests. At 256 / 64 MiB, host cold-after-warm Q1 is 35.45 / 31.65 ms, Q3 125.18 / 174.18 ms, Q5 1921.38 / 1561.78 ms, Q6 336.72 / 305.73 ms: all still MISS (§2). Newest-first warming does not cover arbitrary history. Gap 3 remains open for these queries; accept the measured historical misses or evaluate an explicit workload policy separately. Process/OS cold-start and Pi edge p95 remain unmeasured.
4. **Edge p95 per class** on the Pi, ideally against a 1-vessel × 1-year store.
5. **The CI p95-regression gate.** Committed Q1–Q8 results are now available (§8).

Recorded as D48 in `plan/spec/11-risks-decisions.md` (`a33035e`, 2026-10-08). The five follow-ups above remain measurement conditions; startup warming addresses item 3 and must report any misses it leaves.

## 10. OTLP receiver capacity

Measured 2026-10-08 on this container against a debug `lume` binary (`target/debug/lume`, rustc 1.96.1) started as `lume ti otlp --bind 127.0.0.1 --port 0`. Command: `ti-bench otlp-soak --spawn --agents N --rate 1 --duration 300 --seed 42`. The three artifacts record HEAD `36e57fc8bc3dd711def96fe920fb4b05efe0dc65`. Each simulated agent posts `/v1/metrics` every 10 s and `/v1/logs` once per second, with the first post of agent `i` delayed by `i * interval / N`. A thread starts a post only while elapsed wall time is under 300 s. `load_wall_s` is the time until those in-flight posts finish. `offered_*` is the full schedule; `metrics_posts` and `log_posts` are attempts inside the window. These rows are this debug build at rate 1 for 300 s.

| Agents | Offered metrics / logs | Attempts metrics / logs | HTTP 200 metrics / logs | HTTP 503 metrics / logs | Tokens stored = sent | Docs = 2xx logs | load_wall_s | RSS start / end / peak (bytes) | Artifact |
|---:|---|---|---|---|---|---|---:|---|---|
| 10 | 300 / 3000 | 68 / 678 | 68 / 678 | 0 / 0 | 584 = 584 | 678 = 678 | 304.171 | 98336768 / 155844608 / 156295168 | [agents10](../../bench/results/2026-10-08-otlp-soak-36e57fc-agents10.json) |
| 50 | 1500 / 15000 | 78 / 764 | 78 / 764 | 0 / 0 | 659 = 659 | 764 = 764 | 319.758 | 96321536 / 352731136 / 368250880 | [agents50](../../bench/results/2026-10-08-otlp-soak-36e57fc-agents50.json) |
| 200 | 6000 / 60000 | 4437 / 44274 | 89 / 747 | 4348 / 43527 | 790 = 790 | 747 = 747 | 347.162 | 97959936 / 469258240 / 472272896 | [agents200](../../bench/results/2026-10-08-otlp-soak-36e57fc-agents200.json) |

POST latency, nearest rank over every attempt, milliseconds:

| Agents | `/v1/logs` p50 / p95 / p99 (n) | `/v1/metrics` p50 / p95 / p99 (n) |
|---:|---|---|
| 10 | 4032.505 / 4493.178 / 4727.223 (678) | 4051.03 / 4527.455 / 4879.871 (68) |
| 50 | 18721.98 / 20889.91 / 21070.918 (764) | 18548.505 / 20773.838 / 20835.533 (78) |
| 200 | 0.145 / 0.309 / 24470.47 (44274) | 0.156 / 0.365 / 24456.366 (4437) |

HTTP 400, 413, `other`, and transport `error` are 0 on both endpoints in all three runs. At 10 and 50 agents every attempt is HTTP 200. At 200 agents the remaining attempts are HTTP 503 (4348 metric, 43527 log). The accept path writes that 503 when active connections are already at `MAX_CONCURRENT_CONNECTIONS` (64, `src/agent.rs`). Those rejects return in well under a millisecond, so the 200-agent p50 and p95 are 503 latency and the p99 includes the accepted posts. `sql.ok` is true in each artifact: the sum of per-vessel `max("claude_code.token.usage")` equals the token deltas in HTTP 200 metric posts, and `count(*)` from `docs` equals the HTTP 200 log posts.

### Release, group commit (`15b9cab`)

Same schedule, rate 1, seed 42, duration 300 s, measured 2026-10-08 against release `lume` and `ti-bench` (rustc 1.96.1, `lto` on). HEAD at measurement was `15b9cab159509f2ca42689960fc5322e34ad7c90`. The receiver is still `lume ti otlp`. An idle POST flushes immediately. Batches that arrive during that flush stage, and the next leader takes at most 32. Each POST waits for its group's fsync. A standalone receiver has no query path, so the dirty-flag reload does not run during these soaks. `sql.ok` is `lume ti query <sql> --store <dir> --json` after the spawned receiver was stopped. `docs_json` is the size of `docs/documents.json` at the end of the run and the `upsert_all` time of each log-bearing group (full-file read, atomic rewrite, and fsync). Recent soak documents are inside the 90-day retention window, so retention deletes do not add rewrites.

| Agents | Offered metrics / logs | Attempts metrics / logs | HTTP 200 metrics / logs | HTTP 503 metrics / logs | Tokens stored = sent | Docs = 2xx logs | load_wall_s | RSS start / end / peak (bytes) | Artifact |
|---:|---|---|---|---|---|---|---:|---|---|
| 10 | 300 / 3000 | 300 / 3000 | 300 / 3000 | 0 / 0 | 2632 = 2632 | 3000 = 3000 | 299.987 | 36769792 / 48230400 / 52281344 | [agents10](../../bench/results/2026-10-08-otlp-soak-15b9cab-agents10.json) |
| 50 | 1500 / 15000 | 1500 / 14999 | 1500 / 14999 | 0 / 0 | 12871 = 12871 | 14999 = 14999 | 300.369 | 36462592 / 967000064 / 990384128 | [agents50](../../bench/results/2026-10-08-otlp-soak-15b9cab-agents50.json) |
| 200 | 6000 / 60000 | 5942 / 59540 | 1876 / 20764 | 4066 / 38776 | 15858 = 15858 | 20764 = 20764 | 300.569 | 36896768 / 1912934400 / 1975578624 | [agents200](../../bench/results/2026-10-08-otlp-soak-15b9cab-agents200.json) |

POST latency, nearest rank over every attempt, milliseconds:

| Agents | `/v1/logs` p50 / p95 / p99 (n) | `/v1/metrics` p50 / p95 / p99 (n) |
|---:|---|---|
| 10 | 7.774 / 33.355 / 55.482 (3000) | 46.58 / 57.663 / 68.185 (300) |
| 50 | 178.166 / 385.992 / 450.211 (14999) | 205.12 / 378.589 / 453.243 (1500) |
| 200 | 0.165 / 1330.731 / 1764.261 (59540) | 0.167 / 1328.47 / 1708.508 (5942) |

`documents.json` at the end of the run, and `upsert_all` rewrite time per log-bearing group:

| Agents | Bytes | Log-group commits | Rewrite p50 / p95 / max (µs) |
|---:|---:|---:|---|
| 10 | 1442955 | 3000 | 5384 / 7713 / 25990 |
| 50 | 7246028 | 2862 | 8689 / 19942 / 138064 |
| 200 | 10056248 | 784 | 15188 / 53668 / 261543 |

HTTP 400, 413, `other`, and transport `error` are 0 in all three release runs. Attempted/offered is 3300/3300, 16499/16500, and 65482/66000. At 10 agents every attempt is HTTP 200, both p95 values are under 250 ms (logs 33.355, metrics 57.663), and `sql.ok` is true. At 50 agents there are no 503s, the schedule is above 95% attempted, and `sql.ok` is true. Both p95 values miss 250 ms (logs 385.992, metrics 378.589). The one unattempted log is the window closing, not a failed POST. At 200 agents there is no numeric target. The 503s are the 64-connection cap. Their p50 is the fast reject; p95 is about 1.3 s.

At 10 agents each log is its own group (3000 commits). At 50 agents, 14999 logs share 2862 log-bearing groups (about 5.2 documents each). At 200 agents, 20764 acknowledged logs share 784 groups (about 26.5 documents each, under the cap of 32). One rewrite's p95 is 7.7 ms, 19.9 ms, and 53.7 ms at those three sizes. The 50-agent POST p95 is the queue of serialized group flushes, not a single rewrite. The file is still rewritten in full on every log group, and that cost grows with the store. `DocStore` was not changed.

### Release, A10 + A13 (`83badce`)

Same schedule, rate 1, seed 42, duration 300 s, measured 2026-10-08 against release `lume` and `ti-bench` (rustc 1.96.1, `lto` on). HEAD at measurement was `83badcee1e870210c47f08ed0c231d2e148be99f`, on `ti/otlp-a13-soak` from `origin/plan/lume-ti` at `067374c` (D52 append `DocStore`). Group commit is the `15b9cab` leader: an idle POST flushes immediately, arrivals during that flush stage, and the next leader takes at most 32. Each POST waits for its group's fsync. `sql.ok` is `lume ti query` after the spawned receiver stops. `docs_json.bytes` is the end length of `docs/documents.log`. Bytes written per log-bearing group are the append delta when the generation is unchanged, and the new log's length when compaction replaces the generation. `rewrite_us` is only `DocStore::upsert_all`. `compactions` counts stderr lines with `compact=1`. These three runs compacted zero times. Recent soak documents are inside the 90-day retention window, so retention deletes do not add writes.

| Agents | Offered metrics / logs | Attempts metrics / logs | HTTP 200 metrics / logs | HTTP 503 metrics / logs | Tokens stored = sent | Docs = 2xx logs | load_wall_s | RSS start / end / peak (bytes) | Artifact |
|---:|---|---|---|---|---|---|---:|---|---|
| 10 | 300 / 3000 | 300 / 3000 | 300 / 3000 | 0 / 0 | 2632 = 2632 | 3000 = 3000 | 300.003 | 35663872 / 44105728 / 45088768 | [agents10](../../bench/results/2026-10-08-otlp-soak-83badce-agents10.json) |
| 50 | 1500 / 15000 | 1500 / 14999 | 1500 / 14999 | 0 / 0 | 12871 = 12871 | 14999 = 14999 | 300.162 | 37285888 / 388984832 / 402468864 | [agents50](../../bench/results/2026-10-08-otlp-soak-83badce-agents50.json) |
| 200 | 6000 / 60000 | 5851 / 58488 | 1715 / 16522 | 4136 / 41966 | 14547 = 14547 | 16522 = 16522 | 301.367 | 36892672 / 564715520 / 734072832 | [agents200](../../bench/results/2026-10-08-otlp-soak-83badce-agents200.json) |

POST latency, nearest rank over every attempt, milliseconds:

| Agents | `/v1/logs` p50 / p95 / p99 (n) | `/v1/metrics` p50 / p95 / p99 (n) |
|---:|---|---|
| 10 | 5.848 / 13.978 / 57.054 (3000) | 49.919 / 69.402 / 91.228 (300) |
| 50 | 113.384 / 330.487 / 381.797 (14999) | 181.572 / 275.951 / 322.419 (1500) |
| 200 | 0.137 / 1289.963 / 1758.654 (58488) | 0.143 / 1299.212 / 1810.815 (5851) |

`documents.log` end length, bytes written per log-bearing group, and `upsert_all` time:

| Agents | End bytes | Log-group commits | Written p50 / p95 / max (bytes) | Compactions | Upsert p50 / p95 / max (µs) |
|---:|---:|---:|---|---:|---|
| 10 | 1382977 | 3000 | 452 / 491 / 493 | 0 | 1971 / 3356 / 11652 |
| 50 | 6750468 | 3493 | 493 / 4392 / 7277 | 0 | 1741 / 2494 / 17580 |
| 200 | 7391840 | 654 | 13101 / 14342 / 14617 | 0 | 1827 / 2616 / 23062 |

HTTP 400, 413, `other`, and transport `error` are 0 in all three runs. Attempted/offered is 3300/3300, 16499/16500, and 64339/66000. At 10 agents every attempt is HTTP 200, both p95 values are under 250 ms (logs 13.978, metrics 69.402), and `sql.ok` is true. At 50 agents there are no 503s, the schedule is above 95% attempted, and `sql.ok` is true. Both p95 values miss 250 ms (logs 330.487, metrics 275.951). The one unattempted log is the window closing, not a failed POST. At 200 agents there is no numeric target. The 503s are the 64-connection cap (4136 metric, 41966 log). Their p50 is the fast reject; p95 is about 1.3 s.

At 10 agents each log is its own group (3000 commits). At 50 agents, 14999 logs share 3493 log-bearing groups (about 4.3 documents each). At 200 agents, 16522 acknowledged logs share 654 groups (about 25.3 documents each, under the cap of 32). Written p95 is 491, 4392, and 14342 bytes. `upsert_all` p95 is 3.356 ms, 2.494 ms, and 2.616 ms. The 50-agent POST p95 is 330.487 ms beside that 2.494 ms upsert. Peak RSS is 45088768, 402468864, and 734072832 bytes. Beside `15b9cab`, 10-agent logs p95 is 13.978 ms (was 33.355) and metrics p95 is 69.402 ms (was 57.663). 50-agent logs p95 is 330.487 ms (was 385.992) and metrics p95 is 275.951 ms (was 378.589). Both 50-agent p95 values still miss 250 ms. `DocStore` was not changed for this measurement.

### Flush phases, 50 agents (`925d5e6`)

The 50-agent p95 still misses, so the same release schedule was run once more at `925d5e607084467c5a77499a82dadb8754ef0da8` with timers only. Flush order is unchanged. Artifact: [agents50 profile](../../bench/results/2026-10-08-otlp-flush-profile-925d5e6-agents50.json). Attempts were 1500/14999, every attempt was HTTP 200, 503s were 0, and `sql.ok` was true. Logs p50/p95/p99 were 100.012/311.767/369.856 ms (n=14999). Metrics were 174.351/233.293/289.216 ms (n=1500). Logs p95 still misses 250 ms. `documents.log` ended at 6756146 bytes, with 3827 log commits, 0 compactions, and written p95 of 4089 bytes. The 10/50/200 rows above stay the `83badce` measurement, where both 50-agent p95 values miss.

Phase times are nearest rank over the stderr lines, in microseconds. `open` is `DocStore::open`. `upsert` is `upsert_all`. `logs` is that open, the upsert, and retention deletes. `metrics` is `flush_all`, `store.flush`, and `seal_and_retain` timed together. `checkpoint` is `persist_counters`. Log-only groups leave `metrics_us` and `checkpoint_us` at 0 and are left out of those two percentiles.

| Phase | p50 (µs) | p95 (µs) |
|---|---:|---:|
| open | 7586 | 17228 |
| upsert | 1693 | 2446 |
| logs | 8240 | 21895 |
| metrics | 156188 | 177390 |
| checkpoint | 2885 | 4236 |

Open max is 34621 µs and upsert max is 6775 µs. The metrics phase p50/p95 is 156.188/177.390 ms. The log-commit phase p95 is 21.895 ms, and open p95 is 17.228 ms. The log POST p95 on this run is 311.767 ms, above the log-commit phase. The long section inside the group flush is the metrics shard flush. `flush_all`, `store.flush`, and `seal_and_retain` are not timed separately. `DocStore` was not changed.
