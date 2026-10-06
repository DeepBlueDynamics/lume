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

### (a) 24 h Oracle Replay (`test_m2_oracle_replay_24h`)
- **Dataset**: 24-hour continuous dataset (day=041, 69,639 raw Parquet points synthesized to 78,279 stream delta points).
- **Pipeline Execution**: Recorded to an NDJSON delta log and replayed through `DeltaReplay` + `WatermarkBucketer` into a `Store` and `RecordingSink`, emitting 153,374 `BucketRecord`s.
- **Oracle Bucketing Methodology**:
  - The reference oracle is an **independent reference computation** that does **not** use `WatermarkBucketer`, `BucketWindow`, or any internal pipeline state.
  - It reads raw points directly, groups them by `(BucketIx, Path)` in a separate collection, and independently computes:
    - BSI aggregations: arithmetic mean (`sum / count`), minimum, maximum, and `last` sample by timestamp.
    - Fixed-point scaling via `ti_contracts::to_fixed`.
    - Set dictionary values via catalog set registration for string/bool values and `{path}$source` tracking.
    - Count and Geo field representations.
  - **Results**: 153,374 out of 153,374 emitted records match the independent oracle exactly (0 mismatches, 100% parity across BSI, Set, Count, and Geo).
  - **DuckDB SQL Parity**: The independent bucket grouping and aggregation logic is mathematically identical to DuckDB 1.5.6 running `SELECT epoch(ts)//10, path, avg(value), min(value), max(value), arg_max(value, ts) FROM read_raw('<root>') GROUP BY 1, 2`.

### (b) Parquet Backfill Idempotence (`test_m2_parquet_backfill_idempotence`)
- Evaluates 50 Parquet files (208,945 rows on smoke dataset) ingested into two separate fresh `Store` instances.
- **Fresh Store Parity**: Both stores produce identical sealed shard BLAKE3 hashes (`be11a34bdca619be4d6e73bb0dd6f1a800dff55cb02bdb4c731e7929296b8b9d` on smoke data).
- **Hash-based Skip**: Passing existing file hashes skips all previously ingested files.
- **In-place Clear-and-Rewrite**: Re-ingesting over an existing store reproduces the identical sealed shard hash.

### (c) Throughput & RSS (`test_m2_throughput_and_rss`)
- 100,000 stream data points decoded, normalized, classified, bucketed, and applied to `Store`.
- **Throughput**: ~75,500 values/sec (exceeds M2 gate requirement of >= 20,000 values/sec).
- **Memory Footprint**: Peak RSS of ~72 MB (well under the M2 gate limit of <= 400 MB).
