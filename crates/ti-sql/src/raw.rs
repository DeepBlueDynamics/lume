//! D20 normalization over real-layout Parquet, without merging incompatible
//! scalar schemas. Each file remains a ListingTable; a UNION view flattens objects.
use datafusion::arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use datafusion::datasource::MemTable;
use datafusion::{
    common::{DataFusionError, Result},
    prelude::{ParquetReadOptions, SessionContext},
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

fn parquet_files(root: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            parquet_files(&entry.path(), out)?;
        } else if kind.is_file() && entry.path().extension().is_some_and(|e| e == "parquet") {
            out.push(entry.path());
        }
    }
    Ok(())
}
fn quoted(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}
fn sql_string(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}
pub async fn register_raw(context: &SessionContext, root: &Path) -> Result<()> {
    let tier = root.join("tier=raw");
    let mut files = vec![];
    parquet_files(&tier, &mut files)?;
    files.sort();
    let mut selects = vec![];
    for (index, path) in files.iter().enumerate() {
        let table = format!("__ti_raw_{index}");
        let path = path
            .to_str()
            .ok_or_else(|| DataFusionError::Plan("Parquet path is not UTF-8".into()))?;
        context
            .register_parquet(&table, path, ParquetReadOptions::default())
            .await?;
        let provider = context.table_provider(&table).await?;
        let schema = provider.schema();
        let has = |name: &str| schema.index_of(name).is_ok();
        for name in ["context", "path", "received_timestamp", "source_label"] {
            if !has(name) {
                return Err(DataFusionError::Plan(format!(
                    "raw file {path} lacks {name}"
                )));
            }
        }
        let received = "TRY_CAST(received_timestamp AS TIMESTAMP(6))";
        let ts = if has("signalk_timestamp") {
            format!("COALESCE(TRY_CAST(signalk_timestamp AS TIMESTAMP(6)), {received})")
        } else {
            received.into()
        };
        let common = format!("context, {ts} AS ts");
        if has("value") {
            // Casting via VARCHAR agrees with DuckDB union_by_name when
            // scalar DOUBLE/BOOLEAN/UTF8 files are combined.
            selects.push(format!("SELECT {common}, path, TRY_CAST(CAST(value AS VARCHAR) AS DOUBLE) AS value, CASE WHEN TRY_CAST(CAST(value AS VARCHAR) AS DOUBLE) IS NULL THEN CAST(value AS VARCHAR) ELSE CAST(NULL AS VARCHAR) END AS value_str, source_label AS source FROM {table} WHERE value IS NOT NULL"));
        }
        for field in schema.fields() {
            if let Some(key) = field.name().strip_prefix("value_") {
                let column = quoted(field.name());
                selects.push(format!("SELECT {common}, concat(path, {}) AS path, TRY_CAST({column} AS DOUBLE) AS value, CAST(NULL AS VARCHAR) AS value_str, source_label AS source FROM {table} WHERE {column} IS NOT NULL",sql_string(&format!(".{key}"))));
            }
        }
    }
    if selects.is_empty() {
        let schema = Arc::new(Schema::new(vec![
            Field::new("context", DataType::Utf8, true),
            Field::new("ts", DataType::Timestamp(TimeUnit::Microsecond, None), true),
            Field::new("path", DataType::Utf8, true),
            Field::new("value", DataType::Float64, true),
            Field::new("value_str", DataType::Utf8, true),
            Field::new("source", DataType::Utf8, true),
        ]));
        context.register_table("raw", Arc::new(MemTable::try_new(schema, vec![vec![]])?))?;
    } else {
        let dataframe = context.sql(&selects.join(" UNION ALL ")).await?;
        context.register_table("raw", dataframe.into_view())?;
    }
    Ok(())
}
