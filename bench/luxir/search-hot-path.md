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

All eight runs have byte-identical TREC output to their frozen references and zero errors at concurrency 8 and 16. TREC-COVID p50 improves 9.84× with flags off and 11.08× with F3+F2. At Step 1, the latter remained just above the <10 ms target; heap results follow below.

Both binaries use rustc 1.96.1, --features ti, thin LTO, codegen-units=1, stripped. Source base is 03e0cff; Step 1 scoring commit is 094ea55. Full binary hashes and measurements are in search-hot-path-step1.json.

One engine at a time runs with 8 CPUs and 8 GiB on luxir-bench-net. All indexes are read-only in a Linux named volume. The standalone python:3.12-slim driver is outside engine limits and uses fresh connections with connection time included. Latency is one warm-up plus three passes, taking each query's median before corpus percentiles. Throughput uses 60-second windows at concurrency 8 and 16; these are end-to-end client rates, not a claim of engine saturation. Raw results are in .lanes/data/luxir-bench/runs/lume-{baseline-clean,ids}-{off,f3-f2}-bm25-{scifact,trec-covid}.*.

The first baseline attempt overlapped a short lead formatter job and was stopped and discarded. Only baseline-clean is reported.

## Verification

Root strict clippy and touched-file rustfmt passed. Full library tests passed 79/79 twice with TMPDIR=/tmp and CARGO_TARGET_TMPDIR=/tmp/cargo-tmp on Linux storage, including concurrent_readers_never_observe_a_partial_index. Unit comparisons check exact score bits and tie order against the original scorer across BM25 variants, coordination floors, repeated terms and Unicode. A serialization test confirms derived postings do not alter the saved format. Separate quality-only runs and timed runs both passed all four corpus/profile parity checks. Target was 1.4 GiB before cargo clean; it was cleaned after the Step 1 build.

## Step 2: bounded heap

| Profile | Corpus | IDs p50 ms | Heap p50 ms | Heap p95 ms | Heap p99 ms | Heap QPS 8 | Heap QPS 16 |
|---|---|---:|---:|---:|---:|---:|---:|
| Flags off | scifact | 2.64 | 2.39 | 3.28 | 3.74 | 549.18 | 496.52 |
| Flags off | trec-covid | 9.87 | 10.37 | 13.54 | 16.11 | 308.92 | 294.00 |
| F3+F2 | scifact | 2.60 | 3.06 | 4.42 | 4.84 | 488.70 | 486.63 |
| F3+F2 | trec-covid | 10.07 | 9.11 | 11.24 | 12.44 | 303.29 | 287.41 |

Neutral overall: latency changes are mixed, so this is not a consistent speed win. TREC-COVID F3+F2 reaches 9.11 ms p50 and QPS8 improves 236.19→303.29; flags-off p50 rises 9.87→10.37 ms. SciFact moves −0.25 ms and +0.46 ms. Retain the heap for bounded hit storage and the exact top-k interface. No claim of statistical significance is made from these runs.

All four timed runs have byte-identical TREC files and zero errors at both concurrency levels; the preceding four quality-only checks also passed. Full library tests passed 83/83 twice, including the atomic-index test. Strict root clippy and rustfmt on bm25.rs/search.rs passed. Heap tests preserve score bits and ties across zero/small/oversized limits, repeated terms, Unicode, scoring variants and coordination floors. Graph scores that can reorder hits and hybrid ranking keep exhaustive scoring.

Exact provenance: binary SHA256 `f81b50453f0ee2cac6c88b4fdd7daafb385f48f8d09e627c1ed1abe2a7c3f4c9`, built from `bb25346` on `9da46af`, version string **0.12.2**, rustc 1.96.1, ti, thin LTO/CGU1. After retargeting to `3a92129`, search code in `eff1719` is byte-identical (`git diff --exit-code bb25346 eff1719 -- src/bm25.rs src/search.rs` exits 0); package version and other files differ. This measurement uses the candidate-built binary, not a newly built 0.12.3 binary. Final PR gates will rebuild the final branch. Raw runs use label `lume-heap-*`; full numbers are in search-hot-path-step2.json. Target was 1.4 GiB and was cleaned.

