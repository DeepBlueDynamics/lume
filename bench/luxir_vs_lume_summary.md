# Lume and Luxir, compared

This is a comparison of two separate programs. Luxir is not part of Lume, and Lume does not call Luxir.

Audited numbers are from `.lanes/data/luxir-bench/scoreboard.md` (2026-10-09). The longer reports are still on other branches and will merge separately:

- `bench/luxir_vs_lume.md` and `bench/luxir/relevance_audit.md` on `bench/luxir-luxir`
- `bench/luxir/lume-measurements.md` on `bench/luxir`
- `bench/luxir/relevance-results.md` on `bench/relevance-eval` (matrix `214ee91`, NFCorpus `6afb2b7`)
- `bench/luxir/search-hot-path.md` on `perf/search-hot-path` (Step 1 labeled `7198931` in the scoreboard)

## Setup

Luxir 0.1.0 (prebuilt x86-64-v3) and Lume. Same conditions for both: Docker `--cpus 8 --memory 8g`, indexes on Linux volumes, one `python:3.12-slim` client on a shared Docker network. Warm servers: one warmup pass, then 3 timed passes, and the median. Throughput is 8 and 16 workers for 60 s each. The Lume build is thin LTO, codegen-units 1, the same as the release. Latency is client wall-clock.

BEIR SciFact: 5,183 documents, 300 queries. TREC-COVID: 171,332 documents, 50 queries. NFCorpus, held out: 3.6k documents, 323 queries.

## Quality

nDCG@10. "Before" is Lume with stemming off and the old coordination penalty. "After" is stemming plus coordination floor 1.0. That change is not in v0.12.3. It is the planned default for new indexes (PR #8; the scoreboard names v0.12.4). Existing indexes keep their recorded mode until they are reindexed.

| nDCG@10 | SciFact | TREC-COVID | NFCorpus (held out) |
|---|---:|---:|---:|
| Lume before | 0.645 | 0.561 | 0.296 |
| Lume after | 0.677 | 0.632 | 0.314 |
| Gain | +0.032 | +0.071 | +0.019 |
| Luxir BM25 | 0.679 | 0.605 | 0.321 |

On TREC-COVID the scoreboard says Lume after this change beats Luxir on every reported quality metric (nDCG@10 0.632 vs 0.605). On SciFact it is close (0.677 vs 0.679, MRR 0.644 vs 0.645). On held-out NFCorpus the pre-registered gate passed (every Lume metric improved, and the variant order matched SciFact and TREC-COVID), and Luxir BM25 is still ahead (0.321 vs 0.314).

Title fallback helps TREC-COVID and hurts SciFact, so it stays off. Luxir hybrid (GTR-T5 vectors, RRF) is a different mode: SciFact nDCG@10 0.694. It is not the BM25 comparison above.

Published BEIR BM25, for reference only: SciFact 0.665, TREC-COVID 0.656, NFCorpus 0.325.

## Speed

Released rows are BM25. p50 is the median query. QPS is 8 workers.

| | p50 | p99 | QPS (8 workers) |
|---|---:|---:|---:|
| Lume v0.12.2, SciFact | 286 ms | 326 ms | 17 |
| Lume v0.12.3, SciFact | 5.7 ms | 10.9 ms | 510 |
| Luxir BM25, SciFact | 0.36 ms | 0.49 ms | 4,493 |
| Lume v0.12.2, TREC-COVID | 7,892 ms | 8,873 ms | out of memory at 8 workers |
| Lume v0.12.3, TREC-COVID | 99 ms | 187 ms | 40 |
| Luxir BM25, TREC-COVID | 1.26 ms | 3.03 ms | 4,197 |

v0.12.3 is the resident index in `lume serve`. The scoreboard calls that fix 50× on SciFact (286 ms to 5.7 ms). TREC-COVID went from 7,892 ms to 99 ms. Rankings did not change. At this released speed the scoreboard says Luxir is about 16× faster on SciFact and about 80× faster on TREC-COVID.

Unreleased, measured on branch `perf/search-hot-path`. Step 1, integer term ids, with the new defaults (not the v0.12.3 defaults):

| | p50 | p99 | QPS (8 workers) |
|---|---:|---:|---:|
| SciFact | 2.60 ms | 3.73 ms | 502 |
| TREC-COVID | 10.07 ms | 14.46 ms | 236 |

The scoreboard says TREC-COVID went from 111.6 ms to 10.1 ms (11×) with those new defaults, and from 97 ms to 9.9 ms with flags off, with byte-identical rankings. This is not in v0.12.3.

## Index cost

TREC-COVID:

| | Build time | Size | Peak RAM |
|---|---:|---:|---:|
| Lume | 63 s | 1.11 GB | 2.78 GB |
| Luxir | 7.1 s | 183 MB | 103 MB |

## Capabilities

Luxir has the richer query language: phrase and proximity, boolean NOT, fuzzy terms, and facets. Lume search has SQL. `lume sql` reads `sections`, `entities`, and `entity_edges`, and `match()` returns a BM25 `score`. Lume also has time series and ARM64, including a Raspberry Pi. The scoreboard's binary sizes are 82 MB for Luxir and 102 MB for Lume (one binary for search and TI SQL).

Boolean NOT and facets through `lume sql` are marked "being verified". The Luxir report shows `match()` combined with `NOT match()` being rejected, and it shows a `GROUP BY` example without a captured result. Do not read those two cells as proven.

| | Luxir | Lume search | Lume TI SQL |
|---|:---:|:---:|:---:|
| Phrase / proximity | Yes | Partial | No |
| Boolean NOT | Yes | No | being verified |
| Fuzzy terms | Yes | Partial (spell-correct) | No |
| Facets / aggregations | Yes | No | being verified |
| Hybrid text + vector | Yes | Yes | No |
| SQL | No | Yes (`lume sql`: `sections` / `entities` / `entity_edges`, `match()` with `score`) | Yes |
| Time series | Partial | No | Yes |
| Roaring bitmaps | roaring-style (custom) | roaring-style (custom) | Yes (roaring crate) |
| ARM64 / Raspberry Pi | No (x86-64 only) | Yes | Yes |
| Binary size | 82 MB | 102 MB | (same binary) |

## Bugs this comparison found in Lume

1. `lume serve` re-read the whole index on every search. Fixed in v0.12.3: 50× faster on SciFact.
2. The MCP server dropped connections without saying so. It now sends `Connection: close`.
3. No stemming. Stemming together with coordination floor 1.0 passed the held-out NFCorpus check and is the planned default for new indexes. Not in v0.12.3.
4. Plain-text (`.txt`) sections got the scored title "Lines N-M", so title weight went to junk tokens. The title-fallback fix is opt-in: it helps TREC-COVID and hurts SciFact.
5. `lume serve`, CLI search, eval, and SQL search ignored the BM25 tuning environment variables. Fixed in `c257334`. The scoreboard says that ships in v0.12.4.
6. An audit claimed Lume splits "SARS-CoV-2". It joins that token into "sarscov2". The audit is corrected.
