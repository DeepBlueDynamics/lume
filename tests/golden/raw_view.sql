-- DuckDB view `raw` over the real signalk-parquet raw tier.
--
-- The real layout (plan/design/signalk-formats.md §1.3) differs from the corpus's
-- assumed `raw` table, so oracles target this view instead of the files directly:
--   * received_timestamp / signalk_timestamp are ISO-8601 VARCHAR, not TIMESTAMP
--   * there is no `$source` column; the sourceRef is `source_label`
--   * object paths have value_<key> columns and NO `value` column
--   * `value` type is inferred per file (DOUBLE / BOOLEAN / UTF8) and can drift
--
-- The real layout is read with hive_partitioning=false, union_by_name=true, and the
-- file list is a single glob over tier=raw only (never quarantine/ or failed/).
--
-- Load it, then materialise `raw`/`docs`/catalogs against a generated root:
--     .read tests/golden/raw_view.sql
--     CREATE OR REPLACE VIEW raw AS SELECT * FROM read_raw('<root>');
--     CREATE OR REPLACE VIEW docs AS SELECT * FROM read_parquet('<root>/docs/*.parquet',
--         hive_partitioning=false, union_by_name=true);
--     ... same for vessels/paths/shards under <root>/catalog/ ...

-- Flattened raw: one row per (context, path, timestamp, source). Scalar paths expose
-- `value` (numeric) / `value_str` (string/boolean); object paths are flattened to
-- path.<key> rows (navigation.position.{latitude,longitude}, navigation.attitude.{roll,pitch,yaw}).
CREATE OR REPLACE FUNCTION read_raw(root VARCHAR)
RETURNS TABLE (
    context VARCHAR, ts TIMESTAMP, path VARCHAR, value DOUBLE, value_str VARCHAR, source VARCHAR
) AS (
    WITH files AS (
        SELECT * FROM read_parquet(
            root || '/tier=raw/**/*.parquet',
            hive_partitioning = false, union_by_name = true
        )
    ),
    scalar AS (
        SELECT
            context,
            COALESCE(TRY_CAST(signalk_timestamp AS TIMESTAMP), TRY_CAST(received_timestamp AS TIMESTAMP)) AS ts,
            path,
            TRY_CAST(value AS DOUBLE) AS value,
            CASE WHEN TRY_CAST(value AS DOUBLE) IS NULL THEN value ELSE NULL END AS value_str,
            source_label AS source
        FROM files
        WHERE value IS NOT NULL
    ),
    position AS (
        SELECT context,
               COALESCE(TRY_CAST(signalk_timestamp AS TIMESTAMP), TRY_CAST(received_timestamp AS TIMESTAMP)) AS ts,
               'navigation.position.latitude' AS path,
               TRY_CAST(value_latitude AS DOUBLE) AS value, NULL AS value_str, source_label AS source
        FROM files WHERE value_latitude IS NOT NULL
        UNION ALL
        SELECT context,
               COALESCE(TRY_CAST(signalk_timestamp AS TIMESTAMP), TRY_CAST(received_timestamp AS TIMESTAMP)) AS ts,
               'navigation.position.longitude' AS path,
               TRY_CAST(value_longitude AS DOUBLE) AS value, NULL AS value_str, source_label AS source
        FROM files WHERE value_longitude IS NOT NULL
    ),
    attitude AS (
        SELECT context,
               COALESCE(TRY_CAST(signalk_timestamp AS TIMESTAMP), TRY_CAST(received_timestamp AS TIMESTAMP)) AS ts,
               'navigation.attitude.roll' AS path,
               TRY_CAST(value_roll AS DOUBLE) AS value, NULL AS value_str, source_label AS source
        FROM files WHERE value_roll IS NOT NULL
        UNION ALL
        SELECT context,
               COALESCE(TRY_CAST(signalk_timestamp AS TIMESTAMP), TRY_CAST(received_timestamp AS TIMESTAMP)) AS ts,
               'navigation.attitude.pitch' AS path,
               TRY_CAST(value_pitch AS DOUBLE) AS value, NULL AS value_str, source_label AS source
        FROM files WHERE value_pitch IS NOT NULL
    )
    SELECT * FROM scalar
    UNION ALL SELECT * FROM position
    UNION ALL SELECT * FROM attitude
);
