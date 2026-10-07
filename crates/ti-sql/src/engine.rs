//! Shared in-process CLI/MCP engine for the first W7 slice.
use crate::{core_error, rows_json, DocumentsFactory, SqlSession};
use datafusion::common::{DataFusionError, Result};
use futures::StreamExt;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

pub use datafusion::arrow::ipc as arrow_ipc;
pub use tokio::runtime::Runtime as SurfaceRuntime;
type QueryBatches = (
    Value,
    Vec<datafusion::arrow::record_batch::RecordBatch>,
    datafusion::arrow::datatypes::SchemaRef,
);
pub fn surface_runtime() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Runtime::new()?)
}
pub const MAX_ROWS: usize = 500;
pub const MAX_BYTES: usize = 64 * 1024;
pub struct TiEngine {
    pub session: SqlSession,
    root: PathBuf,
    skipped_stores: Vec<String>,
}
fn invalid(message: impl Into<String>) -> DataFusionError {
    DataFusionError::Plan(message.into())
}

/// Read one representative field-file header per shard directory, rejecting mixed stores.
/// Empty stores have no header: callers must supply a width or ti.toml.
pub fn store_width(root: &Path, requested: Option<u64>) -> Result<u64> {
    fn visit(path: &Path, width: &mut Option<u64>) -> Result<()> {
        if !path.exists() {
            return Ok(());
        }
        let mut children = Vec::new();
        let mut representative: Option<PathBuf> = None;
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let ty = entry.file_type()?;
            if ty.is_dir() {
                children.push(entry.path());
            } else if ty.is_file() && entry.path().extension().is_some_and(|s| s == "rbm") {
                let candidate = entry.path();
                if representative
                    .as_ref()
                    .is_none_or(|current| candidate < *current)
                {
                    representative = Some(candidate);
                }
            }
        }
        // All field files in a shard version share its immutable width. Probe one
        // deterministic header per directory, while still checking every shard.
        if let Some(path) = representative {
            let mut file = std::io::BufReader::with_capacity(256, std::fs::File::open(path)?);
            let mut magic = [0; 8];
            file.read_exact(&mut magic)?;
            if magic != ti_contracts::SHARD_MAGIC {
                return Err(invalid("invalid shard magic"));
            }
            let mut version = [0; 2];
            file.read_exact(&mut version)?;
            if u16::from_le_bytes(version) != ti_contracts::FORMAT_VERSION {
                return Err(invalid("unsupported shard version"));
            }
            let mut len = [0; 4];
            file.read_exact(&mut len)?;
            file.seek_relative(i64::from(u32::from_le_bytes(len)) + 8)?;
            let mut bytes = [0; 8];
            file.read_exact(&mut bytes)?;
            let found = u64::from_le_bytes(bytes);
            if found == 0 || width.is_some_and(|w| w != found) {
                return Err(invalid(format!("store bucket width mismatch: this store\'s width is {found} s; omit width_seconds")));
            }
            *width = Some(found);
        }
        for child in children {
            visit(&child, width)?;
        }
        Ok(())
    }
    if requested == Some(0) {
        return Err(invalid("width must be positive"));
    }
    let mut width = requested;
    let config = root.join("ti.toml");
    if config.exists() {
        let config = ti_contracts::TiConfig::from_toml(&std::fs::read_to_string(config)?)
            .map_err(core_error)?;
        if width.is_some_and(|w| w != config.width_seconds) {
            return Err(invalid(format!(
                "store bucket width mismatch: this store\'s width is {} s; omit width_seconds",
                config.width_seconds
            )));
        }
        width = Some(config.width_seconds);
    }
    visit(&root.join("shards"), &mut width)?;
    width.ok_or_else(|| invalid("empty store requires --width <seconds> or <store>/ti.toml"))
}
impl TiEngine {
    pub async fn open(
        root: &Path,
        width: Option<u64>,
        documents: Option<&DocumentsFactory>,
    ) -> Result<Self> {
        let has_explicit_width = width.is_some();
        let width = store_width(root, width)?;
        let session = match (documents, has_explicit_width) {
            (Some(factory), true) => {
                crate::open_or_create_store_with_documents(root, width, factory).await?
            }
            (Some(factory), false) => {
                crate::open_store_with_documents(root, width, factory).await?
            }
            (None, true) => crate::open_or_create_store(root, width).await?,
            (None, false) => crate::open_store(root, width).await?,
        };
        let config_path = root.join("ti.toml");
        let mut skipped_stores = Vec::new();
        if config_path.exists() {
            let content = std::fs::read_to_string(&config_path)?;
            let cfg = ti_contracts::TiConfig::from_toml(&content).map_err(core_error)?;
            for (name, store_cfg) in &cfg.stores {
                if name == "default" {
                    continue;
                }
                let store_dir = if let Some(r) = &store_cfg.root {
                    PathBuf::from(r)
                } else {
                    root.join("stores").join(name)
                };
                if !store_dir.join("catalog").is_dir() {
                    skipped_stores.push(name.clone());
                    continue;
                }
                let width = store_cfg.width_seconds().map_err(|e| {
                    invalid(format!(
                        "configured store '{name}' has invalid width '{}': {e}",
                        store_cfg.width
                    ))
                })?;
                let store = ti_store::Store::open_or_create(&store_dir, width)
                    .map_err(|e| invalid(format!("failed to open store '{name}': {e}")))?;
                let table_name = crate::table_name_for_store(name);
                let catalog = crate::build_sql_catalog(&store, width).map_err(|e| {
                    invalid(format!("failed to build catalog for store '{name}': {e}"))
                })?;
                session
                    .register_store_table(&table_name, Arc::new(store), catalog)
                    .map_err(|e| {
                        invalid(format!("failed to register table for store '{name}': {e}"))
                    })?;
            }
        }
        Ok(Self {
            session,
            root: root.into(),
            skipped_stores,
        })
    }
    pub fn from_session(session: SqlSession, root: PathBuf) -> Self {
        Self {
            session,
            root,
            skipped_stores: Vec::new(),
        }
    }
    pub fn units(&self) -> BTreeMap<String, Option<String>> {
        let mut u: BTreeMap<String, Option<String>> = self
            .session
            .catalog
            .fields
            .iter()
            .map(|f| (crate::field_name(f), f.units.clone()))
            .collect();
        if let Ok(extras) = self.session.extra_catalogs.lock() {
            for cat in extras.values() {
                for f in &cat.fields {
                    u.entry(crate::field_name(f))
                        .or_insert_with(|| f.units.clone());
                }
            }
        }
        u
    }
    /// Streaming result admission: collect at most max_rows+1 rows and 64 KiB
    /// including metadata. Stop reading once either cap is reached.
    pub async fn query(&self, sql: &str, max_rows: usize) -> Result<Value> {
        Ok(self.query_batches(sql, max_rows).await?.0)
    }
    pub async fn query_arrow(&self, sql: &str, max_rows: usize) -> Result<(Vec<u8>, usize, bool)> {
        let (reply, mut batches, schema) = self.query_batches(sql, max_rows).await?;
        let mut count = reply["row_count"].as_u64().unwrap_or(0) as usize;
        let mut truncated = reply["truncated"] == true;
        loop {
            let mut body = Vec::new();
            {
                let mut writer =
                    datafusion::arrow::ipc::writer::StreamWriter::try_new(&mut body, &schema)?;
                for batch in &batches {
                    writer.write(batch)?;
                }
                writer.finish()?;
            }
            if body.len() <= MAX_BYTES {
                return Ok((body, count, truncated));
            }
            let Some(last) = batches.pop() else {
                return Err(invalid("Arrow schema exceeds 64 KiB; select fewer columns"));
            };
            if last.num_rows() > 1 {
                batches.push(last.slice(0, last.num_rows() - 1));
            }
            count -= 1;
            truncated = true;
        }
    }
    async fn query_batches(&self, sql: &str, max_rows: usize) -> Result<QueryBatches> {
        self.query_with_parameters(sql, max_rows, vec![]).await
    }
    pub async fn query_with_parameters(
        &self,
        sql: &str,
        max_rows: usize,
        parameters: Vec<datafusion::common::ScalarValue>,
    ) -> Result<QueryBatches> {
        let started = Instant::now();
        let limit = max_rows.clamp(1, MAX_ROWS);
        let first = self.session.reports()?.len();
        let frame = self.session.prepare(sql).await?;
        let frame = if parameters.is_empty() {
            frame
        } else {
            frame.with_param_values(parameters)?
        };
        let field_lookup = |col_name: &str| -> Option<ti_contracts::FieldSpec> {
            if let Some(f) = self.session.catalog.field(col_name) {
                return Some(f.clone());
            }
            if let Ok(extras) = self.session.extra_catalogs.lock() {
                for cat in extras.values() {
                    if let Some(f) = cat.field(col_name) {
                        return Some(f.clone());
                    }
                }
            }
            None
        };
        let specs: Vec<_> = frame
            .schema()
            .fields()
            .iter()
            .map(|f| field_lookup(f.name()))
            .collect();
        let names: Vec<_> = frame
            .schema()
            .fields()
            .iter()
            .zip(&specs)
            .map(|(f, spec)| {
                spec.as_ref()
                    .map(crate::field_name)
                    .unwrap_or_else(|| f.name().clone())
            })
            .collect();
        if names
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != names.len()
        {
            return Err(invalid(
                "duplicate output columns after @agg resolution; use distinct SQL aliases",
            ));
        }
        let columns: Vec<_> = frame
            .schema()
            .fields()
            .iter()
            .zip(&names)
            .zip(&specs)
            .map(|((f, name), spec)| {
                json!({
                    "name": name,
                    "type": f.data_type().to_string(),
                    "units": spec.as_ref().and_then(|spec| spec.units.as_deref())
                })
            })
            .collect();
        let fields: Vec<_> = frame
            .schema()
            .fields()
            .iter()
            .zip(&names)
            .zip(&specs)
            .map(|((f, name), spec)| {
                let mut metadata = f.metadata().clone();
                if let Some(units) = spec.as_ref().and_then(|s| s.units.as_ref()) {
                    metadata.insert("units".into(), units.clone());
                }
                datafusion::arrow::datatypes::Field::new(
                    name,
                    f.data_type().clone(),
                    f.is_nullable(),
                )
                .with_metadata(metadata)
            })
            .collect();
        let metadata = std::collections::HashMap::from([(
            "lume.units".into(),
            serde_json::to_string(&self.units())
                .map_err(|e| DataFusionError::External(Box::new(e)))?,
        )]);
        let schema = std::sync::Arc::new(
            datafusion::arrow::datatypes::Schema::new(fields).with_metadata(metadata),
        );
        let mut batches = Vec::new();
        let mut stream = frame.limit(0, Some(limit + 1))?.execute_stream().await?;
        let mut response = json!({"columns":columns, "rows":[], "row_count":0, "truncated":false,
            "elapsed_ms":0, "pushdown":"", "units":self.units(), "hint":null});
        let mut rows = Vec::new();
        while let Some(batch) = stream.next().await {
            let batch = batch?;
            let source_names: Vec<_> = batch
                .schema()
                .fields()
                .iter()
                .map(|f| f.name().clone())
                .collect();
            let mut kept = 0;
            let mut stop = false;
            for mut row in rows_json(std::slice::from_ref(&batch))? {
                let values: Vec<_> = source_names
                    .iter()
                    .map(|name| row.remove(name).unwrap_or(Value::Null))
                    .collect();
                row = names.iter().cloned().zip(values).collect();
                if rows.len() == limit {
                    response["truncated"] = json!(true);
                    stop = true;
                    break;
                }
                rows.push(row);
                response["rows"] = json!(&rows);
                if serde_json::to_vec(&response)
                    .map_err(|e| DataFusionError::External(Box::new(e)))?
                    .len()
                    > MAX_BYTES - 2048
                {
                    rows.pop();
                    response["truncated"] = json!(true);
                    stop = true;
                    break;
                }
                kept += 1;
            }
            if kept > 0 {
                let slice = batch.slice(0, kept);
                batches.push(datafusion::arrow::record_batch::RecordBatch::try_new(
                    schema.clone(),
                    slice
                        .columns()
                        .iter()
                        .zip(schema.fields())
                        .map(|(array, field)| {
                            if array.data_type() == field.data_type() {
                                Ok(array.clone())
                            } else {
                                datafusion::arrow::compute::cast(array, field.data_type())
                            }
                        })
                        .collect::<std::result::Result<Vec<_>, _>>()?,
                )?);
            }
            if stop {
                break;
            }
        }
        response["rows"] = json!(rows);
        response["row_count"] = json!(response["rows"].as_array().expect("rows array").len());
        response["elapsed_ms"] = json!(started.elapsed().as_millis() as u64);
        let reports = self.session.reports()?;
        let classes: std::collections::BTreeSet<_> = reports[first..]
            .iter()
            .flat_map(|r| r.filters.iter().map(|(_, class, _)| class.clone()))
            .collect();
        response["pushdown"] = json!(format!(
            "{} scans; conjunct classes: {:?}",
            reports.len() - first,
            classes
        ));
        if response["truncated"] == true {
            response["hint"] = json!("Aggregate results or narrow the time range.");
        }
        if serde_json::to_vec(&response)
            .map_err(|e| DataFusionError::External(Box::new(e)))?
            .len()
            > MAX_BYTES
        {
            return Err(invalid(
                "query metadata exceeds 64 KiB; select fewer columns",
            ));
        }
        Ok((response, batches, schema))
    }
    pub async fn explain(&self, sql: &str) -> Result<Value> {
        let started = Instant::now();
        Ok(
            json!({"plan":self.session.explain(sql).await?, "pushdown":"See Conjunct classes and execution details in plan", "units":self.units(), "elapsed_ms":started.elapsed().as_millis() as u64}),
        )
    }
    pub async fn schema(&self, prefix: Option<&str>, kind: Option<&str>) -> Result<Value> {
        let mut tables = Vec::new();
        let mut table_names = vec![
            "telemetry".to_string(),
            "docs".to_string(),
            "paths".to_string(),
            "vessels".to_string(),
            "shards".to_string(),
        ];
        if let Ok(extras) = self.session.extra_catalogs.lock() {
            for extra_name in extras.keys() {
                if !table_names.contains(extra_name) {
                    table_names.push(extra_name.clone());
                }
            }
        }
        let prefix = prefix.filter(|p| !p.is_empty());
        let table_filter = prefix.and_then(|p| {
            let short = p.rsplit('.').next().unwrap_or(p);
            table_names
                .iter()
                .find(|name| name.as_str() == short)
                .cloned()
        });
        let column_prefix = if table_filter.is_some() { None } else { prefix };
        let mut total_columns = 0usize;
        let mut matched_columns = 0usize;
        let mut returned_columns = 0usize;
        let mut examples = Vec::new();
        const COLUMN_LIMIT: usize = 256;
        for name in &table_names {
            let frame = self
                .session
                .prepare(&format!("SELECT * FROM {name} LIMIT 0"))
                .await?;
            let extra_cat = self
                .session
                .extra_catalogs
                .lock()
                .ok()
                .and_then(|m| m.get(name).cloned());
            let cat = extra_cat.as_deref().unwrap_or(&self.session.catalog);
            let columns: Vec<_> = frame
                .schema()
                .fields()
                .iter()
                .filter(|f| {
                    table_filter.as_ref().is_none_or(|table| table == name)
                        && column_prefix.is_none_or(|p| f.name().starts_with(p))
                        && kind.is_none_or(|k| f.data_type().to_string().eq_ignore_ascii_case(k))
                })
                .map(|f| {
                    let spec = cat.field(f.name());
                    json!({
                        "name": f.name(),
                        "type": f.data_type().to_string(),
                        "nullable": f.is_nullable(),
                        "units": spec.and_then(|s| s.units.as_deref()),
                        "scale": spec.and_then(|s| match s.kind {
                            ti_contracts::FieldKind::Bsi { scale } => Some(scale),
                            _ => None,
                        })
                    })
                })
                .collect();
            let column_count = frame.schema().fields().len();
            total_columns += column_count;
            if examples.len() < 6 {
                examples.extend(
                    frame
                        .schema()
                        .fields()
                        .iter()
                        .take(2)
                        .map(|f| format!("{name}.{}", f.name())),
                );
                examples.truncate(6);
            }
            let matching_column_count = columns.len();
            matched_columns += matching_column_count;
            let columns: Vec<_> = columns
                .into_iter()
                .take(COLUMN_LIMIT - returned_columns)
                .collect();
            returned_columns += columns.len();
            tables.push(json!({"name": name, "column_count": column_count,
                "matching_column_count": matching_column_count, "columns": columns}));
        }
        let coverage: Vec<_> = self
            .session
            .catalog
            .vessels
            .values()
            .map(|v| json!({"vessel": v.urn, "from": v.first_seen, "to": v.last_seen}))
            .collect();
        let truncated = returned_columns < matched_columns;
        let hint = if matched_columns == 0 {
            format!("no columns match prefix {}; {total_columns} columns exist, e.g. {}. Use a table name such as telemetry, or a Signal K path prefix such as environment.wind.",
                prefix.unwrap_or("(none)"), examples.join(", "))
        } else if truncated {
            format!("Returned {returned_columns} of {matched_columns} matching columns; narrow prefix or select a table.")
        } else {
            "Quote Signal K column names in SQL; call ti_resolve to find paths and vessels.".into()
        };
        Ok(
            json!({"tables": tables, "width_seconds": self.session.catalog.width_seconds,
            "time_coverage": coverage, "units": self.units(), "column_count": total_columns,
            "matching_column_count": matched_columns, "returned_column_count": returned_columns,
            "truncated": truncated, "hint": hint}),
        )
    }
    pub async fn status(&self) -> Result<Value> {
        let shards = rows_json(&self.session.query("SELECT * FROM shards").await?)?;
        let sealed = shards
            .iter()
            .filter(|r| r.get("sealed") == Some(&Value::Bool(true)))
            .count();
        let mut wal_bytes = 0u64;
        let wal = self.root.join("wal");
        if wal.is_dir() {
            for entry in std::fs::read_dir(wal)? {
                let entry = entry?;
                if entry.file_type()?.is_file() {
                    wal_bytes += entry.metadata()?.len();
                }
            }
        }
        let vessels: Vec<_> = self
            .session
            .catalog
            .vessels
            .values()
            .map(|v| json!({"vessel": v.urn, "last_seen": v.last_seen, "last_sync": null}))
            .collect();
        let mut unavailable = vec![
            "ingest_lag_seconds: no ingest supervisor attached".to_string(),
            "last_sync: sync is not implemented".to_string(),
        ];
        for s in &self.skipped_stores {
            unavailable.push(format!("store '{s}': catalog directory absent"));
        }
        let alerts = crate::rules::alert_status(&self.root)?;
        let report_path = self.root.join("parquet-import.json");
        let parquet_import: Value = if report_path.exists() {
            serde_json::from_slice(&std::fs::read(report_path)?)
                .map_err(|e| datafusion::common::DataFusionError::External(Box::new(e)))?
        } else {
            Value::Null
        };
        let mut ingest_lag_seconds = Value::Null;
        let mut last_delta = Value::Null;
        let mut reconnects = Value::Null;
        let mut documents_rejected_pre_epoch = Value::Null;
        let counter_names = [
            "samples_dropped_late",
            "samples_dropped_nonfinite",
            "samples_skipped_magnitude",
            "samples_rejected_source",
            "apply_failures",
            "apply_retries",
            "samples_rejected_blocked",
            "ingest_blocked",
        ];
        let mut ingest_counters = std::collections::BTreeMap::new();
        let ingest_status_file = self.root.join("ingest_status.json");
        if let Ok(content) = std::fs::read_to_string(&ingest_status_file) {
            if let Ok(val) = serde_json::from_str::<Value>(&content) {
                if let Some(lag) = val.get("ingest_lag_seconds") {
                    ingest_lag_seconds = lag.clone();
                    unavailable.retain(|u| !u.starts_with("ingest_lag_seconds"));
                }
                if let Some(ld) = val.get("last_delta") {
                    last_delta = ld.clone();
                }
                if let Some(count) = val.get("documents_rejected_pre_epoch") {
                    documents_rejected_pre_epoch = count.clone();
                }
                for name in counter_names {
                    if let Some(value) = val.get(name) {
                        ingest_counters.insert(name, value.clone());
                    }
                }
                if let Some(rc) = val.get("reconnects") {
                    reconnects = rc.clone();
                }
            }
        }
        let mut skipped_magnitudes = BTreeMap::new();
        for field in &self.session.catalog.fields {
            if field.kind != ti_contracts::FieldKind::Count || field.agg.is_some() {
                continue;
            }
            let Some(path) = field.path.strip_suffix("@skipped_magnitudes") else {
                continue;
            };
            let mut count = 0u64;
            for shard in self.session.source.shards(None, 0, u32::MAX) {
                let columns = self
                    .session
                    .source
                    .eval(shard, &ti_contracts::Predicate::Present(field.id))
                    .map_err(core_error)?;
                if columns.is_empty() {
                    continue;
                }
                let partial = self
                    .session
                    .source
                    .agg(shard, &columns, field.id, ti_contracts::AggOp::Sum)
                    .map_err(core_error)?;
                let ti_contracts::AggPartial::Sum { sum, .. } = partial else {
                    return Err(invalid(
                        "skipped-magnitude counter returned a non-sum partial",
                    ));
                };
                let sum = u64::try_from(sum)
                    .map_err(|_| invalid("skipped-magnitude counter exceeds u64"))?;
                count = count
                    .checked_add(sum)
                    .ok_or_else(|| invalid("skipped-magnitude counter exceeds u64"))?;
            }
            skipped_magnitudes.insert(path.to_string(), count);
        }
        let skipped_magnitude_total: u64 = skipped_magnitudes.values().sum();
        let mut res = json!({
            "store": self.root, "width_seconds": self.session.catalog.width_seconds,
            "documents": alerts["document_count"], "alerts": alerts["alert_count"],
            "active_alert_count": alerts["active_alert_count"], "active_alerts": alerts["active_alerts"],
            "active_alerts_truncated": alerts["active_alerts_truncated"],
            "wal_bytes": wal_bytes, "shards": {"open": shards.len() - sealed, "sealed": sealed},
            "ingest_lag_seconds": ingest_lag_seconds, "vessels": vessels, "units": self.units(),
            "skipped_stores": self.skipped_stores, "parquet_import": parquet_import,
            "skipped_magnitudes": {"total": skipped_magnitude_total, "paths": skipped_magnitudes},
            "documents_rejected_pre_epoch": documents_rejected_pre_epoch,
            "unavailable": unavailable
        });
        for name in counter_names {
            res[name] = ingest_counters.remove(name).unwrap_or(Value::Null);
        }
        if !last_delta.is_null() {
            res["last_delta"] = last_delta;
        }
        if !reconnects.is_null() {
            res["reconnects"] = reconnects;
        }
        Ok(res)
    }
}
