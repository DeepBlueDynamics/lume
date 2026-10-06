//! Shared in-process CLI/MCP engine for the first W7 slice.
use crate::{core_error, rows_json, DocumentsFactory, SqlSession};
use datafusion::common::{DataFusionError, Result};
use futures::StreamExt;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
    time::Instant,
};

pub use tokio::runtime::Runtime as SurfaceRuntime;
pub use datafusion::arrow::ipc as arrow_ipc;
type QueryBatches = (Value, Vec<datafusion::arrow::record_batch::RecordBatch>, datafusion::arrow::datatypes::SchemaRef);
pub fn surface_runtime() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Runtime::new()?)
}
pub const MAX_ROWS: usize = 500;
pub const MAX_BYTES: usize = 64 * 1024;
pub struct TiEngine {
    pub session: SqlSession,
    root: PathBuf,
}
fn invalid(message: impl Into<String>) -> DataFusionError {
    DataFusionError::Plan(message.into())
}

/// Read one representative field-file header per shard directory, rejecting mixed stores.
/// Empty stores have no header: callers must supply a width or ti.toml.
pub fn store_width(root: &Path, requested: Option<u64>) -> Result<u64> {
    fn visit(path: &Path, width: &mut Option<u64>) -> Result<()> {
        if !path.exists() { return Ok(()); }
        let mut children = Vec::new();
        let mut representative: Option<PathBuf> = None;
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let ty = entry.file_type()?;
            if ty.is_dir() { children.push(entry.path()); }
            else if ty.is_file() && entry.path().extension().is_some_and(|s| s == "rbm") {
                let candidate = entry.path();
                if representative.as_ref().is_none_or(|current| candidate < *current) {
                    representative = Some(candidate);
                }
            }
        }
        // All field files in a shard version share its immutable width. Probe one
        // deterministic header per directory, while still checking every shard.
        if let Some(path) = representative {
            let mut file = std::io::BufReader::with_capacity(256, std::fs::File::open(path)?);
            let mut magic = [0; 8]; file.read_exact(&mut magic)?;
            if magic != ti_contracts::SHARD_MAGIC { return Err(invalid("invalid shard magic")); }
            let mut version = [0; 2]; file.read_exact(&mut version)?;
            if u16::from_le_bytes(version) != ti_contracts::FORMAT_VERSION { return Err(invalid("unsupported shard version")); }
            let mut len = [0; 4]; file.read_exact(&mut len)?;
            file.seek_relative(i64::from(u32::from_le_bytes(len)) + 8)?;
            let mut bytes = [0; 8]; file.read_exact(&mut bytes)?;
            let found = u64::from_le_bytes(bytes);
            if found == 0 || width.is_some_and(|w| w != found) { return Err(invalid("store bucket width mismatch")); }
            *width = Some(found);
        }
        for child in children { visit(&child, width)?; }
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
            return Err(invalid("store bucket width mismatch"));
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
        let width = store_width(root, width)?;
        let session = match documents {
            Some(factory) => crate::open_store_with_documents(root, width, factory).await?,
            None => crate::open_store(root, width).await?,
        };
        Ok(Self {
            session,
            root: root.into(),
        })
    }
    pub fn from_session(session: SqlSession, root: PathBuf) -> Self {
        Self { session, root }
    }
    pub fn units(&self) -> BTreeMap<String, Option<String>> {
        self.session
            .catalog
            .fields
            .iter()
            .map(|f| (crate::field_name(f), f.units.clone()))
            .collect()
    }
    /// Streaming result admission: collect at most max_rows+1 rows and 64 KiB
    /// including metadata. Stop reading once either cap is reached.
    pub async fn query(&self, sql: &str, max_rows: usize) -> Result<Value> {
        Ok(self.query_batches(sql,max_rows).await?.0)
    }
    pub async fn query_arrow(&self, sql: &str, max_rows: usize) -> Result<(Vec<u8>, usize, bool)> {
        let (reply, mut batches, schema) = self.query_batches(sql,max_rows).await?;
        let mut count = reply["row_count"].as_u64().unwrap_or(0) as usize;
        let mut truncated = reply["truncated"] == true;
        loop {
            let mut body = Vec::new();
            {
                let mut writer = datafusion::arrow::ipc::writer::StreamWriter::try_new(&mut body, &schema)?;
                for batch in &batches { writer.write(batch)?; }
                writer.finish()?;
            }
            if body.len() <= MAX_BYTES { return Ok((body,count,truncated)); }
            let Some(last) = batches.pop() else { return Err(invalid("Arrow schema exceeds 64 KiB; select fewer columns")); };
            if last.num_rows() > 1 { batches.push(last.slice(0,last.num_rows()-1)); }
            count -= 1;
            truncated = true;
        }
    }
    async fn query_batches(&self, sql: &str, max_rows: usize) -> Result<QueryBatches> {
        let started = Instant::now();
        let limit = max_rows.clamp(1, MAX_ROWS);
        let first = self.session.reports()?.len();
        let frame = self.session.prepare(sql).await?;
        let names: Vec<_> = frame.schema().fields().iter().map(|f| {
            self.session.catalog.field(f.name()).map(crate::field_name).unwrap_or_else(||f.name().clone())
        }).collect();
        if names.iter().collect::<std::collections::BTreeSet<_>>().len() != names.len() {
            return Err(invalid("duplicate output columns after @agg resolution; use distinct SQL aliases"));
        }
        let columns: Vec<_> = frame.schema().fields().iter().zip(&names).map(|(f,name)| json!({"name":name,"type":f.data_type().to_string(), "units":self.session.catalog.field(f.name()).and_then(|spec|spec.units.as_deref())})).collect();
        let fields: Vec<_> = frame.schema().fields().iter().zip(&names).map(|(f,name)| {
            let mut metadata = f.metadata().clone();
            if let Some(units) = self.session.catalog.field(f.name()).and_then(|s|s.units.as_ref()) {
                metadata.insert("units".into(), units.clone());
            }
            datafusion::arrow::datatypes::Field::new(name,f.data_type().clone(),f.is_nullable()).with_metadata(metadata)
        }).collect();
        let metadata = std::collections::HashMap::from([("lume.units".into(),serde_json::to_string(&self.units()).map_err(|e|DataFusionError::External(Box::new(e)))?)]);
        let schema = std::sync::Arc::new(datafusion::arrow::datatypes::Schema::new(fields).with_metadata(metadata));
        let mut batches = Vec::new();
        let mut stream = frame.limit(0, Some(limit + 1))?.execute_stream().await?;
        let mut response = json!({"columns":columns, "rows":[], "row_count":0, "truncated":false,
            "elapsed_ms":0, "pushdown":"", "units":self.units(), "hint":null});
        let mut rows = Vec::new();
        while let Some(batch) = stream.next().await {
            let batch = batch?;
            let source_names: Vec<_> = batch.schema().fields().iter().map(|f|f.name().clone()).collect();
            let mut kept = 0;
            let mut stop = false;
            for mut row in rows_json(std::slice::from_ref(&batch))? {
                let values: Vec<_> = source_names.iter().map(|name|row.remove(name).unwrap_or(Value::Null)).collect();
                row = names.iter().cloned().zip(values).collect();
                if rows.len() == limit { response["truncated"] = json!(true); stop = true; break; }
                rows.push(row);
                response["rows"] = json!(&rows);
                if serde_json::to_vec(&response).map_err(|e| DataFusionError::External(Box::new(e)))?.len() > MAX_BYTES - 2048 {
                    rows.pop();
                    response["truncated"] = json!(true);
                    stop = true; break;
                }
                kept += 1;
            }
            if kept > 0 {
                let slice = batch.slice(0,kept);
                batches.push(datafusion::arrow::record_batch::RecordBatch::try_new(schema.clone(),slice.columns().to_vec())?);
            }
            if stop { break; }
        }
        response["rows"] = json!(rows);
        response["row_count"] = json!(response["rows"].as_array().expect("rows array").len());
        response["elapsed_ms"] = json!(started.elapsed().as_millis() as u64);
        let reports = self.session.reports()?;
        let classes: std::collections::BTreeSet<_> = reports[first..].iter().flat_map(|r|r.filters.iter().map(|(_,class,_)|class.clone())).collect();
        response["pushdown"] = json!(format!("{} scans; conjunct classes: {:?}", reports.len()-first, classes));
        if response["truncated"] == true { response["hint"] = json!("Aggregate results or narrow the time range."); }
        if serde_json::to_vec(&response).map_err(|e| DataFusionError::External(Box::new(e)))?.len() > MAX_BYTES {
            return Err(invalid("query metadata exceeds 64 KiB; select fewer columns"));
        }
        Ok((response,batches,schema))
    }
    pub async fn explain(&self, sql: &str) -> Result<Value> {
        let started = Instant::now();
        Ok(
            json!({"plan":self.session.explain(sql).await?, "pushdown":"See Conjunct classes and execution details in plan", "units":self.units(), "elapsed_ms":started.elapsed().as_millis() as u64}),
        )
    }
    pub async fn schema(&self, prefix: Option<&str>, kind: Option<&str>) -> Result<Value> {
        let mut tables = Vec::new();
        for name in ["telemetry", "docs", "paths", "vessels", "shards"] {
            let frame = self
                .session
                .prepare(&format!("SELECT * FROM {name} LIMIT 0"))
                .await?;
            let columns: Vec<_> = frame.schema().fields().iter().filter(|f| prefix.is_none_or(|p|f.name().starts_with(p)) && kind.is_none_or(|k|f.data_type().to_string().eq_ignore_ascii_case(k))).map(|f| {
                let spec = self.session.catalog.field(f.name());
                json!({"name":f.name(),"type":f.data_type().to_string(),"nullable":f.is_nullable(),
                    "units":spec.and_then(|s|s.units.as_deref()),"scale":spec.and_then(|s|match s.kind{ti_contracts::FieldKind::Bsi{scale}=>Some(scale),_=>None})})
            }).collect();
            tables.push(json!({"name":name,"columns":columns}));
        }
        let coverage: Vec<_> = self
            .session
            .catalog
            .vessels
            .values()
            .map(|v| json!({"vessel":v.urn,"from":v.first_seen,"to":v.last_seen}))
            .collect();
        Ok(
            json!({"tables":tables,"width_seconds":self.session.catalog.width_seconds,"time_coverage":coverage,"units":self.units()}),
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
            .map(|v| json!({"vessel":v.urn,"last_seen":v.last_seen,"last_sync":null}))
            .collect();
        Ok(
            json!({"store":self.root,"width_seconds":self.session.catalog.width_seconds,
            "wal_bytes":wal_bytes,"shards":{"open":shards.len()-sealed,"sealed":sealed},
            "ingest_lag_seconds":null,"vessels":vessels,"units":self.units(),
            "unavailable":["ingest_lag_seconds: no ingest supervisor attached","last_sync: sync is not implemented"]}),
        )
    }
}
