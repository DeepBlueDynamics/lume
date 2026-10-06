//! Parquet raw-tier backfill reader.
//!
//! Enforces:
//! - Streaming read of `signalk-parquet` raw tier files via arrow-rs `parquet`
//! - Mapping both raw-tier layout (`received_timestamp`/`signalk_timestamp`, `path`, `value`, `value_*`, `source_label`)
//!   and normalized DuckDB view layout (`context, ts, path, value, value_str, source`)
//! - Content hash calculation via pure BLAKE3 and manifest skip checking
//! - Clear-and-rewrite idempotence for historical buckets

use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::Path;

use arrow_array::{
    Array, BooleanArray, Float32Array, Float64Array, Int32Array, Int64Array, RecordBatch,
    StringArray, TimestampMicrosecondArray, TimestampMillisecondArray, TimestampNanosecondArray,
    TimestampSecondArray,
};
use arrow_schema::DataType;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use ti_contracts::{bucket_of, Catalog, Error, Result, ShardSink, TiConfig, VesselOrd, VesselSpec};

use crate::bucket::BucketWindow;
use crate::classify::Classifier;
use crate::decode::RawDataPoint;
use crate::normalize::{normalize_point, NormalizedValue};

#[derive(Debug, Clone, PartialEq)]
pub enum BackfillStatus {
    Skipped {
        hash: [u8; 32],
    },
    Ingested {
        hash: [u8; 32],
        rows_read: usize,
        buckets_emitted: usize,
    },
}

/// Compute raw 32-byte BLAKE3 content hash of a file.
pub fn compute_file_hash(path: &Path) -> Result<[u8; 32]> {
    let mut file = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(*hasher.finalize().as_bytes())
}

/// Format 32-byte hash as lowercase hex string.
pub fn hash_to_hex(hash: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in hash {
        use std::fmt::Write;
        let _ = write!(s, "{:02x}", b);
    }
    s
}

fn extract_string(col: &dyn Array, i: usize) -> Option<&str> {
    if col.is_null(i) {
        return None;
    }
    col.as_any()
        .downcast_ref::<StringArray>()
        .map(|arr| arr.value(i))
}

fn extract_timestamp(col: &dyn Array, i: usize) -> Option<i64> {
    if col.is_null(i) {
        return None;
    }
    match col.data_type() {
        DataType::Utf8 => {
            let s = extract_string(col, i)?;
            chrono::DateTime::parse_from_rfc3339(s)
                .map(|dt| dt.timestamp())
                .ok()
        }
        DataType::Timestamp(arrow_schema::TimeUnit::Second, _) => {
            let arr = col.as_any().downcast_ref::<TimestampSecondArray>()?;
            Some(arr.value(i))
        }
        DataType::Timestamp(arrow_schema::TimeUnit::Millisecond, _) => {
            let arr = col.as_any().downcast_ref::<TimestampMillisecondArray>()?;
            Some(arr.value(i) / 1000)
        }
        DataType::Timestamp(arrow_schema::TimeUnit::Microsecond, _) => {
            let arr = col.as_any().downcast_ref::<TimestampMicrosecondArray>()?;
            Some(arr.value(i) / 1_000_000)
        }
        DataType::Timestamp(arrow_schema::TimeUnit::Nanosecond, _) => {
            let arr = col.as_any().downcast_ref::<TimestampNanosecondArray>()?;
            Some(arr.value(i) / 1_000_000_000)
        }
        DataType::Int64 => {
            let arr = col.as_any().downcast_ref::<Int64Array>()?;
            Some(arr.value(i))
        }
        _ => None,
    }
}

fn extract_scalar_value(col: &dyn Array, i: usize) -> Option<serde_json::Value> {
    if col.is_null(i) {
        return None;
    }
    match col.data_type() {
        DataType::Float64 => {
            let arr = col.as_any().downcast_ref::<Float64Array>()?;
            serde_json::Number::from_f64(arr.value(i)).map(serde_json::Value::Number)
        }
        DataType::Float32 => {
            let arr = col.as_any().downcast_ref::<Float32Array>()?;
            serde_json::Number::from_f64(arr.value(i) as f64).map(serde_json::Value::Number)
        }
        DataType::Int64 => {
            let arr = col.as_any().downcast_ref::<Int64Array>()?;
            Some(serde_json::Value::Number(arr.value(i).into()))
        }
        DataType::Int32 => {
            let arr = col.as_any().downcast_ref::<Int32Array>()?;
            Some(serde_json::Value::Number(arr.value(i).into()))
        }
        DataType::Boolean => {
            let arr = col.as_any().downcast_ref::<BooleanArray>()?;
            Some(serde_json::Value::Bool(arr.value(i)))
        }
        DataType::Utf8 => {
            let s = extract_string(col, i)?;
            Some(serde_json::Value::String(s.to_string()))
        }
        _ => None,
    }
}

