# Lume TI performance comparisons

Measured 2026-10-06/07 on the hardware below. Each table says how it was run. Numbers are from single runs on
one machine of each kind unless noted; treat them as indicative, not as a controlled benchmark suite.

**Hardware**

- **Pi:** Raspberry Pi 5, 8 GB, SD card, no HAT, no active cooler (runs 72–86 °C). HaLOS Marine image, Signal K 2.31
  in Docker, InfluxDB 2.9.1, Grafana 13.2. Lume is the `signalk-lume-ti` plugin's native aarch64 build (thin LTO).
- **Host:** Windows 11 laptop, Rust 1.96 release builds unless noted.

---

## 1. Lume vs InfluxDB on the Pi (same Signal K data)

Both stores were fed the same live Signal K replay (N2K sample data) on the same Pi. Influx was fed through HaLOS's
preconfigured `signalk-to-influxdb2` writer (1 s resolution); Lume through its plugin (10 s buckets). Window
2026-10-06 23:00–23:50Z, own vessel only, 20 runs per query, warm p50. Harness: `bench/influx_vs_lume.py`.

| Query | InfluxDB p50 | Lume p50 | Speed-up | Answers agree? |
|---|---:|---:|---:|---|
| Hourly maximum depth | 21.4 ms | 6.1 ms | 3.5× | **Exact match** |
| 1-minute mean SOG | 15.2 ms | 9.3 ms | 1.6× | Within 0.44% (mean of bucket means vs raw mean) |
| Minutes where SOG > 3.2 and depth < 35 m | 53.2 ms | 9.5 ms | 5.6× | Same 40 minutes; per-minute minimums differ slightly |
| Minimum depth and the position at that time | 156.8 ms | 5.6 ms | 28× | Same depth; Influx finds its first occurrence 10 min later |
| Point counts per path | 4,080 ms | 57 ms | 72× | 3 constant battery paths differed by one bucket (bug, since fixed) |
| Raw depth range (all rows) | 42.9 ms | 43.7 ms | ≈ same | Bucket means differ up to ~3.5% (see caveat) |

Caveats:

- HaLOS's Influx writer keeps at most one sample per second, so Influx stores fewer samples than Lume sees.
  The mean and minimum differences come from that, not from either store being wrong.
- The one-bucket gap was a real Lume bug: a swallowed write error dropped one 10 s bucket. It is fixed in
  `3896e5c`, and re-verification on the Pi is pending.
- A first 17-minute run (10 runs) gave the same picture: 1.9–14× faster, and 28× on per-path counts.

## 2. Ingest and storage (host)

Golden boat corpus: 5 vessels, 11,500 Parquet files. Command: `lume ti backfill`, release build.

| Metric | Value |
|---|---:|
| Raw rows ingested | 95,924,426 |
| Backfill time | 744.5 s |
| Throughput | **128,847 rows/s** |
| Shards sealed | 65 in 16.9 s |
| Raw Parquet on disk | 1,892.7 MB |
| Lume index on disk | 730.8 MB (**0.39× raw**) |

Correctness on the same store: the golden corpus passes **61 / 0 failed / 1 excluded** against DuckDB as the
reference.

## 3. Offline document library (cruiser library)

The 7 default PDFs from `docs/cruiser_library.csv` (8.9 MB, about 352–356 sections).

| Step | Laptop (Python `uv` PDF extractor) | Pi 5 (pure-Rust lopdf extractor, D43) |
|---|---:|---:|
| Fetch (`lume crawl --list`) | 8.4 s | 18.5 s (Wi-Fi) |
| Extract text and build the index | 6.46 s | **0.92 s** |
| Index build alone | 51 ms | — |
| Search, e.g. "battery voltage low charging alternator" | 83–121 ms | **69 ms** |
| Index size | — | 4.9 MB for 8.9 MB of PDFs |

Search times include starting the `lume` process and loading the index; the BM25 pruning itself takes microseconds.
On the Pi, 4 image-only pages in one PDF were skipped and reported (no OCR).

**PDF extractor choice (D43):** on a generated 900-page PDF, lopdf took 100.2 ms against 126.6 ms for
pdf-extract (1.26×), with identical text.

## 4. Fleet sync (M6)

Boats sync sealed shards to a shore store over HTTP, then fleet queries are checked against every boat.

| Run | Vessels | Time | Per vessel |
|---|---:|---:|---:|
| Release build (gate) | 50 | **67.3 s** | 1.35 s |
| Release build | 5 | 5.5 s | 1.1 s |
| Debug build | 5 | 101–107 s | ~21 s |
| Debug build | 10 | 243 s | 24 s |

The lossy-link test (20% drop plus a 30-minute outage) resumes and ends with identical manifests.

## 5. Binary and package size

| Artifact | Size |
|---|---:|
| Host release `lume.exe` (`--features ti`) before PDF/EPUB support | 112,273,408 B |
| Same, after D43 PDF/EPUB and `count_paths` | 114,342,400 B (+1.84%) |
| Linux x64, thin LTO, 1 codegen unit, stripped | 100.85 MB (34.78 MB gzip) |
| Linux arm64 (Pi native, 16 codegen units), stripped | 133.9 MB |
| npm plugin package (both binaries) | 82.8 MB gzip, 235 MB unpacked |

