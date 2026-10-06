//! `docs` table over an injected `DocumentIndex` (W5).
//!
//! Enforces:
//! - Rows follow the frozen `docs_schema`; `score` is null except under `match()` (spec/14).
//! - `match(body, 'q')` on this table is an **Exact** pushdown: the scan returns only
//!   matching documents with their BM25 score. Several `match` filters intersect by id.
//! - The `match` UDF itself never evaluates rows; reaching it means the call was not a
//!   direct docs filter, which is an error rather than a silent substring fallback.

use async_trait::async_trait;
use datafusion::arrow::array::{BooleanArray, StringArray};
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
                "match(body, 'q') must filter the docs table directly \
                 (or use match(notes|logbook|alerts, 'q') on telemetry)"
                    .into(),
            ))
        }),
    )
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
        ti_contracts::docs_schema()
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
        MemTable::try_new(self.schema(), vec![batches])?
            .scan(state, projection, &[], limit)
            .await
    }
}
