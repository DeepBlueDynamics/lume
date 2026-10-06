//! Interval extraction consumes compressed bitmap runs without reading telemetry values.
use crate::{SqlCatalog, TelemetryExec};
use async_trait::async_trait;
use datafusion::arrow::array::{Array, StringArray, TimestampSecondArray, UInt64Array};
use datafusion::arrow::datatypes::{DataType, Field, Schema, SchemaRef, TimeUnit};
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::catalog::{Session, TableFunctionArgs, TableFunctionImpl, TableProvider};
use datafusion::common::{DataFusionError, Result, ScalarValue};
use datafusion::execution::{SessionState, TaskContext};
use datafusion::logical_expr::{Expr, TableType};
use datafusion::physical_expr::EquivalenceProperties;
use datafusion::physical_plan::execution_plan::{Boundedness, EmissionType};
use datafusion::physical_plan::stream::RecordBatchStreamAdapter;
use datafusion::physical_plan::{
    DisplayAs, DisplayFormatType, ExecutionPlan, Partitioning, PlanProperties,
    SendableRecordBatchStream,
};
use datafusion::prelude::SessionContext;
use std::collections::BTreeMap;
use std::fmt::{Debug, Formatter};
use std::sync::Arc;

fn error(s: impl Into<String>) -> DataFusionError {
    DataFusionError::Plan(s.into())
}
fn schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("vessel", DataType::Utf8, false),
        Field::new(
            "start",
            DataType::Timestamp(TimeUnit::Second, Some("UTC".into())),
            false,
        ),
        Field::new(
            "end",
            DataType::Timestamp(TimeUnit::Second, Some("UTC".into())),
            false,
        ),
        Field::new("buckets", DataType::UInt64, false),
    ]))
}
fn string_arg(e: &Expr) -> Result<Option<String>> {
    match e {
        Expr::Literal(
            ScalarValue::Utf8(v) | ScalarValue::LargeUtf8(v) | ScalarValue::Utf8View(v),
            _,
        ) => Ok(v.clone()),
        Expr::Literal(ScalarValue::Null, _) => Ok(None),
        _ => Err(error("intervals arguments must be string literals or NULL")),
    }
}
/// Exact integer nanoseconds; fractional durations do not round across a bucket boundary.
fn duration(s: &str) -> Result<i128> {
    ti_contracts::parse_interval_duration_nanoseconds(s).map_err(crate::core_error)
}

#[derive(Debug)]
pub(crate) struct IntervalsFunction {
    pub catalog: Arc<SqlCatalog>,
}
impl TableFunctionImpl for IntervalsFunction {
    fn call_with_args(&self, args: TableFunctionArgs) -> Result<Arc<dyn TableProvider>> {
        let args = args.exprs();
        if args.is_empty() || args.len() > 4 {
            return Err(error(
                "intervals expects predicate_sql, min_len, max_gap, vessel",
            ));
        }
        let predicate =
            string_arg(&args[0])?.ok_or_else(|| error("intervals predicate_sql cannot be NULL"))?;
        // Parse as one expression, then serialize it: statement separators cannot escape the WHERE.
        let mut parser = datafusion::sql::sqlparser::parser::Parser::new(
            &datafusion::sql::sqlparser::dialect::GenericDialect {},
        )
        .try_with_sql(&predicate)
        .map_err(|e| error(e.to_string()))?;
        let expr = parser.parse_expr().map_err(|e| error(e.to_string()))?;
        if parser.peek_token().token != datafusion::sql::sqlparser::tokenizer::Token::EOF {
            return Err(error(
                "intervals predicate_sql must contain exactly one SQL expression",
            ));
        }
        let min_len = if let Some(e) = args.get(1) {
            duration(&string_arg(e)?.ok_or_else(|| error("min_len cannot be NULL"))?)?
        } else {
            0
        };
        let max_gap = if let Some(e) = args.get(2) {
            duration(&string_arg(e)?.ok_or_else(|| error("max_gap cannot be NULL"))?)?
        } else {
            0
        };
        let vessel = args.get(3).map(string_arg).transpose()?.flatten();
        let restriction = vessel
            .map(|s| format!(" AND vessel = '{}'", s.replace('\'', "''")))
            .unwrap_or_default();
        Ok(Arc::new(IntervalsProvider {
            sql: format!("SELECT vessel, ts FROM telemetry WHERE ({expr}){restriction}"),
            catalog: self.catalog.clone(),
            min_len,
            max_gap,
        }))
    }
}