Size reduction is still being measured (D45).

## 6. Agent answering fleet questions with only Lume MCP (M5 item 2)

The harness is `bench/agent_mcp_run.py` and the grader is `bench/agent_mcp_grade.py`. 20 natural-language questions
(Q1–Q8 classes) over the golden boat store. The model gets only the five read-only `ti_*` MCP tools, at most 12 turns
and 20 tool calls per question, and never sees SQL templates or answers. Answers are graded against hidden DuckDB
results: exact, within tolerance, or as a row set matched by value.

| Model | Lume MCP tools before `899898b` | After (schema guidance, teaching errors) |
|---|---:|---:|
| glm-5.3 (cloud, through Ollama) | 15 / 20 | **17 / 20** |
| qwen2.5:7b (local Ollama) | 0 / 20 | 5 / 20 |

The three glm-5.3 misses after the fix:

- a Q2 multi-condition window query (54 rows instead of 3; it did not aggregate to 10-minute windows);
- a Q5 that hit the 20 tool-call cap;
- a Q8 daily mean that ended on a schema lookup instead of the answer query.

The first run surfaced and fixed three real Lume problems: `store:""` was treated as a path, `ti_schema` returned
empty column lists for unmatched prefixes, and the tool descriptions had no data-model guidance.

Deviation from the milestone text: no nemesis8 launcher was reachable, so the harness itself is the agent runtime.
The tool restriction is the same.

---

## 7. Sealed-shard query cache (native host)

Lead-run Rust 1.96 release build at f17885d, store-full, seven warm iterations,
same binary with caching disabled/enabled. Class-level timings, milliseconds:

| Class | Warm p50 off | Warm p50 256 MiB | Cold off / on |
|---|---:|---:|---:|
| Q1 | 99.4 | 0.5 | 100 / 29 |
| Q2 | 210 | 1.5 | — |
| Q3 | 553 | 1.3 | — |
| Q4 | 1003 | 3.1 | — |
| Q5 | 4275 | 296 | 4301 / 1567 |
| Q7 | 159 | 3.3 | — |
| Q8 | 555 | 13.5 | — |

The 64 MiB run is nearly identical: Q5 warm p50 288 ms and Q8 14.0 ms.
All measured classes meet their p95 targets except Q5: 308 ms against 150 ms.
Cold clears only the decoded application cache; the OS cache is uncontrolled.

The saved native-256 report contains 20 per-query records; comparison.json
records matching row counts and answer fingerprints for all 20. Its comparison.md
is populated. The helper also accepts older class-only reports, explicitly marks
their values as unchecked, and rejects empty reports rather than printing an empty
table. Artifacts are under .lanes/data/query-cache/native-{256,64}, outside git.

Q5's slow warm case is q5-002 (electric-only motoring intervals using motor power
and IS DISTINCT FROM on diesel state): 299.98 ms, versus 6.76 ms for q5-001.
The 256 MiB run records no evictions for either query, so retained-cache capacity
does not explain this difference. Profiling q5-002 is queued after resolve-eval;
no cause or fix has been established. The lead verified the cache-enabled release
build against the count_paths store: 61 passed / 0 failed / 1 excluded, with all
20 A/B fingerprints matching.

## 8. Resolve accuracy through live MCP (M5 item 1)

100 new natural sailor/agent phrases over store-full, with 30 holdout phrases
committed before reading the resolver or tuning. Rust 1.99 debug, real loopback
lume serve MCP endpoint; zero transport errors.

| Split | Baseline top 1 / top 3 | Improved top 1 / top 3 |
|---|---:|---:|
| All 100 | 47 / 65 | **99 / 100** |
| Development 70 | 34 / 47 | 69 / 70 |
| Frozen holdout 30 | 13 / 18 | **30 / 30** |

General nautical vocabulary, units, unique single-edit typo corrections and
distinct-path candidate ranking improve the resolver. The fixture was unchanged
during tuning; holdout miss details were withheld until the final run.
See plan/design/resolve-eval.md for reproduction and limitations.

A separately authored lead blind set initially scored 16/20 top-one and 19/20
top-three (lead-reported). After the general idiom/depth/provenance follow-up,
the evaluator's separate exact-path split scores 18/20 and 19/20, with no errors.
The one miss returns latitude/longitude leaves for an expected position parent;
its expectation and ranking were left unchanged after viewing results. The
original 100-phrase set remains 99/100 top-one and 100/100 top-three.

## Not yet measured

- Lume vs InfluxDB over a full hour or longer, and cold-cache (after restart) latency.
- Grafana panel latency over Postgres. The 100k-row streaming change is merged; the Pi smoke rerun is pending.
- Pi 1-hour ingest throughput and memory (M2 item 3).
- The full 471-item library (fetch and index time).
