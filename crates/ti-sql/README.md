# ti-sql

Read-only DataFusion 55.1.0 SQL over the frozen ShardSource boundary. Root integration is optional behind --features ti.

SqlSession registers telemetry, an empty docs stub, and fixture catalog tables. TelemetryProvider translates exact predicates into the shared bitmap IR, prunes vessel/time shards, assigns one partition per surviving shard, and calls ShardSource::read in batches of at most 8,192 columns. Unsupported expressions stay in DataFusion. Mean fields expose their canonical @mean column and bare alias. The source set column is List<Utf8>; SQL equality and IN are rewritten to array_has before type coercion.

FixtureSource supplies projection-aware Arrow reconstruction for the W1 MemorySource. Durable sources return canonical requested fields with vessel/ts, physical Float64 values, nullable scalar fields, and nonnullable list items. TelemetryProvider adds mean aliases and virtual text columns. session_from_store/open_store use W2's Store directly and replace fixture vessel/path catalogs with authoritative disk batches; the shards table combines sealed manifest metadata with open shards.

SQL literals for BSI fields use the shared checked fixed-point conversion. Bitmap comparisons use boundaries in reconstructed Float64 space, so values that collide above 2^53 agree with materialized values. A typed DataFusion analysis rewrite inserts an opaque ti_timestamp identity function into timestamp predicates before coercion and simplification. It also applies to prepared DataFrames with bound parameters. The helper is not registered for user SQL lookup or function catalogs.

SqlSession::explain and SQL EXPLAIN execute the same read-only plan and report its measured shard counts, bitmap cardinalities, materialized rows, and all conjunct classes, including residual Unsupported filters.

register_raw registers real-layout Parquet files under tier=raw as individual ListingTables and builds a normalization view with the D20 columns context, ts, path, value, value_str, source. It selects Signal K timestamps with received-time fallback, flattens value_<key> object columns to path.key, handles per-file scalar type differences, and uses microsecond timestamps to match the DuckDB view. It does not scan sibling quarantine directories.

## Intervals and bitmap aggregates

    SELECT * FROM intervals('wind > 25', min_len => '5m', max_gap => '10s', vessel => NULL)

intervals(predicate_sql, min_len => '0s', max_gap => '0s', vessel => NULL) returns vessel, start, end and buckets. start/end are UTC second timestamps, with half-open intervals. Exact predicates consume Roaring next_range runs without ShardSource::read; residual predicates use DataFusion first. Runs merge per vessel across shard boundaries. max_gap measures missing bucket duration, min_len applies to the merged wall-clock span, and buckets counts matches without bridged gaps. Duration literals support ns/us/ms/s/m/h/d/w with exact fractional nanoseconds. Named arguments can appear in any order after positional arguments. The predicate must parse as exactly one SQL expression.

A physical optimizer replaces whole final/single aggregates over exact telemetry scans for count(*), count(indexed-column), and sum/min/max(BSI). Groups may be vessel and/or one fixed-duration date_bin(ts), including a constant off-grid origin. Window masks select bucket ranges directly; ShardSource::agg contributions merge with checked counts and i128 sums. Counts of nonnullable vessel/ts use CountAll. SQL NULL and empty global/grouped aggregate behavior is preserved. Calendar-month bins, multiple window sizes, residual filters, aggregate FILTER, DISTINCT, computed aggregate arguments, and other aggregate/group shapes retain ordinary DataFusion execution. EXPLAIN records the chosen path or the concrete fallback reason.

SqlSession::new_with_bitmap_aggregates(source, catalog, false) disables the rule for comparisons. The ignored synthetic_year_benchmark test compares Q4 max wind per day over 50 vessel-years at 60-second width against this materializing path, validates identical results, and prints three warm timings and their median ratio. It is a focused in-memory Q4 fixture, not the full raw fleet dataset or reference-machine percentile acceptance.

## Verification

The pre-store snapshot is FixtureSnapshot JSON containing field definitions, vessel metadata, set dictionaries, and final BucketRecords. Example:

    cargo run --features ti -- ti verify --fixture crates/ti-sql/tests/fixture/snapshot.json --corpus crates/ti-sql/tests/fixture

For an existing W2 store, use --store <root> instead of --fixture. Its deployment bucket width must match bucket_width_seconds in corpus.json. The CLI requires exactly one of these inputs and refuses a store root without a catalog directory.

Stored expected JSON is the default. The comparator sorts by the corpus sort keys, compares declared exact columns, and allows half one fixed-point unit for declared BSI columns. NULL differs from a value. Numeric/string keys must exist. UTC timestamp formatting is normalized between Arrow ISO output and DuckDB JSON.

For a live oracle, add --oracle duckdb --oracle-setup setup.sql. The DuckDB CLI must be on PATH; setup.sql must define raw/docs/catalog views over the same fixture files. No DuckDB engine is bundled. --raw <generated-root> registers the corresponding Parquet raw view in TI.

Each M3 report explicitly excludes entries invoking intervals, match, in_bbox, or within_nm with an M4 reason. Eligible aggregates use BitmapAggregateExec; other aggregates run through DataFusion. The small checked-in fixture expected outputs are independently calculated from four rows; they are not described as DuckDB-generated. The complete generated tests/golden expected outputs remain a separate acceptance gate.

Pending integrations: generated correctness-set loader and full M3 corpus comparison, W5 docs materialization, and W6 exact geo refinement. Geo UDF stubs return a named unsupported error until W6 is integrated; their classifier keeps geo inexact and rejects NOT pushdown. Full M4 text/geo/golden acceptance and reference-machine performance remain pending.

## Native compression exception

D27 permits zstd-sys for DataFusion's additive Parquet defaults and Arrow IPC zstd feature. No crates are vendored or patched. The SQL graph's native compression binding is zstd-sys; bzip2-sys, lzma-sys, and liblzma-sys must stay absent. Graph checks use cargo tree -p ti-sql, or cargo tree --features ti, so optional TI dependencies are actually included.

zig and cargo-zigbuild are absent in the W4 container. aarch64-musl validation remains a W8 release item.
