//! `docs` table over an injected `DocumentIndex` (W5).
//!
//! Enforces:
//! - Rows follow the frozen `docs_schema`; `score` is null except under `match()` (spec/14).
//! - `match(body, 'q')` on this table is an **Exact** pushdown: the scan returns only
//!   matching documents with their BM25 score. Several `match` filters intersect by id.
//! - The `match` UDF itself never evaluates rows; reaching it means the call was not a
//!   direct docs filter, which is an error rather than a silent substring fallback.

use async_trait::async_trait;
use datafusion::arrow::array::{Array, BooleanArray, Float64Array, StringArray};
use datafusion::arrow::compute::filter_record_batch;
use datafusion::arrow::datatypes::{DataType, SchemaRef};
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::catalog::{Session, TableProvider};
use datafusion::common::{DataFusionError, Result, ScalarValue};
use datafusion::datasource::MemTable;
use datafusion::logical_expr::{
    create_udf, ColumnarValue, Expr, ScalarUDF, TableProviderFilterPushDown, TableType, Volatility,
};
use datafusion::physical_plan::ExecutionPlan;
use std::collections::BTreeSet;
use std::fmt::{Debug, Formatter};
use std::sync::Arc;
use ti_contracts::DocumentIndex;

/// The query string of a pushable `match(body, 'q')` filter.
fn match_query(e: &Expr) -> Option<&str> {
    let Expr::ScalarFunction(f) = e else {
        return None;
    };
    if !f.name().eq_ignore_ascii_case("match") || f.args.len() != 2 {
        return None;
    }
    let body = matches!(&f.args[0], Expr::Column(c) if c.name == "body");
    match &f.args[1] {
        Expr::Literal(ScalarValue::Utf8(Some(q)), _) if body => Some(q),
        _ => None,
    }
}

/// `match(Utf8, Utf8) -> Boolean`, resolved only through docs pushdown.
pub(crate) fn match_udf() -> ScalarUDF {
    create_udf(
        "match",
        vec![DataType::Utf8, DataType::Utf8],
        DataType::Boolean,
        Volatility::Stable,
        Arc::new(|_: &[ColumnarValue]| {
            Err(DataFusionError::Plan(
                "match() is only supported as a top-level AND filter on the docs or sections table \
                 (or use match(notes|logbook|alerts, 'q') on telemetry)"
                    .into(),
            ))
        }),
    )
}

fn json_lookup<'a>(val: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
    let obj = val.as_object()?;
    if let Some(found) = obj.get(key) {
        return Some(found);
    }
    if key.contains('.') {
        let mut cur = val;
        for part in key.split('.') {
            cur = cur.as_object()?.get(part)?;
        }
        return Some(cur);
    }
    None
}

fn extract_json_str(raw: &str, key: &str) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(raw).ok()?;
    match json_lookup(&parsed, key)? {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn extract_json_num(raw: &str, key: &str) -> Option<f64> {
    let parsed: serde_json::Value = serde_json::from_str(raw).ok()?;
    match json_lookup(&parsed, key)? {
        serde_json::Value::Number(n) => n.as_f64().filter(|f| f.is_finite()),
        serde_json::Value::String(s) => s.trim().parse::<f64>().ok().filter(|f| f.is_finite()),
        _ => None,
    }
}

/// `json_str(body, 'key') -> Utf8`: extracts a string (or scalar number/boolean as text)
/// from a JSON object string, returning NULL if missing, null, or non-scalar.
pub(crate) fn json_str_udf() -> ScalarUDF {
    create_udf(
        "json_str",
        vec![DataType::Utf8, DataType::Utf8],
        DataType::Utf8,
        Volatility::Immutable,
        Arc::new(|args: &[ColumnarValue]| {
            let arrays = ColumnarValue::values_to_arrays(args)?;
            let bodies = datafusion::arrow::compute::cast(&arrays[0], &DataType::Utf8)?;
            let keys = datafusion::arrow::compute::cast(&arrays[1], &DataType::Utf8)?;
            let bodies = bodies
                .as_any()
                .downcast_ref::<StringArray>()
                .ok_or_else(|| DataFusionError::Execution("json_str body must be Utf8".into()))?;
            let keys = keys
                .as_any()
                .downcast_ref::<StringArray>()
                .ok_or_else(|| DataFusionError::Execution("json_str key must be Utf8".into()))?;
            let out: Vec<Option<String>> = (0..bodies.len())
                .map(|row| {
                    if bodies.is_null(row) || keys.is_null(row) {
                        None
                    } else {
                        extract_json_str(bodies.value(row), keys.value(row))
                    }
                })
                .collect();
            Ok(ColumnarValue::Array(Arc::new(StringArray::from(out))))
        }),
    )
}

/// `json_num(body, 'key') -> Float64`: extracts a finite number (or numeric string)
/// from a JSON object string, returning NULL if missing or non-numeric.
pub(crate) fn json_num_udf() -> ScalarUDF {
    create_udf(
        "json_num",
        vec![DataType::Utf8, DataType::Utf8],
        DataType::Float64,
        Volatility::Immutable,
        Arc::new(|args: &[ColumnarValue]| {
            let arrays = ColumnarValue::values_to_arrays(args)?;
            let bodies = datafusion::arrow::compute::cast(&arrays[0], &DataType::Utf8)?;
            let keys = datafusion::arrow::compute::cast(&arrays[1], &DataType::Utf8)?;
            let bodies = bodies
                .as_any()
                .downcast_ref::<StringArray>()
                .ok_or_else(|| DataFusionError::Execution("json_num body must be Utf8".into()))?;
            let keys = keys
                .as_any()
                .downcast_ref::<StringArray>()
                .ok_or_else(|| DataFusionError::Execution("json_num key must be Utf8".into()))?;
            let out: Vec<Option<f64>> = (0..bodies.len())
                .map(|row| {
                    if bodies.is_null(row) || keys.is_null(row) {
                        None
                    } else {
                        extract_json_num(bodies.value(row), keys.value(row))
                    }
                })
                .collect();
            Ok(ColumnarValue::Array(Arc::new(Float64Array::from(out))))
        }),
    )
}

pub(crate) fn sql_schema() -> SchemaRef {
    let mut fields = ti_contracts::docs_schema().fields().to_vec();
    fields.push(Arc::new(datafusion::arrow::datatypes::Field::new(
        "entity",
        DataType::Utf8,
        false,
    )));
    Arc::new(datafusion::arrow::datatypes::Schema::new(fields))
}

/// `docs` provider; see the module docs.
pub struct DocsProvider {
    pub index: Arc<dyn DocumentIndex>,
}

impl Debug for DocsProvider {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("DocsProvider")
    }
}