/// Read all raw data points from a Parquet file.
pub fn read_parquet_points(path: &Path, self_urn: &str) -> Result<Vec<RawDataPoint>> {
    let file = File::open(path)?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)
        .map_err(|e| Error::Corrupt(format!("parquet open error: {e}")))?;
    let reader = builder
        .build()
        .map_err(|e| Error::Corrupt(format!("parquet reader error: {e}")))?;

    let mut points = Vec::new();

    for batch_res in reader {
        let batch: RecordBatch =
            batch_res.map_err(|e| Error::Corrupt(format!("parquet batch error: {e}")))?;
        let num_rows = batch.num_rows();

        let ctx_col = batch.column_by_name("context");
        let path_col = batch.column_by_name("path");

        // Timestamp column: signalk_timestamp, then received_timestamp, then ts
        let ts_col = batch
            .column_by_name("signalk_timestamp")
            .or_else(|| batch.column_by_name("received_timestamp"))
            .or_else(|| batch.column_by_name("ts"));

        // Source column: source_label, then source
        let src_col = batch
            .column_by_name("source_label")
            .or_else(|| batch.column_by_name("source"));

        // Scalar value column
        let val_col = batch
            .column_by_name("value")
            .or_else(|| batch.column_by_name("value_str"));

        // Value JSON column
        let json_col = batch.column_by_name("value_json");

        // Collect all value_<key> columns for flattened objects
        let mut object_cols = Vec::new();
        for (idx, field) in batch.schema().fields().iter().enumerate() {
            let name = field.name();
            if name.starts_with("value_") && name != "value_str" && name != "value_json" {
                let key = &name[6..];
                object_cols.push((key.to_string(), batch.column(idx)));
            }
        }

        for row in 0..num_rows {
            let context = ctx_col
                .and_then(|c| extract_string(c.as_ref(), row))
                .unwrap_or(self_urn);
            let context = if context.is_empty() || context == "vessels.self" {
                self_urn
            } else {
                context
            };

            let path_str = match path_col.and_then(|c| extract_string(c.as_ref(), row)) {
                Some(p) => p,
                None => continue,
            };

            let timestamp = match ts_col.and_then(|c| extract_timestamp(c.as_ref(), row)) {
                Some(t) => t,
                None => continue,
            };

            let source = src_col
                .and_then(|c| extract_string(c.as_ref(), row))
                .unwrap_or("default");

            // Extract value
            let mut value = serde_json::Value::Null;
            if let Some(col) = val_col {
                if let Some(v) = extract_scalar_value(col.as_ref(), row) {
                    value = v;
                }
            }

            if value.is_null() && !object_cols.is_empty() {
                let mut map = serde_json::Map::new();
                for (key, col) in &object_cols {
                    if let Some(v) = extract_scalar_value(col.as_ref(), row) {
                        map.insert(key.clone(), v);
                    }
                }
                if !map.is_empty() {
                    value = serde_json::Value::Object(map);
                }
            }

            if value.is_null() {
                if let Some(col) = json_col {
                    if let Some(s) = extract_string(col.as_ref(), row) {
                        if let Ok(parsed) = serde_json::from_str(s) {
                            value = parsed;
                        }
                    }
                }
            }

            points.push(RawDataPoint {
                context: context.to_string(),
                path: path_str.to_string(),
                source: source.to_string(),
                timestamp,
                value,
            });
        }
    }

    Ok(points)
}

