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
Performance measurements, full TI tests and the host corpus gate remain pending.