fn keys(batch: &RecordBatch) -> Result<(&StringArray, &StringArray)> {
    let id = batch.column(0).as_any().downcast_ref::<StringArray>();
    let vessel = batch.column(1).as_any().downcast_ref::<StringArray>();
    match (id, vessel) {
        (Some(id), Some(vessel)) => Ok((id, vessel)),
        _ => Err(DataFusionError::Internal("docs id/vessel type".into())),
    }
}

fn identities(batches: &[RecordBatch]) -> Result<BTreeSet<(String, String)>> {
    let mut out = BTreeSet::new();
    for batch in batches {
        let (id, vessel) = keys(batch)?;
        for i in 0..batch.num_rows() {
            out.insert((vessel.value(i).to_string(), id.value(i).to_string()));
        }
    }
    Ok(out)
}

#[async_trait]
impl TableProvider for DocsProvider {
    fn schema(&self) -> SchemaRef {
        sql_schema()
    }
    fn table_type(&self) -> TableType {
        TableType::View
    }
    fn supports_filters_pushdown(
        &self,
        filters: &[&Expr],
    ) -> Result<Vec<TableProviderFilterPushDown>> {
        Ok(filters
            .iter()
            .map(|e| {
                if match_query(e).is_some() {
                    TableProviderFilterPushDown::Exact
                } else {
                    TableProviderFilterPushDown::Unsupported
                }
            })
            .collect())
    }
    async fn scan(
        &self,
        state: &dyn Session,
        projection: Option<&Vec<usize>>,
        filters: &[Expr],
        limit: Option<usize>,
    ) -> Result<Arc<dyn ExecutionPlan>> {
        let error = |e: ti_contracts::Error| DataFusionError::External(Box::new(e));
        let queries: Vec<&str> = filters.iter().filter_map(match_query).collect();
        let mut batches = match queries.first() {
            None => self.index.documents(None, None, None).map_err(error)?,
            Some(q) => self.index.documents(None, None, Some(q)).map_err(error)?,
        };
        for q in queries.iter().skip(1) {
            let keep = identities(&self.index.documents(None, None, Some(q)).map_err(error)?)?;
            batches = batches
                .iter()
                .map(|batch| {
                    let (id, vessel) = keys(batch)?;
                    let mask: BooleanArray =
                        (0..batch.num_rows())
                            .map(|i| {
                                Some(keep.contains(&(
                                    vessel.value(i).to_string(),
                                    id.value(i).to_string(),
                                )))
                            })
                            .collect();
                    Ok(filter_record_batch(batch, &mask)?)
                })
                .collect::<Result<_>>()?;
        }
        let batches = batches
            .into_iter()
            .map(|batch| {
                let mut columns = batch.columns().to_vec();
                columns.push(batch.column(1).clone());
                Ok(RecordBatch::try_new(self.schema(), columns)?)
            })
            .collect::<Result<Vec<_>>>()?;
        MemTable::try_new(self.schema(), vec![batches])?
            .scan(state, projection, &[], limit)
            .await
    }
}
