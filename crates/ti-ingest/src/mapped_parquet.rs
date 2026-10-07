//! Streaming, projected generic Parquet reader. Explicit identity/time, no guesses.
use crate::RawDataPoint;
use arrow_array::{types::*, *};
use arrow_schema::{DataType, TimeUnit as ArrowTimeUnit};
use parquet::arrow::{arrow_reader::ParquetRecordBatchReaderBuilder, ProjectionMask};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    path::{Path, PathBuf},
};
use ti_contracts::{EntityMapping, Error, ParquetFormat, ParquetMapping, Result, TimeUnit};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MappingReport {
    pub files: usize,
    pub peak_active_windows: usize,
    pub peak_active_bytes: u64,
    pub journal_bytes: u64,
    pub late_bucket_reloads: usize,
    pub rows_read: usize,
    pub points_read: usize,
    pub null_entity: usize,
    pub null_time: usize,
    pub null_metric: usize,
    pub null_value: usize,
    pub unsupported_columns: BTreeSet<String>,
    pub classification_misses: BTreeSet<String>,
    pub unit_scale_misses: BTreeSet<String>,
}
fn invalid(message: impl Into<String>) -> Error {
    Error::InvalidInput(message.into())
}

/// Utf8 variants and dictionary values, retaining categorical dictionary semantics.
pub fn scalar(column: &dyn Array, row: usize) -> Result<Option<serde_json::Value>> {
    if column.is_null(row) {
        return Ok(None);
    }
    macro_rules! number {
        ($($ty:ty),+) => { $(
            if let Some(a) = column.as_any().downcast_ref::<$ty>() {
                return Ok(Some(serde_json::json!(a.value(row))));
            }
        )+ };
    }
    number!(
        Int8Array,
        Int16Array,
        Int32Array,
        Int64Array,
        UInt8Array,
        UInt16Array,
        UInt32Array,
        UInt64Array,
        Float32Array,
        Float64Array,
        BooleanArray
    );
    macro_rules! decimal {
        ($($ty:ty),+) => { $(
            if let Some(a) = column.as_any().downcast_ref::<$ty>() {
                let value = a.value(row).to_string().parse::<f64>()
                    .map_err(|_| invalid("decimal value exceeds numeric range"))? / 10f64.powi(i32::from(a.scale()));
                return Ok(serde_json::Number::from_f64(value).map(serde_json::Value::Number));
            }
        )+ };
    }
    decimal!(
        Decimal32Array,
        Decimal64Array,
        Decimal128Array,
        Decimal256Array
    );
    if let Some(a) = column.as_any().downcast_ref::<Float16Array>() {
        return Ok(
            serde_json::Number::from_f64(a.value(row).to_f64()).map(serde_json::Value::Number)
        );
    }
    if let Some(value) = [
        column
            .as_any()
            .downcast_ref::<StringArray>()
            .map(|a| a.value(row)),
        column
            .as_any()
            .downcast_ref::<LargeStringArray>()
            .map(|a| a.value(row)),
        column
            .as_any()
            .downcast_ref::<StringViewArray>()
            .map(|a| a.value(row)),
    ]
    .into_iter()
    .flatten()
    .next()
    {
        return Ok(Some(value.into()));
    }
    macro_rules! dictionary {
        ($($ty:ty),+) => { $(
            if let Some(a) = column.as_any().downcast_ref::<DictionaryArray<$ty>>() {
                let Some(key) = a.key(row) else { return Ok(None) };
                return scalar(a.values().as_ref(), key).map(|v| v.map(|v| match v {
                    serde_json::Value::String(_) => v, _ => serde_json::Value::String(v.to_string())
                }));
            }
        )+ };
    }
    dictionary!(
        Int8Type, Int16Type, Int32Type, Int64Type, UInt8Type, UInt16Type, UInt32Type, UInt64Type
    );
    Err(Error::Unsupported(format!(
        "mapped parquet scalar type {:?}",
        column.data_type()
    )))
}
fn text(column: &dyn Array, row: usize) -> Result<Option<String>> {
    match scalar(column, row)? {
        Some(serde_json::Value::String(s)) => Ok(Some(s)),
        None => Ok(None),
        _ => Err(invalid(
            "mapped entity, metric and source columns must contain strings",
        )),
    }
}
fn offset(timezone: Option<&str>) -> Result<chrono::FixedOffset> {
    let zone = timezone.ok_or_else(|| invalid("naive time requires an explicit timezone"))?;
    let seconds = if zone == "UTC" || zone == "Z" {
        0
    } else {
        let hours: i32 = zone
            .get(1..3)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| invalid("invalid timezone"))?;
        let minutes: i32 = zone
            .get(4..6)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| invalid("invalid timezone"))?;
        (hours * 3600 + minutes * 60) * if zone.starts_with('-') { -1 } else { 1 }
    };
    chrono::FixedOffset::east_opt(seconds).ok_or_else(|| invalid("timezone out of range"))
}
/// Whole UTC seconds. Subsecond values floor instead of truncating negative epochs.
pub fn time_seconds(
    column: &dyn Array,
    row: usize,
    unit: TimeUnit,
    timezone: Option<&str>,
) -> Result<Option<i64>> {
    if column.is_null(row) {
        return Ok(None);
    }
    let mut naive = false;
    let seconds = match column.data_type() {
        DataType::Timestamp(ArrowTimeUnit::Second, zone) => {
            naive = zone.is_none();
            column
                .as_any()
                .downcast_ref::<TimestampSecondArray>()
                .unwrap()
                .value(row)
        }
        DataType::Timestamp(ArrowTimeUnit::Millisecond, zone) => {
            naive = zone.is_none();
            column
                .as_any()
                .downcast_ref::<TimestampMillisecondArray>()
                .unwrap()
                .value(row)
                .div_euclid(1000)
        }
        DataType::Timestamp(ArrowTimeUnit::Microsecond, zone) => {
            naive = zone.is_none();
            column
                .as_any()
                .downcast_ref::<TimestampMicrosecondArray>()
                .unwrap()
                .value(row)
                .div_euclid(1_000_000)
        }
        DataType::Timestamp(ArrowTimeUnit::Nanosecond, zone) => {
            naive = zone.is_none();
            column
                .as_any()
                .downcast_ref::<TimestampNanosecondArray>()
                .unwrap()
                .value(row)
                .div_euclid(1_000_000_000)
        }
        DataType::Date32 => {
            naive = true;
            i64::from(
                column
                    .as_any()
                    .downcast_ref::<Date32Array>()
                    .unwrap()
                    .value(row),
            ) * 86400
        }
        DataType::Date64 => {
            naive = true;
            column
                .as_any()
                .downcast_ref::<Date64Array>()
                .unwrap()
                .value(row)
                .div_euclid(1000)
        }
        DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View | DataType::Dictionary(_, _) => {
            use chrono::TimeZone;
            let Some(value) = text(column, row)? else {
                return Ok(None);
            };
            if let Ok(time) = chrono::DateTime::parse_from_rfc3339(&value) {
                return Ok(Some(time.timestamp()));
            }
            let date = chrono::NaiveDateTime::parse_from_str(&value, "%Y-%m-%d %H:%M:%S%.f")
                .or_else(|_| chrono::NaiveDateTime::parse_from_str(&value, "%Y-%m-%dT%H:%M:%S%.f"))
                .or_else(|_| {
                    chrono::NaiveDate::parse_from_str(&value, "%Y-%m-%d")
                        .map(|d| d.and_hms_opt(0, 0, 0).unwrap())
                })
                .map_err(|e| invalid(format!("invalid mapped time {value:?}: {e}")))?;
            return Ok(Some(
                offset(timezone)?
                    .from_local_datetime(&date)
                    .single()
                    .ok_or_else(|| invalid("ambiguous mapped time"))?
                    .timestamp(),
            ));
        }
        _ => {
            let value = scalar(column, row)?
                .and_then(|v| v.as_i64())
                .ok_or_else(|| invalid("time requires timestamp, date or integer epoch"))?;
            let factor = match unit {
                TimeUnit::Seconds => 1,
                TimeUnit::Milliseconds => 1000,
                TimeUnit::Microseconds => 1_000_000,
                TimeUnit::Nanoseconds => 1_000_000_000,
                TimeUnit::Rfc3339 => {
                    return Err(invalid("rfc3339 unit requires a text time column"))
                }
            };
            value.div_euclid(factor)
        }
    };
    // Arrow timestamp without a zone denotes an explicit wall clock, not guessed local time.
    let seconds = if naive {
        seconds
            .checked_sub(i64::from(offset(timezone)?.local_minus_utc()))
            .ok_or(Error::Overflow("mapped timestamp"))?
    } else {
        seconds
    };
    Ok(Some(seconds))
}
fn supported(data_type: &DataType) -> bool {
    if let DataType::Dictionary(_, value) = data_type {
        return supported(value);
    }
    matches!(
        data_type,
        DataType::Int8
            | DataType::Int16
            | DataType::Int32
            | DataType::Int64
            | DataType::UInt8
            | DataType::UInt16
            | DataType::UInt32
            | DataType::UInt64
            | DataType::Float16
            | DataType::Float32
            | DataType::Float64
            | DataType::Boolean
            | DataType::Decimal32(_, _)
            | DataType::Decimal64(_, _)
            | DataType::Decimal128(_, _)
            | DataType::Decimal256(_, _)
            | DataType::Utf8
            | DataType::LargeUtf8
            | DataType::Utf8View
    )
}
/// Stream projected batches, returning a bounded vector of points for each Arrow batch.
pub fn read_file(
    path: &Path,
    mapping: &ParquetMapping,
    report: &mut MappingReport,
    mut consume: impl FnMut(Vec<RawDataPoint>) -> Result<()>,
) -> Result<()> {
    mapping.validate()?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(File::open(path)?)
        .map_err(|e| Error::Corrupt(format!("{}: {e}", path.display())))?;
    let schema = builder.schema();
    let mut names = BTreeSet::from([mapping.time.clone()]);
    if let EntityMapping::Column(name) = &mapping.entity {
        names.insert(name.clone());
    }
    if let Some(source) = &mapping.source {
        names.insert(source.clone());
    }
    let metrics: Vec<String> = match mapping.format {
        ParquetFormat::Long => {
            names.insert(mapping.metric.clone().unwrap());
            names.insert(mapping.value.clone().unwrap());
            vec![]
        }
        ParquetFormat::Wide => schema
            .fields()
            .iter()
            .filter_map(|field| {
                if names.contains(field.name()) || mapping.exclude.contains(field.name()) {
                    return None;
                }
                if supported(field.data_type()) {
                    Some(field.name().clone())
                } else {
                    report.unsupported_columns.insert(field.name().clone());
                    None
                }
            })
            .collect(),
    };
    names.extend(metrics.iter().cloned());
    let projection = names
        .iter()
        .map(|name| {
            schema
                .index_of(name)
                .map_err(|_| invalid(format!("{}: missing mapped column {name}", path.display())))
        })
        .collect::<Result<Vec<_>>>()?;
    let mask = ProjectionMask::roots(builder.parquet_schema(), projection);
    let batch_rows = (65_536 / names.len().max(1)).clamp(1, 8192);
    let reader = builder
        .with_projection(mask)
        .with_batch_size(batch_rows)
        .build()
        .map_err(|e| Error::Corrupt(e.to_string()))?;
    report.files += 1;
    for batch in reader {
        let batch = batch?;
        let column = |name: &str| {
            batch
                .column_by_name(name)
                .map(|a| a.as_ref())
                .ok_or_else(|| invalid(format!("missing projected column {name}")))
        };
        let time = column(&mapping.time)?;
        let mut points = Vec::new();
        for row in 0..batch.num_rows() {
            report.rows_read += 1;
            let entity = match &mapping.entity {
                EntityMapping::Column(name) => text(column(name)?, row)?,
                EntityMapping::Constant { constant } => Some(constant.clone()),
            };
            let Some(entity) = entity.filter(|s| !s.is_empty()) else {
                report.null_entity += 1;
                continue;
            };
            ti_contracts::validate_entity_urn(&entity)?;
            let Some(timestamp) =
                time_seconds(time, row, mapping.time_unit, mapping.timezone.as_deref())?
            else {
                report.null_time += 1;
                continue;
            };
            let source = mapping
                .source
                .as_deref()
                .map(|name| text(column(name)?, row))
                .transpose()?
                .flatten()
                .unwrap_or_else(|| "default".into());
            let values = match mapping.format {
                ParquetFormat::Long => {
                    let Some(metric) = text(column(mapping.metric.as_ref().unwrap())?, row)?
                        .filter(|s| !s.is_empty())
                    else {
                        report.null_metric += 1;
                        continue;
                    };
                    vec![(metric, column(mapping.value.as_ref().unwrap())?)]
                }
                ParquetFormat::Wide => metrics
                    .iter()
                    .map(|name| Ok((name.clone(), column(name)?)))
                    .collect::<Result<Vec<_>>>()?,
            };
            for (metric, array) in values {
                let Some(value) = scalar(array, row)?.filter(|v| !v.is_null()) else {
                    report.null_value += 1;
                    continue;
                };
                points.push(RawDataPoint {
                    context: entity.clone(),
                    path: format!("{}{metric}", mapping.prefix),
                    source: source.clone(),
                    timestamp,
                    value,
                });
                report.points_read += 1;
                if points.len() == 8192 {
                    consume(std::mem::take(&mut points))?;
                }
            }
        }
        consume(points)?;
    }
    Ok(())
}

