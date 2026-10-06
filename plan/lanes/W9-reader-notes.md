# Generic Parquet reader: first slice

`lume ti backfill --parquet 'data/*.parquet' --entity entity_id --time timestamp --time-unit ms --wide --units units.toml --store store`

For long data, replace `--wide` with `--metric metric --value value`. `--entity-constant` supplies an explicit canonical entity URN. Optional flags: `--timezone UTC` (or a fixed offset), `--source source_column`, `--prefix robot.`, and `--exclude column1,column2`. With no `--parquet`, mappings come from `[[sources.parquet]]` in the store's `ti.toml`. Units files contain a `[units]` table, for example `"*.current" = { unit = "A", scale = 2 }`. Use literal TOML strings for Windows paths.

Input uses projected Arrow batches of 8,192 rows. Null entity/time rows are counted and skipped. Naive timestamp/date/text values require an explicit timezone; integer epochs are UTC. File globs and bucket emission are sorted. Dictionary values remain categorical. Numeric unit/scale misses, type changes, and unsupported wide columns appear under `parquet_import` in `lume ti status`.

Historical windows close per entity at its maximum observed time minus `sources.backfill.lateness_seconds` (default 3600). Closed accumulators live in a per-run disk journal under the store root so late rows can reload, merge, and rewrite exact sums/counts and preferred-source state. Reimports start fresh; abandoned journals are discarded rather than replayed. Normal completion and errors remove scratch. Transactions are capped at 50,000 records; multiple rewrites of one bucket cannot share a transaction. Sinks flush every 256 files and after emission.

`[sources.backfill]` caps `max_active_bytes` (default 128 MiB, conservative allocation estimate), `max_index_entries` (2,000,000), and `max_journal_bytes` (4 GiB). Hitting a cap returns a clear error recommending a smaller range, sorted input, or adjusted limits. Backfill reports peak active windows/estimated bytes, journal bytes, and late reloads through `ti status`. These caps cover reader accumulators and lookup scratch; the storage engine and Arrow reader have their own allocations.

This slice deliberately precedes D38 entity generalization, mapped document imports, and the robot-fleet oracle. Existing fixture IDs use Signal K-compatible URNs until D38 lands. Verification includes long/wide reader fixtures, timestamps/dates/timezones, null accounting, aggregation across an Arrow batch boundary, categorical fields, deterministic reseals, and an executable CLI backfill/query/status test.
