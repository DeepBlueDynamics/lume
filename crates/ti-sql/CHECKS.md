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