/// Backfill a single Parquet file through normalization and bucketing, emitting
/// idempotent `rewrite: true` records to the sink.
pub fn backfill_parquet_file(
    path: &Path,
    self_urn: &str,
    manifest_hashes: Option<&HashSet<[u8; 32]>>,
    config: &TiConfig,
    catalog: &dyn Catalog,
    sink: &mut dyn ShardSink,
) -> Result<BackfillStatus> {
    let hash = compute_file_hash(path)?;

    if let Some(known) = manifest_hashes {
        if known.contains(&hash) {
            return Ok(BackfillStatus::Skipped { hash });
        }
    }

    let raw_points = read_parquet_points(path, self_urn)?;
    let rows_read = raw_points.len();

    let mut classifier = Classifier::new(config);
    let mut windows: BTreeMap<(VesselOrd, u32), BucketWindow> = BTreeMap::new();

    for raw in raw_points {
        let norm_points = normalize_point(raw, &config.allow_paths, &config.deny_paths);
        for p in norm_points {
            let canonical_urn = if p.context.starts_with("vessels.urn:") {
                p.context.clone()
            } else if p.context.starts_with("urn:") {
                format!("vessels.{}", p.context)
            } else {
                p.context.clone()
            };

            let vessel = catalog.register_vessel(&VesselSpec {
                urn: canonical_urn,
                name: None,
                mmsi: None,
            })?;
            let bucket_ix = bucket_of(p.timestamp, config.width_seconds)?;

            let (eff_path, kind) = match classifier.classify(&p.context, &p.path, &p.value) {
                Some(res) => res,
                None => continue,
            };

            let window = windows.entry((vessel, bucket_ix)).or_default();
            match p.value {
                NormalizedValue::Double(d) => {
                    let scale = match kind {
                        ti_contracts::FieldKind::Bsi { scale } => scale,
                        _ => 3,
                    };
                    window.add_numeric(&eff_path, d, scale, &p.source, p.timestamp);
                }
                NormalizedValue::String(s) => {
                    let prio = config
                        .source_priorities
                        .get(&eff_path)
                        .and_then(|l| l.iter().position(|src| src == &p.source))
                        .unwrap_or(0);
                    window.add_set(&eff_path, &s, prio, &p.source, p.timestamp);
                }
                NormalizedValue::Bool(b) => {
                    let s = if b { "true" } else { "false" };
                    let prio = config
                        .source_priorities
                        .get(&eff_path)
                        .and_then(|l| l.iter().position(|src| src == &p.source))
                        .unwrap_or(0);
                    window.add_set(&eff_path, s, prio, &p.source, p.timestamp);
                }
                NormalizedValue::Geo { lat, lon } => {
                    for cell in ti_geo::cells_for(lat, lon)? {
                        window.add_geo_cell(&eff_path, cell, &p.source);
                    }
                }
                NormalizedValue::Null => {}
            }
        }
    }

    let mut buckets_emitted = 0;
    // Emit all buckets with rewrite: true for clear-and-rewrite backfill idempotence
    for ((vessel, bucket_ix), window) in windows {
        let records = window.emit_records(vessel, bucket_ix, true, config, catalog)?;
        if !records.is_empty() {
            sink.apply(&records)?;
            buckets_emitted += 1;
        }
    }

    sink.flush()?;

    Ok(BackfillStatus::Ingested {
        hash,
        rows_read,
        buckets_emitted,
    })
}

/// Recursively backfill all `.parquet` files found under `dir`.
/// Files are sorted by path for deterministic processing order.
pub fn backfill_directory(
    dir: &Path,
    self_urn: &str,
    manifest_hashes: Option<&HashSet<[u8; 32]>>,
    config: &TiConfig,
    catalog: &dyn Catalog,
    sink: &mut dyn ShardSink,
) -> Result<Vec<BackfillStatus>> {
    let mut files = Vec::new();
    find_parquet_files_recursive(dir, &mut files)?;
    files.sort();

    let mut results = Vec::with_capacity(files.len());
    for file in files {
        let status =
            backfill_parquet_file(&file, self_urn, manifest_hashes, config, catalog, sink)?;
        results.push(status);
    }
    Ok(results)
}

