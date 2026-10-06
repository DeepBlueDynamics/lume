# ti-sql

Read-only DataFusion 55.1.0 SQL over the frozen ShardSource boundary. Root integration is optional behind --features ti.

SqlSession registers telemetry, an empty docs stub, and fixture catalog tables. TelemetryProvider translates exact predicates into the shared bitmap IR, prunes vessel/time shards, assigns one partition per surviving shard, and calls ShardSource::read in batches of at most 8,192 columns. Unsupported expressions stay in DataFusion. Mean fields expose their canonical @mean column and bare alias. The source set column is List<Utf8>; SQL equality and IN are rewritten to array_has before type coercion.

FixtureSource supplies projection-aware Arrow reconstruction for the W1 MemorySource. Any durable source must return the frozen telemetry schema for the requested FieldSpecs, including vessel/ts, mean aliases, physical Float64 values, nullable scalar fields, nonnullable list items, and virtual text columns.

SQL literals for BSI fields use the shared checked fixed-point conversion. Bitmap comparisons use boundaries in reconstructed Float64 space, so values that collide above 2^53 agree with materialized values. A typed DataFusion analysis rewrite inserts an opaque ti_timestamp identity function into timestamp predicates before coercion and simplification. It also applies to prepared DataFrames with bound parameters. The helper is not registered for user SQL lookup or function catalogs.

SqlSession::explain and SQL EXPLAIN execute the same read-only plan and report its measured shard counts, bitmap cardinalities, materialized rows, and all conjunct classes, including residual Unsupported filters.

register_raw registers real-layout Parquet files under tier=raw as individual ListingTables and builds a normalization view with the D20 columns context, ts, path, value, value_str, source. It selects Signal K timestamps with received-time fallback, flattens value_<key> object columns to path.key, handles per-file scalar type differences, and uses microsecond timestamps to match the DuckDB view. It does not scan sibling quarantine directories.

## Verification

The pre-store snapshot is FixtureSnapshot JSON containing field definitions, vessel metadata, set dictionaries, and final BucketRecords. Example:

    cargo run --features ti -- ti verify --fixture crates/ti-sql/tests/fixture/snapshot.json --corpus crates/ti-sql/tests/fixture

Stored expected JSON is the default. The comparator sorts by the corpus sort keys, compares declared exact columns, and allows half one fixed-point unit for declared BSI columns. NULL differs from a value. Numeric/string keys must exist. UTC timestamp formatting is normalized between Arrow ISO output and DuckDB JSON.

For a live oracle, add --oracle duckdb --oracle-setup setup.sql. The DuckDB CLI must be on PATH; setup.sql must define raw/docs/catalog views over the same fixture files. No DuckDB engine is bundled. --raw <generated-root> registers the corresponding Parquet raw view in TI.

Each M3 report explicitly excludes entries invoking intervals, match, in_bbox, or within_nm with an M4 reason. Ordinary SQL aggregates run through DataFusion. The small checked-in fixture expected outputs are independently calculated from four rows; they are not described as DuckDB-generated. The complete generated tests/golden expected outputs remain a separate acceptance gate.

Pending integrations: durable W2 store/catalog adapter, generated correctness-set loader and full M3 corpus comparison, W5 docs materialization, and W6 exact geo refinement. Geo UDF stubs return a named unsupported error until W6 is integrated; their classifier keeps geo inexact and rejects NOT pushdown. M4 intervals and bitmap aggregate rewrites are separate work.

## Native compression exception

D27 permits zstd-sys for DataFusion's additive Parquet defaults and Arrow IPC zstd feature. No crates are vendored or patched. The SQL graph's native compression binding is zstd-sys; bzip2-sys, lzma-sys, and liblzma-sys must stay absent. Graph checks use cargo tree -p ti-sql, or cargo tree --features ti, so optional TI dependencies are actually included.

zig and cargo-zigbuild are absent in the W4 container. aarch64-musl validation remains a W8 release item.
