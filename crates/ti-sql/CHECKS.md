# W4 foundation checkpoint

Verified in the W4 container on 2026-10-06. The SQL tests, compile, format and strict clippy were repeated after merging the W2 workspace at 20d1b96; the SQL Store adapter remains pending:

- cargo test -p ti-sql: 12 passed (1 unit, 11 integration). Includes plain fractional timestamps, Grafana string BETWEEN, now()-relative pruning, prepared DataFrame bound timestamp parameters, hidden internal function, NULL/set/source semantics, Float64 collisions above 2^53, 8,193-row batching, read-only SQL features, raw real-layout/schema checks, and stored-output comparison.
- cargo test -p lume: 46 passed; existing src/main.rs unused cands assignment warning.
- cargo test -p ti-core: 8 edge tests, 1 golden, all four 10,000-case property tests passed (33.97 seconds for the properties).
- cargo check -p lume --features ti: passed after the timestamp analyzer replacement and W2 workspace merge; existing root cands assignment warning.
- cargo fmt -p ti-sql --check: passed.
- cargo clippy -p ti-sql --all-targets -- -D warnings: passed after final test formatting.
- git diff --check: passed.

Codec checks: cargo tree -p ti-sql -i bzip2-sys, -i lzma-sys, and -i liblzma-sys each reported no matching package. zstd-sys is present and allowed by D27. No vendoring or patches.

zig and cargo-zigbuild are absent in the W4 container. aarch64-musl validation remains the W8 release item.

The small stored-output fixture has six active checks and four explicit M4 exclusions. Its expected rows are independently hand-calculated, not claimed as DuckDB output. Main tests/golden comparison and real-store verification remain pending at this checkpoint; no full M3 completion claim.

## W2 Store adapter checkpoint

Verified before the container/pane replacement on 2026-10-06:

- cargo test -p ti-sql: 13 passed (1 unit, 12 integration), including real Store reads, mean aliases and virtual NULL columns, all-column equality after sealing/reopening, authoritative sealed metadata, and six stored-output checks on both mutable and reopened sealed Store paths.
- cargo clippy -p ti-sql --all-targets -- -D warnings: passed.
- cargo clippy -p ti-store --test read_lists -- -D warnings: passed before the final frozen-schema equality assertion was added.
- cargo fmt -p ti-sql -p ti-store --check: passed.

Verified after resuming in the replacement container:

- cargo test -p ti-store: 10 unit tests, the 1,000-case crash recovery test (318.54 seconds), the source/Geo frozen-schema and open/sealed regression, and all-field seal determinism passed.
- cargo tree --features ti -i bzip2-sys/lzma-sys/liblzma-sys: each reported no matching package.
- cargo tree -p lume --depth 1: four default direct dependencies.
- git diff --check: passed.

The replacement container has rustc 1.99.0 but no rustfmt or clippy components. The lead will run scoped formatting and strict rustc 1.96 clippy on the host at merge. The actual root CLI fixture build was interrupted; no CLI runtime pass is claimed. The generated correctness set is incomplete and its expected outputs remain pending, so full M3 acceptance is still open.

## W4 part 2 checkpoint

Verified in the replacement container on 2026-10-06:

- cargo test -p ti-sql: 20 passed (4 units, 16 integration); the one performance test is ignored by default. The final run includes cross-shard interval boundaries, named arguments, exact max-gap/min-length thresholds, matching-only bucket counts, residual equivalence, actual zero telemetry materialization on the bitmap interval path, aggregate selection/fallback diagnostics, deterministic randomized comparisons to materializing DataFusion, empty/all-NULL aggregates, off-grid date_bin origins, and preservation of DataFusion's NULL-origin error.
- cargo test -p ti-sql --test m4 synthetic_year_benchmark -- --ignored --nocapture: passed. Q4 over 50 vessel-years at W=60s (26,280,000 buckets) returned identical results on both paths. Debug-profile medians: bitmap 3.008692312s, materialized 47.869879320s, 15.91× ratio. See BENCHMARK.md for all timings, workload and limits.
- git diff --check: passed.

The aggregate comparisons use 3 vessels × 65,700 candidate buckets with a deterministic sparse universe and NULLs, 5 thresholds × 4 grouping shapes, and an all-NULL selection. They compare all count/count(column)/sum/min/max result rows against a session with the optimizer disabled. No runtime dependencies or frozen contracts changed.

Formatting and strict rustc 1.96 clippy remain assigned to the lead's host because this container has neither component. The release profile is absent; an optional W=10s release measurement was not run. Full M3 generated expected-output acceptance and full M4 text/geo/reference-machine acceptance remain separate.

## W6 geo checkpoint

Verified on ti/w6-geo based on 8afa646, 2026-10-06:

- Combined `cargo test -p ti-sql -p ti-ingest -p ti-geo`: SQL 21 passed / 2 ignored; ingest 23 passed / 2 ignored; geo 6 passed. Geo coverage includes 600 random bbox/radius cases, poles, antimeridian and degenerate boundaries; SQL covers missing coordinates, NOT, OR, legacy absent H3 rows and exact zero radius. Ingest seals and reads real res5/7/9 cell rows; no placeholder cell 0 is stored.
- `cargo check -p lume --features ti`: passed; only the existing root `cands` assignment warning.
- Real-data M2 oracle replay: `cargo test -p ti-ingest --test m2_gate test_m2_oracle_replay_24h -- --ignored --nocapture` passed with workspace TMPDIR and TI_DATA_DIR. Read 208,937 raw points from 25 Parquet files; all 460,112 emitted BucketRecords matched, zero unmatched.
- Res9 local uniform grid: 22,650 exact matches, 27,085 candidates, 4,435 false-positive buckets =16.3744% of candidates. No universal false-positive rate is claimed.
- `cargo tree -p ti-geo -d`: no duplicate geo/geo-types; normal/build dependencies contain no native geometry/C compiler package.
- `git diff --check`: passed.

Geo SQL binds implicit @last coordinates in typed analysis. OR(H3 cover, BSI envelope) is Inexact and retains exact bbox/haversine residuals; NOT of geo remains Unsupported. The envelope preserves independently reported/preferred-source coordinates, rounding and old shards; no frozen contracts changed. The lead approved D31 and this safety design.

The ignored Q7 stored-output gate requires `TI_DATA_DIR`, `TI_EXPECTED_DIR` and `TMPDIR` under the workspace. It builds one vessel's relevant raw partitions into a scratch Store. q7-005 remains pending W5 because it also invokes match(). q7-006's missing TI GROUP BY is reported to the corpus owner. Shared expected.log reports empty output for all six Q7 entries; those comparisons alone cannot establish corpus recall. Nonempty local geo fixtures provide separate correctness evidence.

The container has no rustfmt/clippy; Rust 1.96 formatting and strict clippy remain assigned to Pike's host at merge.

## W7 CLI/MCP first slice

Verified in the container on 2026-10-06, based on 3354bd0:

- `cargo test --features ti`: 51 passed, zero failures, including the three MCP shape/registration/cap tests. The existing root cands assignment warning remains.
- `cargo test -p ti-sql --lib --test surfaces`: 7 unit + 4 surface tests passed. Includes all four CLI parsers, invalid flags/positionals, persisted/config/header width conflicts, canonical @mean names and m/s units, read-only rejection, open shard status, and idempotent Parquet document import.
- All builds use `CARGO_INCREMENTAL=0`, two compile jobs, workspace TMPDIR, and one build at a time. Interim target measurement 2.8 GB, below the 15 GB budget.
- `cargo build --features ti`: passed. `target/debug/lume ti query "SELECT count(*) FROM telemetry" --store /workspace/lume/.lanes/data/store-full` exited successfully and printed 3,974,400. Cold startup took several minutes including width validation and snapshot opening; no latency acceptance claim.
- Default `cargo build` (without ti): passed; existing root cands warning only. Final target measurement 3.3 GB. No parallel full builds or incremental artifacts.
- Host Rust 1.96 formatting and strict clippy are assigned to Pike, confirmed by Hyperia mail.

MCP query envelopes cap both decoded JSON and encoded text content, with a 500-row ceiling and a 64 KiB budget. Large UTF-8 rows are omitted with a truncation hint. Status distinguishes unavailable ingest/sync metrics instead of reporting zero lag. HTTP/pgwire and the remaining M5 surfaces are outside this slice.

## W7 width and shared HTTP checkpoint

Verified in the container on 2026-10-06 after rebasing ti/w7-surfaces onto 26e535a (including 20117c4 and the host scratch-dir fixes):

- `cargo test -p lume -p ti-sql --features ti`: root 51 unit + 1 HTTP integration passed; ti-sql 29 passed, 2 existing tests ignored; zero failures. This includes the full root feature-ti suite and all ordinary SQL integration tests.
- The HTTP integration starts the real CLI on 0.0.0.0 with port 0 and a sealed two-row Store in CARGO_TARGET_TMPDIR. It checks all four routes, Arrow IPC decoding and units/canonical names, JSON ti_query shape/equivalence, row truncation, empty timestamp schema, large UTF-8 byte cap, DML rejection with HTTP 400, and HTTP/MCP reuse after hiding the catalog directory.
- Default `cargo build` (without ti) passed after rebase. Only the existing root cands assignment warning remains.
- `git diff HEAD --check`: passed after conflict resolution. No dependencies or frozen contracts changed.
- Fresh-process `time target/debug/lume ti status --store /workspace/lume/.lanes/data/store-full`: before 85.546 s, after 70.199 s; identical observed status, 17.94% elapsed decrease. The baseline was 6afb2ab; the after run used the representative-header change before HTTP. OS caches were not purged. Remaining snapshot/catalog startup profiling belongs to Pike.
- All builds used CARGO_INCREMENTAL=0, two jobs, one build at a time and workspace TMPDIR. Final target measurement 5.8 GB, below 15 GB.
- Rust 1.96 fmt/strict clippy for this slice remain assigned to Pike's host; not run in this container.

HTTP and configured-server MCP requests share one startup TiEngine/runtime through Arc, with serialized admission and diagnostic reset. Arrow IPC stream-format responses are bounded and buffered before HTTP writing, with row count/truncation/hint headers and units metadata. Catalog/shard refresh, ingest supervision, LAN-only binding, NUTS/bearer auth and pgwire are outside this slice; see W7-serve.md.