fn find_parquet_files_recursive(dir: &Path, out: &mut Vec<std::path::PathBuf>) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            find_parquet_files_recursive(&path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("parquet") {
            out.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow_array::builder::{Float64Builder, StringBuilder};
    use arrow_schema::{Field, Schema};
    use parquet::arrow::ArrowWriter;
    use parquet::file::properties::WriterProperties;
    use std::sync::Arc;
    use tempfile::NamedTempFile;

    #[test]
    fn test_parquet_read_scalar_and_hash() {
        let file = NamedTempFile::new().unwrap();
        let schema = Arc::new(Schema::new(vec![
            Field::new("context", DataType::Utf8, false),
            Field::new("path", DataType::Utf8, false),
            Field::new("signalk_timestamp", DataType::Utf8, false),
            Field::new("source_label", DataType::Utf8, false),
            Field::new("value", DataType::Float64, false),
        ]));

        let mut ctx_builder = StringBuilder::new();
        let mut path_builder = StringBuilder::new();
        let mut ts_builder = StringBuilder::new();
        let mut src_builder = StringBuilder::new();
        let mut val_builder = Float64Builder::new();

        ctx_builder.append_value("vessels.urn:mrn:imo:mmsi:230999999");
        path_builder.append_value("navigation.speedOverGround");
        ts_builder.append_value("2026-03-03T04:00:00.000Z");
        src_builder.append_value("gps.0");
        val_builder.append_value(5.2);

        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(ctx_builder.finish()),
                Arc::new(path_builder.finish()),
                Arc::new(ts_builder.finish()),
                Arc::new(src_builder.finish()),
                Arc::new(val_builder.finish()),
            ],
        )
        .unwrap();

        let props = WriterProperties::builder().build();
        let mut writer = ArrowWriter::try_new(file.reopen().unwrap(), schema, Some(props)).unwrap();
        writer.write(&batch).unwrap();
        writer.close().unwrap();

        let hash = compute_file_hash(file.path()).unwrap();
        assert_ne!(hash, [0u8; 32]);
        let hex = hash_to_hex(&hash);
        assert_eq!(hex.len(), 64);

        let points = read_parquet_points(file.path(), "vessels.self").unwrap();
        assert_eq!(points.len(), 1);
        assert_eq!(points[0].path, "navigation.speedOverGround");
        assert_eq!(points[0].source, "gps.0");
        assert_eq!(points[0].value, serde_json::json!(5.2));
        assert_eq!(points[0].timestamp, 1772510400); // 2026-03-03T04:00:00Z
    }

    #[test]
    fn test_parquet_read_object_columns() {
        let file = NamedTempFile::new().unwrap();
        let schema = Arc::new(Schema::new(vec![
            Field::new("context", DataType::Utf8, false),
            Field::new("path", DataType::Utf8, false),
            Field::new("signalk_timestamp", DataType::Utf8, false),
            Field::new("source_label", DataType::Utf8, false),
            Field::new("value_latitude", DataType::Float64, false),
            Field::new("value_longitude", DataType::Float64, false),
        ]));

        let mut ctx_builder = StringBuilder::new();
        let mut path_builder = StringBuilder::new();
        let mut ts_builder = StringBuilder::new();
        let mut src_builder = StringBuilder::new();
        let mut lat_builder = Float64Builder::new();
        let mut lon_builder = Float64Builder::new();

        ctx_builder.append_value("vessels.self");
        path_builder.append_value("navigation.position");
        ts_builder.append_value("2026-03-03T04:00:00.000Z");
        src_builder.append_value("gps.1");
        lat_builder.append_value(60.123);
        lon_builder.append_value(24.456);

        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(ctx_builder.finish()),
                Arc::new(path_builder.finish()),
                Arc::new(ts_builder.finish()),
                Arc::new(src_builder.finish()),
                Arc::new(lat_builder.finish()),
                Arc::new(lon_builder.finish()),
            ],
        )
        .unwrap();

        let mut writer = ArrowWriter::try_new(file.reopen().unwrap(), schema, None).unwrap();
        writer.write(&batch).unwrap();
        writer.close().unwrap();

        let points = read_parquet_points(file.path(), "urn:mrn:imo:mmsi:123456789").unwrap();
        assert_eq!(points.len(), 1);
        assert_eq!(points[0].context, "urn:mrn:imo:mmsi:123456789");
        assert_eq!(points[0].path, "navigation.position");
        assert_eq!(
            points[0].value,
            serde_json::json!({
                "latitude": 60.123,
                "longitude": 24.456
            })
        );
    }
}
