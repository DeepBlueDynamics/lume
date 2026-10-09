# CRoaring evaluation harness

Standalone benchmark workspace: pinned roaring 0.11.5 / croaring 2.8.0, with its own lockfile. Never add it to the production dependency graph (D39). Unsafe view construction uses only validated, immutable, exact-length self-serialized buffers; Frozen slices have library-provided 32-byte alignment. This is not an untrusted store decoder.

Build and run sequentially, with no concurrent builds:

```sh
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR="$PWD/target" cargo build --release --locked --manifest-path bench/croaring-eval/Cargo.toml
target/release/croaring-eval 3000
```

Both backends run in one process: nine rounds, alternating backend order, with warmup, black-box inputs/results, medians and ranges. Correctness assertions precede timings: naive BSI range, nonempty chain equality, interval equality and portable cross-deserialization. BSI is the high-to-low unsigned equal-prefix range algorithm over 12 bit planes and 65,536 buckets; it does not measure full signed Predicate evaluation or disk I/O. CRoaring owned/Frozen rows are run-optimized before timing. Portable views read the original roaring-produced bytes in every case. Reported serialization sample is the highest bit plane, including one long run for the run-heavy dataset; alignment padding is excluded from frozen payload size.

For mmap adoption, separately measure verified-file parsing, backing buffer residency/lifetime, RSS over many shards, Pi/aarch64-musl portability and end-to-end SQL. This microbenchmark measures warm in-memory CPU operations and per-row open cost only.
