# W9: generic time-series Parquet (any `(entity, metric, time, value)` data)

Depends on: W3 ingest (bucketer, classifier, backfill), W5 docs, W4 golden harness.
Spec touch points: [05-data-model](../spec/05-data-model.md) (scales, units), [06-ingest](../spec/06-ingest.md) (backfill), [10-contracts](../spec/10-contracts.md) (entity identity), [14-semantics](../spec/14-semantics.md).

Signal K stays the first-class, zero-config input. W9 adds a second front door, so that any time-series Parquet lands in the same store beside its documents. Examples: sensor fleets, robots, industrial historians, vehicle logs and observability metrics. Everything downstream of the reader (bucketing, bitmaps, WAL, sealing, SQL, `match()`, geo, `intervals()`, sync) is already generic. It consumes `RawDataPoint { context, path, source, timestamp, value }`.

## Owns

A column-mapped Parquet reader that produces `RawDataPoint`s. Also the mapping config, the identity rule that replaces `vessels.urn:`, and a units and scales table for non-Signal K data.

## Tasks

- [x] **Mapping config** (`[[sources.parquet]]` in `ti.toml`, and the same as CLI flags):
  - `files`: a glob;
  - `entity`: a column name, or a constant for single-entity files;
  - `time`: a column, plus its unit (`s`, `ms`, `us`, `ns`, or RFC 3339 text) and timezone;
  - `format`: `long` (`metric` and `value` columns) or `wide` (every other numeric, boolean or string column is a metric, and `exclude` drops columns);
  - optional `source` column;
  - optional `prefix` prepended to metric names (`robot.`).
- [x] **Reader:** a streaming arrow-rs reader using the projection from the mapping. Timestamp, Date and integer-epoch time columns are supported. Rows with a null entity or time are counted and skipped, never guessed. It reuses the existing 50k-record apply chunking and the 256-file flush cadence.
- [x] **Classification:**
  - numeric columns map to BSI;
  - string, boolean and dictionary columns map to single-valued set fields, which keeps the D21 rule.
  - A per-metric scale and unit come from a `[units]` table (`"*.current" = { unit = "A", scale = 2 }`). Without one, the scale falls back to 3 and the miss is logged, as for unknown Signal K paths.
- [x] **Identity (contract change, needs a decisions-log entry):**
  - generalize `vessels.urn:` to an entity URN rule, so a URN may start with any `<kind>.urn:` prefix (`vessels.`, `robots.`, `devices.`).
  - Signal K behaviour stays byte-identical, and existing seal hashes must not change.
  - The SQL column stays `vessel` for compatibility, with an `entity` alias.
- [x] **CLI:** `lume ti backfill --parquet "<glob>" --entity <col> --time <col> (--metric <col> --value <col> | --wide) [--units units.toml] --store <root>`. The existing `backfill_store` example becomes a thin wrapper over the same entry point.
- [x] **Documents beside non-Signal K data:** `lume ti import-docs` takes the same mapping (entity, time-start column, optional time-end column, kind, title and body columns). An operator log or incident table then lands in `docs` and works with `match()`.
- [x] **Golden corpus for generic Parquet:** generate a wide-format robot-fleet dataset (`ti-bench gen --profile robots`: 3 robots, 2 days, battery, motor current, mode and fault states, pose). Copy 10 to 15 of the existing golden queries over to it, with DuckDB oracles reading the same Parquet directly. Verify with `lume ti verify`.

## Acceptance

- Long and wide fixtures backfill into a store, and `lume ti verify` is green on the robot-fleet golden set.
- Signal K regression: re-backfilling the golden boat fleet gives identical seal hashes and keeps the boat corpus at 58/0/4.
- A mixed store holds a Signal K boat and a generic-Parquet robot fleet side by side, and one query reads both (`GROUP BY vessel`).
- Unit, scale and classification misses are reported in `lume ti status`, not silently defaulted.

## Out of scope (separate lanes)

- **Sub-second buckets for high-rate data** (100 Hz+ IMU or motor control). Today's widths must divide 3600 with a 1 s floor, so finer widths are a contract change.
- **Live non-Signal K inputs** (MQTT, ROS 2 topics). They would emit the same `RawDataPoint`s through the same bucketer.

## Open

- **Wide files with hundreds of columns:** should there be a column allow-list by default, so a 2,000-column historian export doesn't create 2,000 × 3 bitmap fields?
- **Multiple values for the same (entity, metric, time):** keep last-by-file-order, or use a `source` column to pick the preferred source as Signal K does?

## Verification status

Local: long/wide reader and CLI fixtures pass; mapped documents are idempotent; the mixed boat/robot store verifies 14/0/0, including `match()` and document joins. Root `cargo test --features ti` passes (70 tests, two ignored host gates). The 121-file bounded-window test covers exact late mean/count rewrites, all three caps, scratch cleanup, and roughly flat Linux RSS. Generator regeneration is byte-identical.

Host acceptance still required: `tests/golden/robots/oracle.py` must independently check all 14 nonempty DuckDB results; the full Signal K fleet re-backfill must retain identical seal hashes and corpus 58/0/4; strict fmt/clippy checks are run by the lead. These pending gates are not covered by the checked implementation boxes above.
