# Lume and Luxir, compared

Luxir and Lume are separate programs; Lume doesn't use Luxir. Both were run under the same conditions:
- Docker with `--cpus 8 --memory 8g`;
- indexes on Linux volumes;
- warm servers, timed as the median of 3 passes;
- throughput measured at 8 concurrent workers.

Datasets: BEIR SciFact (5,183 docs, 300 queries), TREC-COVID (171,332 docs, 50 queries) and NFCorpus (3,633 docs, 323 queries, held out).

## Search quality (nDCG@10)

| | SciFact | TREC-COVID | NFCorpus |
|---|---:|---:|---:|
| **Lume BM25** | 0.677 | **0.632** | 0.314 |
| Luxir BM25 | **0.679** | 0.605 | **0.321** |
| **Lume hybrid** (same EmbeddingGemma vectors) | **0.853** | — | **0.373** |
| Luxir hybrid (same vectors, RRF) | 0.779 | — | 0.370 |

Lume wins TREC-COVID BM25 and hybrid on both sets measured. SciFact BM25 is a tie, and Luxir is slightly ahead on NFCorpus BM25.

## Speed (warm server, BM25)

| | p50 | p99 | QPS (8 workers) |
|---|---:|---:|---:|
| Lume, SciFact | 2.19 ms | — | — |
| Luxir, SciFact | 0.36 ms | 0.49 ms | 4,493 |
| Lume, TREC-COVID | 5.88 ms | 11.23 ms | 609 |
| Luxir, TREC-COVID | 1.26 ms | 3.03 ms | 4,197 |

Luxir is faster per query. Both answer in single-digit milliseconds.

## Index (TREC-COVID)

| | Size on disk | Build time | Build peak RAM | Open |
|---|---:|---:|---:|---:|
| Lume (binary index) | 354 MB | ~62 s | 3.15 GB | 0.6 s |
| Luxir | 183 MB | 7.1 s | 103 MB | — |

## What each one does

| | Lume | Luxir |
|---|---|---|
| Full-text BM25 search | Yes | Yes |
| Boolean NOT | Yes (`-term`, `NOT term`, SQL `NOT match()`) | Yes |
| Fuzzy / misspellings | Yes (spell correction) | Yes |
| Facets and filters | Yes (`--facet`, typed metadata, SQL `GROUP BY`) | Yes |
| Hybrid text + vector | Yes | Yes |
| SQL over documents | Yes (`lume sql`) | No |
| Knowledge graph from the index | Yes | No |
| Time series with SQL | Yes (Lume TI) | No |
| Postgres wire / Grafana | Yes | No |
| MCP server for agents | Yes | — |
| ARM64 / Raspberry Pi | Yes | No (x86-64 only) |
| Runs fully offline | Yes | Yes |