/// Deterministic glob expansion without a new runtime dependency. Supports *, ?, **.
pub fn expand_files(pattern: &str) -> Result<Vec<PathBuf>> {
    fn matches(pattern: &[u8], value: &[u8]) -> bool {
        if pattern.is_empty() {
            return value.is_empty();
        }
        match pattern[0] {
            b'*' => {
                matches(&pattern[1..], value)
                    || (!value.is_empty() && matches(pattern, &value[1..]))
            }
            b'?' => !value.is_empty() && matches(&pattern[1..], &value[1..]),
            ch => !value.is_empty() && ch == value[0] && matches(&pattern[1..], &value[1..]),
        }
    }
    fn walk(base: &Path, components: &[String], out: &mut BTreeSet<PathBuf>) -> Result<()> {
        if components.is_empty() {
            if base.is_file() {
                out.insert(base.to_path_buf());
            }
            return Ok(());
        }
        if components[0] == "**" {
            walk(base, &components[1..], out)?;
            if base.is_dir() {
                for entry in std::fs::read_dir(base)? {
                    let path = entry?.path();
                    if path.is_dir() && !path.is_symlink() {
                        walk(&path, components, out)?;
                    }
                }
            }
        } else if components[0].contains(['*', '?']) {
            if base.is_dir() {
                for entry in std::fs::read_dir(base)? {
                    let entry = entry?;
                    if matches(
                        components[0].as_bytes(),
                        entry.file_name().to_string_lossy().as_bytes(),
                    ) {
                        walk(&entry.path(), &components[1..], out)?;
                    }
                }
            }
        } else {
            walk(&base.join(&components[0]), &components[1..], out)?;
        }
        Ok(())
    }
    let path = Path::new(pattern);
    let mut base = PathBuf::new();
    let mut parts = Vec::new();
    let mut wildcard = false;
    for part in path.components() {
        let value = part.as_os_str().to_string_lossy().into_owned();
        wildcard |= value.contains(['*', '?']);
        if wildcard {
            parts.push(value);
        } else {
            base.push(part);
        }
    }
    if base.as_os_str().is_empty() {
        base.push(".");
    }
    let mut files = BTreeSet::new();
    walk(&base, &parts, &mut files)?;
    if files.is_empty() {
        return Err(invalid(format!("no files match {pattern:?}")));
    }
    Ok(files.into_iter().collect())
}

