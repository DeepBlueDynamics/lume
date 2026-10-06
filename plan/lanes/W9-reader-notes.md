# Generic Parquet reader: first slice

`lume ti backfill --parquet 'data/*.parquet' --entity entity_id --time timestamp --time-unit ms --wide --units units.toml --store store`

For long data, replace `--wide` with `--metric metric --value value`. `--entity-constant` supplies an explicit canonical entity URN. Optional flags: `--timezone UTC` (or a fixed offset), `--source source_column`, `--prefix robot.`, and `--exclude column1,column2`. With no `--parquet`, mappings come from `[[sources.parquet]]` in the store's `ti.toml`. Units files contain a `[units]` table, for example `"*.current" = { unit = "A", scale = 2 }`. Use literal TOML strings for Windows paths.

Input uses projected Arrow batches of 8,192 rows. Null entity/time rows are counted and skipped. Naive timestamp/date/text values require an explicit timezone; integer epochs are UTC. File globs and bucket emission are sorted. Dictionary values remain categorical. Numeric unit/scale misses, type changes, and unsupported wide columns appear under `parquet_import` in `lume ti status`.

Historical bucket windows remain in memory across all input batches/files so aggregation and preferred-source selection do not depend on Arrow boundaries or file splits. Input buffers are bounded; accumulation memory currently grows with the history imported. Transactions are capped at 50,000 records and bucket rewrites are kept together. Sinks flush every 256 input files and after emission; the intermediate flush does not release historical windows. Bounded historical accumulation is a follow-up optimization requiring an explicit ordering contract or a spill mechanism.

This slice deliberately precedes D38 entity generalization, mapped document imports, and the robot-fleet oracle. Existing fixture IDs use Signal K-compatible URNs until D38 lands. Verification includes long/wide reader fixtures, timestamps/dates/timezones, null accounting, aggregation across an Arrow batch boundary, categorical fields, deterministic reseals, and an executable CLI backfill/query/status test.