#[derive(Debug)]
struct IntervalsProvider {
    sql: String,
    catalog: Arc<SqlCatalog>,
    min_len: i128,
    max_gap: i128,
}
#[async_trait]
impl TableProvider for IntervalsProvider {
    fn schema(&self) -> SchemaRef {
        schema()
    }
    fn table_type(&self) -> TableType {
        TableType::View
    }
    async fn scan(
        &self,
        state: &dyn Session,
        projection: Option<&Vec<usize>>,
        _filters: &[Expr],
        _limit: Option<usize>,
    ) -> Result<Arc<dyn ExecutionPlan>> {
        let state = state
            .as_any()
            .downcast_ref::<SessionState>()
            .ok_or_else(|| error("intervals requires SessionState"))?;
        let context = SessionContext::new_with_state(state.clone());
        let sql = crate::rewrite_sql(&self.sql, &self.catalog)?;
        let input = context.sql(&sql).await?.create_physical_plan().await?;
        let bitmap = bitmap_scan(&input);
        let projection = projection.cloned().unwrap_or_else(|| (0..4).collect());
        let schema = Arc::new(schema().project(&projection)?);
        let properties = Arc::new(PlanProperties::new(
            EquivalenceProperties::new(schema),
            Partitioning::UnknownPartitioning(1),
            EmissionType::Final,
            Boundedness::Bounded,
        ));
        Ok(Arc::new(IntervalsExec {
            input,
            bitmap,
            projection,
            properties,
            catalog: self.catalog.clone(),
            min_len: self.min_len,
            max_gap: self.max_gap,
        }))
    }
}
/// Only wrappers that preserve all rows are eligible. A residual Filter, limit,
/// join, or other operator keeps ordinary DataFusion execution.
pub(crate) fn bitmap_scan(plan: &Arc<dyn ExecutionPlan>) -> Option<TelemetryExec> {
    if let Some(scan) = plan.downcast_ref::<TelemetryExec>() {
        return Some(scan.clone());
    }
    if !matches!(
        plan.name(),
        "ProjectionExec"
            | "RepartitionExec"
            | "CoalescePartitionsExec"
            | "CoalesceBatchesExec"
            | "CooperativeExec"
    ) {
        return None;
    }
    let children = plan.children();
    if children.len() != 1 {
        return None;
    }
    bitmap_scan(children[0])
}
#[derive(Debug, Clone, PartialEq, Eq)]
struct Run {
    start: u64,
    end: u64,
    buckets: u64,
}
fn merge_runs(mut runs: Vec<Run>, width: u64, min_len: i128, max_gap: i128) -> Vec<Run> {
    runs.sort_by_key(|r| r.start);
    let nanos = i128::from(width) * 1_000_000_000;
    let mut merged: Vec<Run> = vec![];
    for run in runs {
        if let Some(last) = merged.last_mut() {
            if run.start >= last.end && i128::from(run.start - last.end) * nanos <= max_gap {
                last.end = run.end;
                last.buckets += run.buckets;
                continue;
            }
        }
        merged.push(run);
    }
    merged
        .into_iter()
        .filter(|r| i128::from(r.end - r.start) * nanos >= min_len)
        .collect()
}
fn bitmap_runs(bitmap: &ti_contracts::RoaringBitmap, base: u64) -> Vec<Run> {
    let mut iter = bitmap.iter();
    let mut out = vec![];
    // next_range consumes run containers directly when present.
    while let Some(range) = iter.next_range() {
        out.push(Run {
            start: base + u64::from(*range.start()),
            end: base + u64::from(*range.end()) + 1,
            buckets: u64::from(*range.end() - *range.start()) + 1,
        });
    }
    out
}
#[derive(Debug)]
struct IntervalsExec {
    input: Arc<dyn ExecutionPlan>,
    bitmap: Option<TelemetryExec>,
    catalog: Arc<SqlCatalog>,
    min_len: i128,
    max_gap: i128,
    projection: Vec<usize>,
    properties: Arc<PlanProperties>,
}
impl DisplayAs for IntervalsExec {
    fn fmt_as(&self, _t: DisplayFormatType, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "IntervalsExec: mode={}, min_len_ns={}, max_gap_ns={}, half-open; buckets exclude gaps",
            if self.bitmap.is_some() {
                "bitmap runs (all filters exact)"
            } else {
                "materialization fallback (residual predicate)"
            },
            self.min_len,
            self.max_gap
        )?;
        if let Some(scan) = &self.bitmap {
            write!(f, "; ")?;
            scan.fmt_as(_t, f)?;
        }
        Ok(())
    }
}
impl IntervalsExec {
    fn batch(&self, groups: BTreeMap<String, Vec<Run>>) -> Result<RecordBatch> {
        let mut vessels = vec![];
        let mut starts = vec![];
        let mut ends = vec![];
        let mut counts = vec![];
        let timestamp = |bucket: u64| {
            i64::try_from(
                i128::from(ti_contracts::EPOCH)
                    + i128::from(bucket) * i128::from(self.catalog.width_seconds),
            )
            .map_err(|_| error("interval timestamp overflow"))
        };
        for (vessel, runs) in groups {
            for run in merge_runs(runs, self.catalog.width_seconds, self.min_len, self.max_gap) {
                vessels.push(vessel.clone());
                starts.push(timestamp(run.start)?);
                ends.push(timestamp(run.end)?);
                counts.push(run.buckets);
            }
        }
        let batch = RecordBatch::try_new(
            schema(),
            vec![
                Arc::new(StringArray::from(vessels)),
                Arc::new(TimestampSecondArray::from(starts).with_timezone("UTC")),
                Arc::new(TimestampSecondArray::from(ends).with_timezone("UTC")),
                Arc::new(UInt64Array::from(counts)),
            ],
        )?;
        Ok(batch.project(&self.projection)?)
    }
}
impl ExecutionPlan for IntervalsExec {
    fn name(&self) -> &str {
        "IntervalsExec"
    }
    fn properties(&self) -> &Arc<PlanProperties> {
        &self.properties
    }
    fn children(&self) -> Vec<&Arc<dyn ExecutionPlan>> {
        if self.bitmap.is_some() {
            vec![]
        } else {
            vec![&self.input]
        }
    }
    fn with_new_children(
        self: Arc<Self>,
        children: Vec<Arc<dyn ExecutionPlan>>,
    ) -> Result<Arc<dyn ExecutionPlan>> {
        if self.bitmap.is_some() {
            if !children.is_empty() {
                return Err(error("bitmap intervals is a leaf"));
            }
            return Ok(self);
        }
        if children.len() != 1 {
            return Err(error("intervals expects one residual child"));
        }
        Ok(Arc::new(Self {
            input: children[0].clone(),
            bitmap: None,
            catalog: self.catalog.clone(),
            min_len: self.min_len,
            max_gap: self.max_gap,
            projection: self.projection.clone(),
            properties: self.properties.clone(),
        }))
    }
    fn apply_expressions(
        &self,
        _f: &mut dyn FnMut(
            &Arc<dyn datafusion::physical_expr::PhysicalExpr>,
        ) -> Result<datafusion::common::tree_node::TreeNodeRecursion>,
    ) -> Result<datafusion::common::tree_node::TreeNodeRecursion> {
        Ok(datafusion::common::tree_node::TreeNodeRecursion::Continue)
    }
    fn execute(
        &self,
        partition: usize,
        context: Arc<TaskContext>,
    ) -> Result<SendableRecordBatchStream> {
        if partition != 0 {
            return Err(error("invalid intervals partition"));
        }
        let this = Self {
            input: self.input.clone(),
            bitmap: self.bitmap.clone(),
            catalog: self.catalog.clone(),
            min_len: self.min_len,
            max_gap: self.max_gap,
            projection: self.projection.clone(),
            properties: self.properties.clone(),
        };
        let stream = futures::stream::once(async move {
            let mut groups: BTreeMap<String, Vec<Run>> = BTreeMap::new();
            if let Some(scan) = &this.bitmap {
                for key in &scan.keys {
                    let bitmap = scan.selected_bitmap(*key)?;
                    let vessel = scan
                        .catalog
                        .vessels
                        .get(&key.vessel)
                        .ok_or_else(|| error("interval vessel missing"))?;
                    groups
                        .entry(vessel.urn.clone())
                        .or_default()
                        .extend(bitmap_runs(&bitmap, u64::from(key.shard) << 16));
                }
            } else {
                let batches =
                    datafusion::physical_plan::collect(this.input.clone(), context).await?;
                let mut selected: BTreeMap<String, ti_contracts::RoaringBitmap> = BTreeMap::new();
                for batch in batches {
                    let vessels = batch
                        .column(0)
                        .as_any()
                        .downcast_ref::<StringArray>()
                        .ok_or_else(|| error("interval vessel type"))?;
                    let times = batch
                        .column(1)
                        .as_any()
                        .downcast_ref::<TimestampSecondArray>()
                        .ok_or_else(|| error("interval timestamp type"))?;
                    for row in 0..batch.num_rows() {
                        let bucket =
                            ti_contracts::bucket_of(times.value(row), this.catalog.width_seconds)
                                .map_err(crate::core_error)?;
                        selected
                            .entry(vessels.value(row).to_owned())
                            .or_default()
                            .insert(bucket);
                    }
                }
                for (vessel, bitmap) in selected {
                    groups.insert(vessel, bitmap_runs(&bitmap, 0));
                }
            }
            this.batch(groups)
        });
        Ok(Box::pin(RecordBatchStreamAdapter::new(
            self.schema(),
            stream,
        )))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn merged_span_and_matching_count() {
        let runs = vec![
            Run {
                start: 65535,
                end: 65536,
                buckets: 1,
            },
            Run {
                start: 65536,
                end: 65537,
                buckets: 1,
            },
            Run {
                start: 65538,
                end: 65539,
                buckets: 1,
            },
        ];
        assert_eq!(
            merge_runs(runs.clone(), 10, 40_000_000_000, 10_000_000_000),
            vec![Run {
                start: 65535,
                end: 65539,
                buckets: 3
            }]
        );
        assert!(merge_runs(runs.clone(), 10, 40_000_000_001, 10_000_000_000).is_empty());
        assert_eq!(merge_runs(runs, 10, 0, 9_999_999_999).len(), 2);
    }
    #[test]
    fn durations_are_exact_and_checked() {
        assert_eq!(duration("1.5m").unwrap(), 90_000_000_000);
        assert_eq!(duration("0s").unwrap(), 0);
        for bad in [
            "-1s",
            "NaNs",
            "1",
            "0.000000001ms",
            "999999999999999999999999999999999999999h",
        ] {
            assert!(duration(bad).is_err(), "{bad}");
        }
    }
}