/// Bounded historical windows, closed by each entity's observed event-time watermark.
/// Closed accumulator state lives in a capped per-run journal for exact late rewrites.
pub fn backfill(
    mappings: &[ParquetMapping],
    config: &ti_contracts::TiConfig,
    catalogs: &BTreeMap<String, &dyn ti_contracts::Catalog>,
    sinks: &mut BTreeMap<String, &mut dyn ti_contracts::ShardSink>,
) -> Result<MappingReport> {
    use crate::window_journal::{WindowJournal, WindowKey};
    use crate::{BucketWindow, Classifier, NormalizedValue};
    use ti_contracts::{bucket_of, VesselSpec};
    fn window_bytes(window: &BucketWindow) -> u64 {
        // Conservative allocation estimate: map nodes, String capacity, sources and cells.
        let sources =
            |set: &BTreeSet<String>| set.iter().map(|s| 128 + s.capacity() as u64).sum::<u64>();
        256 + window
            .numeric
            .iter()
            .map(|(p, a)| 384 + p.capacity() as u64 + sources(&a.sources))
            .sum::<u64>()
            + window
                .set
                .iter()
                .map(|(p, a)| {
                    320 + p.capacity() as u64
                        + a.winning_value.capacity() as u64
                        + sources(&a.sources)
                })
                .sum::<u64>()
            + window
                .count
                .iter()
                .map(|(p, a)| 256 + p.capacity() as u64 + sources(&a.sources))
                .sum::<u64>()
            + window
                .geo
                .iter()
                .map(|(p, a)| {
                    256 + p.capacity() as u64 + a.cells.len() as u64 * 128 + sources(&a.sources)
                })
                .sum::<u64>()
    }
    fn emit(
        key: &WindowKey,
        window: &BucketWindow,
        config: &ti_contracts::TiConfig,
        resolved: &BTreeMap<String, ti_contracts::StoreConfig>,
        catalogs: &BTreeMap<String, &dyn ti_contracts::Catalog>,
        sinks: &mut BTreeMap<String, &mut dyn ti_contracts::ShardSink>,
        pending: &mut BTreeMap<String, Vec<ti_contracts::BucketRecord>>,
    ) -> Result<()> {
        let records = window.emit_records_with_aggs(
            key.1,
            key.2,
            true,
            Some(&resolved[&key.0].aggs),
            config,
            catalogs[&key.0],
        )?;
        if records.len() > 50_000 {
            return Err(invalid(
                "mapped bucket exceeds 50k record transaction limit",
            ));
        }
        let batch = pending.entry(key.0.clone()).or_default();
        if batch.len() + records.len() > 50_000
            || batch.iter().any(|r| r.vessel == key.1 && r.bucket == key.2)
        {
            sinks.get_mut(&key.0).unwrap().apply(batch)?;
            batch.clear();
        }
        batch.extend(records);
        Ok(())
    }
    let resolved = config.resolved_stores();
    let limits = &config.sources.backfill;
    let mut journal = WindowJournal::new(&config.store_root, limits)?;
    let mut classifiers: BTreeMap<_, _> = resolved
        .keys()
        .map(|n| (n.clone(), Classifier::new(config)))
        .collect();
    let mut windows = BTreeMap::<WindowKey, BucketWindow>::new();
    let mut sizes = BTreeMap::<WindowKey, u64>::new();
    let mut active_bytes = 0u64;
    let mut highwater = BTreeMap::<(String, u32), i64>::new();
    let mut pending = BTreeMap::<String, Vec<ti_contracts::BucketRecord>>::new();
    let mut report = MappingReport::default();
    for mapping in mappings {
        for file in expand_files(&mapping.files)? {
            let mut file_report = MappingReport::default();
            read_file(&file, mapping, &mut file_report, |points| {
                for raw in points {
                    for point in
                        crate::normalize_point(raw, &config.allow_paths, &config.deny_paths)
                    {
                        if matches!(point.value, NormalizedValue::Double(_))
                            && ti_contracts::metric_unit(&config.units, &point.path).is_none()
                        {
                            report.unit_scale_misses.insert(point.path.clone());
                        }
                        for (name, settings) in &resolved {
                            let Some(catalog) = catalogs.get(name) else {
                                continue;
                            };
                            if !sinks.contains_key(name) {
                                continue;
                            }
                            if settings.paths.as_ref().is_some_and(|paths| {
                                !crate::normalize::is_path_allowed(
                                    &point.path,
                                    paths,
                                    &config.deny_paths,
                                )
                            }) {
                                continue;
                            }
                            let vessel = catalog.register_vessel(&VesselSpec {
                                urn: point.context.clone(),
                                name: None,
                                mmsi: None,
                            })?;
                            let width = settings.width_seconds()?;
                            let bucket = bucket_of(point.timestamp, width)?;
                            let key = (name.clone(), vessel, bucket);
                            let max_seen = highwater
                                .entry((name.clone(), vessel))
                                .or_insert(point.timestamp);
                            *max_seen = (*max_seen).max(point.timestamp);
                            let Some((path, kind)) = classifiers.get_mut(name).unwrap().classify(
                                &point.context,
                                &point.path,
                                &point.value,
                            ) else {
                                if config.ingest.count_paths.contains(&point.path) {
                                    continue;
                                }
                                return Err(invalid("unclassifiable mapped value"));
                            };
                            if path != point.path {
                                report
                                    .classification_misses
                                    .insert(format!("{}: type changed to {path}", point.path));
                            }
                            if let std::collections::btree_map::Entry::Vacant(entry) =
                                windows.entry(key.clone())
                            {
                                let restored = journal.load(&key)?;
                                if !restored.is_empty() {
                                    report.late_bucket_reloads += 1;
                                }
                                entry.insert(restored);
                            }
                            let window = windows.get_mut(&key).unwrap();
                            crate::watermark::populate_window(
                                window,
                                &path,
                                &point.value,
                                &point.source,
                                point.timestamp,
                                &kind,
                                config,
                            )?;
                            let size = window_bytes(window);
                            active_bytes =
                                active_bytes - sizes.insert(key, size).unwrap_or(0) + size;
                            report.peak_active_windows =
                                report.peak_active_windows.max(windows.len());
                            report.peak_active_bytes = report.peak_active_bytes.max(active_bytes);
                            if active_bytes > limits.max_active_bytes {
                                return Err(invalid("Parquet backfill active-window memory cap reached; lower sources.backfill.lateness_seconds, sort input by entity/time, or import a smaller range"));
                            }
                        }
                    }
                }
                let closed: Vec<_> = windows
                    .keys()
                    .filter(|key| {
                        let width = resolved[&key.0].width_seconds().expect("validated width");
                        let end = i128::from(ti_contracts::EPOCH)
                            + (i128::from(key.2) + 1) * i128::from(width);
                        end <= i128::from(highwater[&(key.0.clone(), key.1)])
                            - i128::from(limits.lateness_seconds)
                    })
                    .cloned()
                    .collect();
                for key in closed {
                    let window = windows.remove(&key).unwrap();
                    active_bytes -= sizes.remove(&key).unwrap_or(0);
                    journal.save(key.clone(), &window)?;
                    emit(
                        &key,
                        &window,
                        config,
                        &resolved,
                        catalogs,
                        sinks,
                        &mut pending,
                    )?;
                }
                Ok(())
            })?;
            report.files += file_report.files;
            report.rows_read += file_report.rows_read;
            report.points_read += file_report.points_read;
            report.null_entity += file_report.null_entity;
            report.null_time += file_report.null_time;
            report.null_metric += file_report.null_metric;
            report.null_value += file_report.null_value;
            report
                .classification_misses
                .extend(file_report.unsupported_columns.iter().cloned());
            report
                .unsupported_columns
                .extend(file_report.unsupported_columns);
            if report.files % 256 == 0 {
                for (name, records) in &mut pending {
                    if !records.is_empty() {
                        sinks.get_mut(name).unwrap().apply(records)?;
                        records.clear();
                    }
                }
                for sink in sinks.values_mut() {
                    sink.flush()?;
                }
            }
        }
    }
    for (key, window) in windows {
        emit(
            &key,
            &window,
            config,
            &resolved,
            catalogs,
            sinks,
            &mut pending,
        )?;
    }
    for (name, records) in pending {
        if !records.is_empty() {
            sinks.get_mut(&name).unwrap().apply(&records)?;
        }
    }
    for sink in sinks.values_mut() {
        sink.flush()?;
    }
    report.journal_bytes = journal.bytes();
    Ok(report)
}