## Step 3: exact term MaxScore

The bounded lexical scorer may skip a document only when a conservative upper bound is strictly below the retained heap threshold. Bounds are maxima of actual rounded term contributions over the whole index. Partial bounds are summed in original query-token order with outward rounding; final scores retain that same order, including repeated terms. Equal-score ties are never pruned. Unsupported parameters, coordination floors other than 1, and single-term queries use exhaustive integer scoring. Hybrid and active graph scoring remain exhaustive.

Derived term bounds are lazy, thread-safe and capped at eight parameter profiles; they do not change the serialized index. First-use bound construction scans the relevant postings, so warm timing must not be described as cold timing.

Verified build: source `58ec10e`, version 0.12.3, rustc 1.96.1, features ti, thin LTO/CGU1. Binary SHA256 `dd260aaffb57410b5a1cf8e3f840533bc97487efeba7890b9672e906804e3fea`. Strict root clippy and touched-file rustfmt passed. Library tests passed 85/85 twice with Linux scratch, including the atomic-index regression both times. Tests compare exact score bits and ties across variants, parameter profiles and repeated terms, require actual pruning, and verify serialized format stability. All four quality-only corpus/profile checks passed with byte-identical TREC output. Debug target 1.6 GiB and release target 1.4 GiB were cleaned. Logs: `.lanes/data/luxir-bench/hot-path/step3-{build,quality}.log`.

| Profile | Corpus | p50 ms | p95 ms | p99 ms | QPS 8 | QPS 16 |
|---|---|---:|---:|---:|---:|---:|
| maxscore-off | scifact | 2.47 | 3.39 | 3.71 | 543.77 | 543.90 |
| maxscore-off | trec-covid | 8.05 | 9.64 | 9.87 | 460.07 | 452.96 |
| maxscore-f3-f2 | scifact | 2.58 | 3.36 | 3.60 | 553.11 | 541.07 |
| maxscore-f3-f2 | trec-covid | 8.16 | 10.62 | 14.69 | 529.42 | 540.27 |

All four timed runs preserve byte-identical rankings and have zero errors at concurrency 8 and 16. TREC-COVID F3+F2 p50 improves 9.11→8.16 ms (10.4%) and QPS8 303.29→529.42 (74.6%); both TREC profiles meet the <10 ms p50 target. F3+F2 p99 rises 12.44→14.69 ms, so this is not a uniform tail-latency improvement. End-to-end QPS remains client-bound. Fixed network/JSON overhead is a hypothesis pending paired stage measurements. Full values are in search-hot-path-step3.json; raw runs use lume-maxscore-*.

## Final candidate API measurement

The public candidate bitmap is exhaustive and accepts an optional allow bitmap; filtered top-k keeps whole-index scoring statistics. Neither candidate generation nor filtering uses top-k pruning to decide membership.

| Profile | Corpus | p50 ms | p95 ms | p99 ms | QPS 8 | QPS 16 |
|---|---|---:|---:|---:|---:|---:|
| candidate-off | scifact | 2.64 | 3.63 | 3.86 | 561.10 | 580.60 |
| candidate-off | trec-covid | 5.79 | 7.80 | 8.76 | 474.66 | 479.11 |
| candidate-f3-f2 | scifact | 2.34 | 3.11 | 3.40 | 589.25 | 582.88 |
| candidate-f3-f2 | trec-covid | 6.77 | 9.01 | 12.09 | 575.34 | 574.30 |

All four saved timing summaries report ranking parity. The lead reports four direct quality checks against MaxScore passing. TREC-COVID F3+F2 p50 is 6.77 ms, below the 10 ms target; the timing difference from MaxScore is not attributed solely to the API change without repeated paired measurements. Exact values and binary provenance are in search-hot-path-candidate.json.

Candidate source 830394d passed 90/90 library tests twice, touched-file formatting and strict root clippy in the build run. The full TI gate rerun is pending: its first attempt stopped because the image lacked clippy. The gate script now installs rustfmt and clippy before checking. No Rust source changed after the measured binary build.
