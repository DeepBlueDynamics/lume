# Search hot-path measurements

Step 1 replaces per-document byte-key TF lookups with integer TF postings. Scoring still visits query tokens in their original order, including repetitions. Coordination, tag filtering and full sorting are unchanged. Runtime postings are derived lazily and never serialized.

## Matched Step 1 result

| Build | Profile | Corpus | p50 ms | p95 ms | p99 ms | QPS 8 | QPS 16 |
|---|---|---|---:|---:|---:|---:|---:|
| Baseline | Flags off | scifact | 7.25 | 11.62 | 14.40 | 493.41 | 482.30 |
| Baseline | Flags off | trec-covid | 97.11 | 147.82 | 183.89 | 70.80 | 59.67 |
| Baseline | F3+F2 | scifact | 9.05 | 14.12 | 17.50 | 483.90 | 475.63 |
| Baseline | F3+F2 | trec-covid | 111.56 | 174.33 | 230.95 | 60.13 | 50.78 |
| Integer postings | Flags off | scifact | 2.64 | 3.56 | 3.81 | 529.03 | 522.66 |
| Integer postings | Flags off | trec-covid | 9.87 | 12.77 | 13.66 | 273.56 | 305.61 |
| Integer postings | F3+F2 | scifact | 2.60 | 3.38 | 3.73 | 502.16 | 506.40 |
| Integer postings | F3+F2 | trec-covid | 10.07 | 13.45 | 14.46 | 236.19 | 240.98 |

All eight runs have byte-identical TREC output to their frozen references and zero errors at concurrency 8 and 16. TREC-COVID p50 improves 9.84× with flags off and 11.08× with F3+F2. The latter remains just above the <10 ms target; the heap and exact pruning steps are not implemented yet.

Both binaries use rustc 1.96.1, --features ti, thin LTO, codegen-units=1, stripped. Source base is 03e0cff; Step 1 scoring commit is 094ea55. Full binary hashes and measurements are in search-hot-path-step1.json.

One engine at a time runs with 8 CPUs and 8 GiB on luxir-bench-net. All indexes are read-only in a Linux named volume. The standalone python:3.12-slim driver is outside engine limits and uses fresh connections with connection time included. Latency is one warm-up plus three passes, taking each query's median before corpus percentiles. Throughput uses 60-second windows at concurrency 8 and 16; these are end-to-end client rates, not a claim of engine saturation. Raw results are in .lanes/data/luxir-bench/runs/lume-{baseline-clean,ids}-{off,f3-f2}-bm25-{scifact,trec-covid}.*.

The first baseline attempt overlapped a short lead formatter job and was stopped and discarded. Only baseline-clean is reported.

## Verification

Root strict clippy and touched-file rustfmt passed. Full library tests passed 79/79 twice with TMPDIR=/tmp and CARGO_TARGET_TMPDIR=/tmp/cargo-tmp on Linux storage, including concurrent_readers_never_observe_a_partial_index. Unit comparisons check exact score bits and tie order against the original scorer across BM25 variants, coordination floors, repeated terms and Unicode. A serialization test confirms derived postings do not alter the saved format. Separate quality-only runs and timed runs both passed all four corpus/profile parity checks. Target was 1.4 GiB before cargo clean; it was cleaned after the Step 1 build.
