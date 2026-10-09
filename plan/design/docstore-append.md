# DocStore append persistence (A13 / D52)

The active format is `docs/documents.log`, version 1. A 24-byte magic/version/generation/checksum header precedes transaction frames: a 16-byte length/payload-CRC/header-CRC prefix and a JSON operation array. Upserts and tombstones commit as one frame. File synchronization precedes publication to the ordered in-memory index. A separate persistent lock file serializes writers and permits shared readers. Refresh replays only the new tail unless compaction changes the generation.

Legacy JSON migrates once, preserving `documents.json.bak`. Downgrading requires explicitly restoring that backup, which excludes changes made after migration. Earlier corruption and unknown versions fail explicitly; incomplete final transactions are repaired under an exclusive lock. Compaction replaces the active log atomically when obsolete operations exceed 50%.

## Measured update cost

Rust 1.96.0, debug profile, incremental off, Linux container on the Windows bind mount. These are relative comparisons on one filesystem, not native Pi latency. Command:

```
cargo +1.96.0 run --locked -j1 -p ti-store --example docstore_append -- --iterations 20 --dir .test-tmp
```

Each size uses the same 135-byte document body, warm-up, and 20 changed upserts. Append and snapshot timing blocks are isolated; interleaving would put each tiny append immediately after a large snapshot write.

| Live docs | Snapshot p50 / p95 ms | Append p50 / p95 ms | Snapshot bytes/update | Append bytes/update |
|---:|---:|---:|---:|---:|
| 1,000 | 15.683 / 22.374 | 6.589 / 10.183 | 317,002 | 297 |
| 10,000 | 93.033 / 99.027 | 7.379 / 9.124 | 3,170,002 | 297 |
| 50,000 | 447.663 / 490.778 | 6.257 / 7.863 | 15,850,002 | 297 |

At 50k documents the measured median update is 71.5 times faster. This is a measured filesystem/profile comparison, not a general speed guarantee.

A separate 1,000-live-document run made 2,100 updates and triggered two compactions: p50 6.409 ms, p95 9.591 ms, amortized mean 6.904 ms, maximum 24.750 ms. Thus the reported append behavior includes the periodic snapshot cost separately.

## Verification

The 13 DocStore tests passed, including legacy migration/interruption, concurrent writers, incremental reader refresh, unknown versions, earlier corruption, torn whole-batch recovery, injected append/fsync failures, and actual child-process termination at three compaction phases. Before rebasing (base 9891f31), the locked root TI suite also passed (approved atomic-index skip), including the changed comparison script executed against real CLI SQL, notes create/update/delete and match(), external text-cache refresh, vessel isolation, and alerts/rules. Both required formatting checks passed. The complete ti-store and ti-ingest suites passed, including the existing 1,000-run shard crash test, seal determinism, sealed repair, cache, resource polling, and notification tests. Strict all-target clippy passed for lume and all eight TI crates with warnings denied. The locked default build (without TI) passed with no warnings: `cargo +1.96.0 build --locked -j1 -p lume`.

Commands run (all cargo commands used CARGO_INCREMENTAL=0, local debug-info=0 overrides and lane-local TMPDIR/CARGO_TARGET_TMPDIR):

```sh
cargo +1.96.0 test --locked -j1 --features ti -- --skip search::tests::concurrent_readers_never_observe_a_partial_index
cargo +1.96.0 test --locked -j1 -p ti-store -p ti-ingest
cargo +1.96.0 clippy --locked -j1 -p lume -p ti-contracts -p ti-core -p ti-store -p ti-ingest -p ti-sql -p ti-sync -p ti-bench -p ti-geo --features ti --all-targets -- -D warnings
cargo +1.96.0 fmt --check -p ti-contracts -p ti-core -p ti-store -p ti-ingest -p ti-sql -p ti-sync -p ti-bench -p ti-geo
rustfmt +1.96.0 --check --edition 2021 src/ti_*.rs src/chat_sql.rs src/sql.rs src/document_extract.rs src/crawl_list.rs src/http_auth.rs tests/ti_*.rs tests/chat_sql.rs tests/support/*.rs
```

The comparison-script integration invokes `count_paths_q2_compare.py` using real `lume ti query --json` for document retrieval from a small append-backed store and the unchanged three expected primary-vessel windows as its cached telemetry reply. This verifies removal of the JSON persistence dependency; it is not a new full-boat or DuckDB oracle run.


After a conflict-free rebase onto a806ffb (A10 OTLP group commits and the lead's HTTP close fix), the requested reruns passed:

```sh
cargo +1.96.0 test --locked -j1 --features ti --test ti_otlp # 9 passed
cargo +1.96.0 test --locked -j1 -p lume -p ti-store --features ti --lib docs:: # 13 DocStore tests passed; root tests filtered
```

Both formatting checks above also passed on the rebased head. The full suite, clippy and default-build results above are pre-rebase gates; they were not repeated in full after the rebase. A10's OTLP byte instrumentation still reads the retired JSON filename and reports zero; this is flagged to the lead for its soak measurement update, not treated as a measured append byte count.
