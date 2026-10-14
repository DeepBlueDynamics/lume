# Benchmarks

What Lume measures, how, and the current numbers. Every number comes from a release build, run with a command or test in this repository.

## Search: Lume vs Luxir

Luxir is a separate, fast BM25 engine. It's the bar we measure search against. Luxir isn't part of Lume, and Lume doesn't use it.

**How it's run:**
- Both engines run in Docker with 8 CPUs and 8 GiB, with indexes on Linux volumes.
- One client sends the queries.
- Servers are warm: one warmup pass, then 3 timed passes, and we report the median.
- Throughput is measured with 8 concurrent workers.

**Datasets** (from [BEIR](https://github.com/beir-cellar/beir)):

| Dataset | Documents | Queries |
|---|---:|---:|
| SciFact | 5,183 | 300 |
| TREC-COVID | 171,332 | 50 |
| NFCorpus | 3,633 | 323 |

NFCorpus is held out: no setting was chosen on it.

### Quality (nDCG@10)

| | SciFact | TREC-COVID | NFCorpus |
|---|---:|---:|---:|
| **Lume BM25** | 0.677 | **0.632** | 0.314 |
| Luxir BM25 | **0.679** | 0.605 | **0.321** |
| **Lume hybrid** | **0.853** | — | **0.373** |
| Luxir hybrid (RRF) | 0.779 | — | 0.370 |

Both hybrid runs use the same EmbeddingGemma 2 vectors (768 dimensions).
- Lume wins TREC-COVID BM25 and hybrid search on both datasets.
- SciFact BM25 is a tie.
- Luxir is slightly ahead on NFCorpus BM25.

### Speed (warm server, BM25)

| | p50 | p99 | QPS |
|---|---:|---:|---:|
| Lume, SciFact | 2.19 ms | — | — |
| Luxir, SciFact | 0.36 ms | 0.49 ms | 4,493 |
| Lume, TREC-COVID | 5.88 ms | 11.23 ms | 609 |
| Luxir, TREC-COVID | 1.26 ms | 3.03 ms | 4,197 |

Luxir is faster per query. Both answer in single-digit milliseconds.

### Index (TREC-COVID)

| | Size on disk | Build | Build peak RAM | Open |
|---|---:|---:|---:|---:|
| Lume, binary index | 354 MB | ~62 s | 3.15 GB | 0.6 s |
| Luxir | 183 MB | 7.1 s | 103 MB | — |

The default v4 binary index returns byte-for-byte the same search results as the legacy JSON index (`LUME_INDEX_FORMAT=json`).

### What each one does

| | Lume | Luxir |
|---|---|---|
| Full-text BM25 search | Yes | Yes |
| Boolean NOT | Yes | Yes |
| Fuzzy / misspellings | Yes (spell correction) | Yes |
| Facets and filters | Yes | Yes |
| Hybrid text + vector | Yes | Yes |
| SQL over documents | Yes (`lume sql`) | No |
| Knowledge graph from the index | Yes | No |
| Time series with SQL | Yes (Lume TI) | No |
| Postgres wire / Grafana | Yes | No |
| ARM64 / Raspberry Pi | Yes | No (x86-64 only) |
| Runs fully offline | Yes | Yes |

## On a Raspberry Pi 5

The published arm64 release binary on a Pi 5 (4 cores), with Signal K running alongside, on SciFact:

| | Result |
|---|---:|
| Build the index | 9.0 s, 153 MB peak RAM |
| Index on disk | 15 MB |
| Open the index | 123 ms |

## Lume TI (telemetry)

**Dataset:** the deterministic generated fleet from `ti-bench gen`. That's 5 vessels × 90 days around Monterey Bay: 95.9 M values in 11,500 `signalk-parquet` files, plus 1,610 notes, logbook entries and alerts.

Because the data is deterministic, every answer is checked exactly against an independent DuckDB run over the same files.

**Correctness**

| Check | Result | Source |
|---|---|---|
| Golden SQL corpus vs an independent DuckDB oracle: telemetry, text, geo, `intervals()`, joins | **58 of 58** runnable queries match | `lume ti verify --corpus tests/golden` |
| 24-hour stream replay vs an independent oracle | **153,374 / 153,374** bucket records match | `crates/ti-ingest/tests/m2_gate.rs` |
| Re-ingesting the full 95.9 M values into the same store | all 65 sealed shards **byte-identical** | `m2_gate.rs` |
| 1,000 seeded `kill -9` runs during ingest | **0 lost, 0 duplicated** acknowledged records | `crates/ti-store/tests/crash_recovery.rs` |
| Plain words → column (`ti_resolve`), 100 phrases over 488 columns | right column in the top 3 for **93 / 100** | `tests/ti_resolve.rs` |

**Speed and size**

| Measurement | Result | Source |
|---|---|---|
| Year-long "max wind per vessel per day" (5 vessel-years, 15.8 M buckets) | **40.3 ms**, 72× faster than DataFusion over the same rows, with identical results | `crates/ti-sql/tests/m4.rs` |
| Bit-sliced range compare over a 65,536-bucket shard | **41 µs** | `crates/ti-core/examples/compare_baseline.rs` |
| Live ingest (decode → normalize → bucket → store) | **181,783 values/s**, 72 MB peak RAM | `m2_gate.rs` |
| Parquet backfill of 95.9 M values | **246,730 values/s** | `crates/ti-ingest/examples/backfill_store.rs` |
| Store size | **554 MB**, 0.29× the raw Parquet | `backfill_store` |
