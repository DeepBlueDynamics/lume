# Sealed-shard query cache

Read-only queries previously decoded every field of a sealed shard for eval,
materialization and aggregation separately. A same-root shared LRU now retains
immutable decoded fields behind Arc, keyed by vessel, shard, version, seal hash
and field ID. A derived universe entry preserves buckets belonging only to an
unprojected field. New manifest versions invalidate that shard's old entries;
unchanged versions survive TiEngine reloads. Mutable shards bypass the cache.

The first universe miss uses the existing complete loader, preserving NULL and
bucket-presence semantics. Further misses read only requested fields. Shard
encoding, WAL and seal hashes are unchanged. A zero-byte budget runs the previous
complete-loader path.

Configure each physical store's ti.toml:

```toml
[query]
sealed_cache_bytes = 67108864 # 64 MiB, recommended for the Pi
```

The default is 268435456 (256 MiB). Zero disables caching. Retained allocations
use a conservative charge including decoded bitmap expansion, dictionaries, Arc
and LRU-index overhead. Entries larger than the budget are not retained; eviction
precedes insertion. Active query batches and transient cold decoding allocations
are outside this retained-cache budget. Concurrent misses may decode the same
immutable field independently.

## Reproducible timing

Build ti-bench once, then run the same binary for both sides:

```sh
CARGO_INCREMENTAL=0 cargo build --release -p ti-bench
python3 bench/shard_cache.py --binary target/release/ti-bench \
  --store .lanes/data/store-full --out .lanes/data/query-cache/native \
  --toolchain 'rustc 1.96 release' --iterations 7 --cache-bytes 268435456
```

Use absolute paths when running from the lane; Windows can use ti-bench.exe and
Python. Output goes outside git, and the store is never modified. Twenty unchanged
golden point, predicate, aggregate and interval/geo/broad-scan queries are selected.
The benchmark engine does not register document search; the full corpus gate
separately checks text queries.

Cold means the decoded application cache was cleared immediately before each
query, not that the OS disk cache was flushed. Warm runs follow that query's cold
run. The report includes cold and warm p50 times, cache charge/counters, row counts
and an order-independent same-binary result fingerprint; it fails on different
answers. Fingerprinting is outside the measured query duration. Native host
timings are needed when the Windows bind mount dominates local reads.

## Verification

Cache tests cover NULL/universe semantics, cache-off and tiny-budget equality,
repair-version isolation, same-root reopen reuse, concurrent reads, LRU eviction,
config validation and measurement controls. The existing full-loader/serializer
tests check the refactor's compatibility. On rustc 1.99, contracts/store tests passed
60/60 with the 1,000-process crash gate explicitly skipped; strict clippy on
contracts/store/sql/bench passed, and Python bench tests passed 34/34. The benchmark
fingerprint unit test compiled under all-target clippy but has not run locally.
Native performance and the host corpus gate are recorded below; full TI tests remain pending.

## Native host measurements (2026-10-07)

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

