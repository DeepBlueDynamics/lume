# W4 Q4 bitmap aggregate measurement

Measured in the W4 Linux x86-64 container on 2026-10-06 with rustc 1.99.0, DataFusion 55.1.0, and Cargo's unoptimized debug test profile. The benchmark source is tests/m4.rs::synthetic_year_benchmark. No external oracle or generated raw files were used.

The deterministic in-memory fixture contains 50 vessels, 365 days, 60-second buckets, one signed integer BSI wind field and one presence field: 26,280,000 existing buckets. Fixture construction took 64.429 seconds. Both sessions share the same ShardSource and catalog; the baseline disables only the bitmap aggregate optimizer. The query groups by vessel and UTC day and computes max(wind), then orders by the same keys.

Command:

    cargo test -p ti-sql --test m4 synthetic_year_benchmark -- --ignored --nocapture

The test first compares both results, runs bitmap EXPLAIN to verify optimizer selection, and measures three further executions per path. Timing includes query planning and collection; equality formatting occurs outside the measured interval. All four result comparisons passed.

| Warm run (sorted by duration) | Bitmap aggregate (seconds) | Materializing DataFusion (seconds) |
|---|---:|---:|
| 1 | 2.972785796 | 47.020240775 |
| 2 | 3.008692312 | 47.869879320 |
| 3 | 3.047159519 | 47.982873595 |

Median ratio: **15.91×** (47.869879320 / 3.008692312). The ignored test passed and took 270.92 seconds including construction, correctness warmup, EXPLAIN execution, timings and comparisons.

This measures the focused Q4 aggregate replacement over 50 vessel-years. It does not establish the reference-machine 300 ms p95 target, release performance, full 120-path raw fleet performance, Store I/O, cold-cache behavior, or full M4 text/geo/golden acceptance. The bucket width here is 60 seconds; the complete synthetic raw fleet specification uses a different workload. Full M3 stored-output acceptance remains separate.
