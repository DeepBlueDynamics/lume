# ti-ingest

`ti-ingest` provides the streaming and batch ingestion pipeline for Lume TI.

## Pipeline Architecture

1. **Decode (`decode.rs`)**: Signal K delta decoder, ISO 8601 timestamp parsing, and 3-tier source resolution (`$source` -> `source.label` -> `context/default`).
2. **Normalize (`normalize.rs`)**: Path allow/deny filtering and flattening of object paths (e.g., `navigation.position.{latitude,longitude}`, `navigation.attitude.{roll,pitch,yaw}`).
3. **Classify (`classify.rs`)**: Path-to-`FieldKind` resolution (BSI with decimal scaling, Set with dictionary encoding, Count, Geo, Presence).
4. **Bucketer & Watermark (`bucket.rs`, `watermark.rs`)**: 10-second window aggregation (`mean`, `min`, `max`, `last`, `count`, `cells`), using deterministic `BTreeMap` storage for field ordering and shard sealing determinism. Out-of-order points within watermark window are accommodated; expired buckets are flushed in chronological order.
5. **Parquet Backfill (`parquet.rs`)**: Batch reader (`read_parquet_points`, `backfill_directory`) reading raw Parquet files with schema evolution, deduplication via manifest hashes, and idempotent clear-and-rewrite ingestion into `Store`.
6. **Recorder & Replay (`recorder.rs`)**: NDJSON delta log persistence (`DeltaRecorder`) and deterministic replay (`DeltaReplay`) simulating live streaming from historical logs.

## M2 Gate Suite (`tests/m2_gate.rs`)

The M2 gate suite validates three core properties required for the M2 milestone:

### Running the Suite

Data-dependent gate tests are marked `#[ignore = "needs TI_DATA_DIR"]` to prevent silent passes during unconfigured runs.
- If `TI_DATA_DIR` is set to a path that does not exist or lacks `tier=raw`, tests fail explicitly with a panic.
- To execute data-dependent tests:
  ```bash
  TI_DATA_DIR=/workspace/lume/.lanes/data/correctness cargo test -p ti-ingest --test m2_gate -- --ignored --nocapture
  ```
- To target a specific day for the 24-hour replay:
  ```bash
  TI_REPLAY_DAY=060 TI_DATA_DIR=/workspace/lume/.lanes/data/correctness cargo test -p ti-ingest --test m2_gate test_m2_oracle_replay_24h -- --ignored --nocapture
  ```

### (a) 24 h Oracle Replay (`test_m2_oracle_replay_24h`)
- **Dataset**: 24-hour continuous dataset:
  - On the fixed correctness dataset (`day=060` / `2026-03-01`): 208,937 raw Parquet points synthesized to 234,857 stream delta points.
  - On smoke dataset (`day=041`): 69,639 raw Parquet points synthesized to 78,279 stream delta points.
- **Pipeline Execution**: Recorded to an NDJSON delta log and replayed through `DeltaReplay` + `WatermarkBucketer` into a `Store` and `RecordingSink`, emitting 460,112 `BucketRecord`s (day=060) or 153,374 `BucketRecord`s (day=041).
- **Oracle Bucketing Methodology**:
  - The reference oracle is an **independent reference computation** that does **not** use `WatermarkBucketer`, `BucketWindow`, or any internal pipeline state.
  - It reads raw points directly, groups them by `(BucketIx, Path)` in a separate collection, and independently computes:
    - BSI aggregations: arithmetic mean (`sum / count`), minimum, maximum, and `last` sample by timestamp.
    - Fixed-point scaling via `ti_contracts::to_fixed`.
    - Set dictionary values via catalog set registration for string/bool values and `{path}$source` tracking.
    - Count and Geo field representations.
  - **Results**:
    - **Correctness dataset (`day=060`)**: **460,112 out of 460,112** emitted records match the independent oracle exactly (0 mismatches, 100% parity across BSI, Set, Count, and Geo).
    - **Smoke dataset (`day=041`)**: **153,374 out of 153,374** emitted records match the independent oracle exactly (0 mismatches).
- **DuckDB SQL Cross-Check**:
  - DuckDB 1.5.6 was executed over `raw_view.sql` for the identical 24-hour window:
    ```sql
    SELECT path, epoch(ts)::BIGINT // 10 AS bucket_ix,
           round(avg(value), 3) AS mean_val, round(min(value), 3) AS min_val,
           round(max(value), 3) AS max_val, count(value) AS cnt
    FROM read_raw('/workspace/lume/.lanes/data/correctness')
    WHERE context = 'vessels.urn:mrn:imo:mmsi:367000000'
      AND ts >= TIMESTAMP '2026-03-01 00:00:00' AND ts < TIMESTAMP '2026-03-02 00:00:00'
      AND value IS NOT NULL
    GROUP BY 1, 2;
    ```
  - DuckDB produced 190,703 `(path, bucket)` aggregate groups.
  - Every scalar path group matches the Rust oracle's independently computed mean, min, and max within scale precision, confirming mathematical equivalence between DuckDB SQL and the ingestion pipeline bucketer.

### (b) Parquet Backfill Idempotence (`test_m2_parquet_backfill_idempotence`)
- Evaluates Parquet files ingested into two separate fresh `Store` instances.
- **Dynamic Shard Discovery**: Inspects `shards/` to seal all open shards regardless of dataset time window (supporting day 060 shard 2704, day 041 shard 294, etc.).
- **Fresh Store Parity**: Both stores produce identical sealed shard BLAKE3 hashes.
- **Hash-based Skip**: Passing existing file hashes skips all previously ingested files.
- **In-place Clear-and-Rewrite**: Re-ingesting over an existing store reproduces identical sealed shard entries.

### (c) Throughput & RSS (`test_m2_throughput_and_rss`)
- 100,000 stream data points decoded, normalized, classified, bucketed, and applied to `Store`.
- **Throughput**: ~75,500 values/sec in debug / ~181,800 values/sec in release (exceeds M2 gate requirement of >= 20,000 values/sec).
- **Memory Footprint**: Peak RSS of ~72 MB on Linux (well under the M2 gate limit of <= 400 MB). On non-Linux platforms, reports `n/a (unsupported OS)`.
