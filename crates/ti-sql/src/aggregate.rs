//! Whole final aggregates over exact bitmap scans bypass Arrow value materialization.
use crate::TelemetryExec;
use datafusion::arrow::array::{
    new_empty_array, new_null_array, ArrayRef, StringArray, TimestampSecondArray,
};
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::common::{
    config::ConfigOptions,
    tree_node::{Transformed, TreeNode},
    DataFusionError, Result, ScalarValue,
};
use datafusion::execution::TaskContext;
use datafusion::physical_expr::{
    expressions::{Column, Literal},
    EquivalenceProperties, PhysicalExpr, ScalarFunctionExpr,
};
use datafusion::physical_optimizer::PhysicalOptimizerRule;
use datafusion::physical_plan::aggregates::{AggregateExec, AggregateMode};
use datafusion::physical_plan::execution_plan::{Boundedness, EmissionType};
use datafusion::physical_plan::projection::ProjectionExec;
use datafusion::physical_plan::stream::RecordBatchStreamAdapter;
use datafusion::physical_plan::{
    DisplayAs, DisplayFormatType, ExecutionPlan, Partitioning, PlanProperties,
    SendableRecordBatchStream,
};
use std::{
    collections::BTreeMap,
    fmt::{Debug, Formatter},
    sync::{Arc, Mutex},
};
use ti_contracts::{AggOp, AggPartial, FieldKind, RoaringBitmap, ShardKey};

fn error(s: impl Into<String>) -> DataFusionError {
    DataFusionError::Execution(s.into())
}
fn empty(op: AggOp) -> AggPartial {
    match op {
        AggOp::CountAll | AggOp::Count => AggPartial::Count(0),
        AggOp::Sum => AggPartial::Sum { sum: 0, count: 0 },
        AggOp::Min => AggPartial::Min(None),
        AggOp::Max => AggPartial::Max(None),
    }
}
fn merge(left: &mut AggPartial, right: AggPartial) -> Result<()> {
    match (left, right) {
        (AggPartial::Count(a), AggPartial::Count(b)) => {
            *a = a
                .checked_add(b)
                .ok_or_else(|| error("bitmap aggregate count overflow"))?
        }
        (AggPartial::Sum { sum: a, count: ac }, AggPartial::Sum { sum: b, count: bc }) => {
            *a = a
                .checked_add(b)
                .ok_or_else(|| error("bitmap aggregate sum overflow"))?;
            *ac = ac
                .checked_add(bc)
                .ok_or_else(|| error("bitmap aggregate sum count overflow"))?;
        }
        (AggPartial::Min(a), AggPartial::Min(b)) => {
            if let Some(b) = b {
                *a = Some(a.map_or(b, |a| a.min(b)));
            }
        }
        (AggPartial::Max(a), AggPartial::Max(b)) => {
            if let Some(b) = b {
                *a = Some(a.map_or(b, |a| a.max(b)));
            }
        }
        _ => return Err(error("ShardSource returned a mismatched aggregate partial")),
    }
    Ok(())
}
#[derive(Debug, Clone)]
struct Request {
    field: u32,
    op: AggOp,
    scale: u8,
}
#[derive(Debug, Clone)]
struct Group {
    expr: Arc<dyn PhysicalExpr>,
    stride_ns: Option<i128>,
}

#[derive(Debug)]
pub(crate) struct BitmapAggregateRule {
    pub diagnostics: Arc<Mutex<Vec<String>>>,
}
impl PhysicalOptimizerRule for BitmapAggregateRule {
    fn name(&self) -> &str {
        "lume_bitmap_aggregate"
    }
    fn schema_check(&self) -> bool {
        true
    }
    fn optimize(
        &self,
        plan: Arc<dyn ExecutionPlan>,
        _config: &ConfigOptions,
    ) -> Result<Arc<dyn ExecutionPlan>> {
        Ok(plan.transform_down(|plan| {
            let Some(agg) = plan.downcast_ref::<AggregateExec>() else { return Ok(Transformed::no(plan)); };
            if matches!(agg.mode(), AggregateMode::Partial | AggregateMode::PartialReduce) { return Ok(Transformed::no(plan)); }
            match candidate(agg) {
                Ok(exec) => {
                    self.diagnostics.lock().map_err(|_| error("aggregate diagnostics lock poisoned"))?
                        .push("BitmapAggregateExec chosen: exact filters; supported count/BSI operations; bitmap group masks".into());
                    Ok(Transformed::yes(Arc::new(exec) as Arc<dyn ExecutionPlan>))
                }
                Err(reason) => {
                    self.diagnostics.lock().map_err(|_| error("aggregate diagnostics lock poisoned"))?
                        .push(format!("BitmapAggregateExec fallback: {reason}"));
                    Ok(Transformed::no(plan))
                }
            }
        })?.data)
    }
}
fn partial(plan: &Arc<dyn ExecutionPlan>) -> Option<&AggregateExec> {
    if let Some(agg) = plan.downcast_ref::<AggregateExec>() {
        return matches!(agg.mode(), AggregateMode::Partial).then_some(agg);
    }
    if !matches!(
        plan.name(),
        "RepartitionExec" | "CoalescePartitionsExec" | "CoalesceBatchesExec" | "CooperativeExec"
    ) {
        return None;
    }
    let children = plan.children();
    if children.len() == 1 {
        partial(children[0])
    } else {
        None
    }
}
/// Resolve projected columns back to scan expressions. Computed expressions
/// remain expressions and are rejected unless they are supported date_bin groups.
fn mapping(plan: &Arc<dyn ExecutionPlan>) -> Result<(TelemetryExec, Vec<Arc<dyn PhysicalExpr>>)> {
    if let Some(scan) = plan.downcast_ref::<TelemetryExec>() {
        let exprs = scan
            .schema()
            .fields()
            .iter()
            .enumerate()
            .map(|(index, f)| Arc::new(Column::new(f.name(), index)) as Arc<dyn PhysicalExpr>)
            .collect();
        return Ok((scan.clone(), exprs));
    }
    if let Some(projection) = plan.downcast_ref::<ProjectionExec>() {
        let (scan, input) = mapping(projection.input())?;
        let output = projection
            .expr()
            .iter()
            .map(|p| resolve(p.expr.clone(), &input))
            .collect::<Result<Vec<_>>>()?;
        return Ok((scan, output));
    }
    if matches!(
        plan.name(),
        "RepartitionExec" | "CoalescePartitionsExec" | "CoalesceBatchesExec" | "CooperativeExec"
    ) {
        let children = plan.children();
        if children.len() == 1 {
            return mapping(children[0]);
        }
    }
    Err(error(format!(
        "{} between aggregate and scan (residual filter or unsupported shape)",
        plan.name()
    )))
}
fn resolve(
    expr: Arc<dyn PhysicalExpr>,
    input: &[Arc<dyn PhysicalExpr>],
) -> Result<Arc<dyn PhysicalExpr>> {
    Ok(expr
        .transform_up(|e| {
            if let Some(col) = e.downcast_ref::<Column>() {
                let e = input
                    .get(col.index())
                    .ok_or_else(|| error("projection column index"))?
                    .clone();
                Ok(Transformed::yes(e))
            } else {
                Ok(Transformed::no(e))
            }
        })?
        .data)
}
fn direct_column(expr: &Arc<dyn PhysicalExpr>) -> Option<&Column> {
    expr.downcast_ref::<Column>()
}
fn time_column(expr: &Arc<dyn PhysicalExpr>) -> bool {
    if let Some(col) = direct_column(expr) {
        return col.name() == "ts";
    }
    if let Some(cast) = expr.downcast_ref::<datafusion::physical_expr::expressions::CastExpr>() {
        if !matches!(
            cast.cast_type(),
            datafusion::arrow::datatypes::DataType::Timestamp(_, _)
        ) {
            return false;
        }
        let children = expr.children();
        return children.len() == 1 && time_column(children[0]);
    }
    false
}
fn stride(expr: &Arc<dyn PhysicalExpr>) -> Option<i128> {
    let literal = expr.downcast_ref::<Literal>()?;
    let nanos = match literal.value() {
        ScalarValue::IntervalMonthDayNano(Some(v)) if v.months == 0 => {
            i128::from(v.days) * 86_400_000_000_000 + i128::from(v.nanoseconds)
        }
        ScalarValue::IntervalDayTime(Some(v)) => {
            i128::from(v.days) * 86_400_000_000_000 + i128::from(v.milliseconds) * 1_000_000
        }
        _ => return None,
    };
    (nanos > 0).then_some(nanos)
}
fn candidate(final_agg: &AggregateExec) -> std::result::Result<BitmapAggregateExec, String> {
    let agg = match final_agg.mode() {
        AggregateMode::Single | AggregateMode::SinglePartitioned => final_agg,
        AggregateMode::Final | AggregateMode::FinalPartitioned => partial(final_agg.input())
            .ok_or_else(|| "final input is not an unmodified partial aggregate".to_owned())?,
        _ => return Err("unsupported aggregate mode".into()),
    };
    if agg.group_expr().has_grouping_set() {
        return Err("grouping sets".into());
    }
    if agg.filter_expr().iter().any(Option::is_some) {
        return Err("aggregate FILTER clause".into());
    }
    let (scan, input) = mapping(agg.input()).map_err(|e| e.to_string())?;
    let mut requests = vec![];
    for a in agg.aggr_expr() {
        if a.is_distinct() || !a.order_bys().is_empty() {
            return Err("DISTINCT or ordered aggregate".into());
        }
        let op = match a.fun().name() {
            "count" => AggOp::Count,
            "sum" => AggOp::Sum,
            "min" => AggOp::Min,
            "max" => AggOp::Max,
            name => return Err(format!("unsupported aggregate {name}")),
        };
        let args = a.expressions();
        if args.len() != 1 {
            return Err("aggregate arity".into());
        }
        let expr = resolve(args[0].clone(), &input).map_err(|e| e.to_string())?;
        if op == AggOp::Count
            && (expr
                .downcast_ref::<Literal>()
                .is_some_and(|v| !v.value().is_null())
                || direct_column(&expr).is_some_and(|c| matches!(c.name(), "vessel" | "entity" | "ts")))
        {
            requests.push(Request {
                field: 0,
                op: AggOp::CountAll,
                scale: 0,
            });
            continue;
        }
        let col = direct_column(&expr)
            .ok_or_else(|| "aggregate argument is a computed expression".to_owned())?;
        let field = scan
            .catalog
            .field(col.name())
            .ok_or_else(|| format!("unindexed aggregate column {}", col.name()))?;
        let scale = match field.kind {
            FieldKind::Bsi { scale } => scale,
            _ if op == AggOp::Count => 0,
            _ => return Err("sum/min/max requires a BSI column".into()),
        };
        requests.push(Request {
            field: field.id,
            op,
            scale,
        });
    }
    let mut groups = vec![];
    let mut bins = 0;
    for (expr, _) in agg.group_expr().expr() {
        let expr = resolve(expr.clone(), &input).map_err(|e| e.to_string())?;
        let stride_ns = if direct_column(&expr).is_some_and(|c| matches!(c.name(), "vessel" | "entity")) {
            None
        } else if let Some(fun) = expr.downcast_ref::<ScalarFunctionExpr>() {
            if fun.name() != "date_bin"
                || !(2..=3).contains(&fun.args().len())
                || !time_column(&fun.args()[1])
            {
                return Err("GROUP BY must be vessel and/or date_bin(ts)".into());
            }
            if fun.args().get(2).is_some_and(|arg| {
                arg.downcast_ref::<Literal>()
                    .is_none_or(|value| value.value().is_null())
            }) {
                return Err("date_bin requires a nonnull constant origin".into());
            }
            bins += 1;
            Some(stride(&fun.args()[0]).ok_or_else(|| "date_bin requires a positive fixed-duration interval (calendar months unsupported)".to_owned())?)
        } else {
            return Err("GROUP BY must be vessel and/or date_bin(ts)".into());
        };
        groups.push(Group { expr, stride_ns });
    }
    if bins > 1 {
        return Err("multiple date_bin window sizes".into());
    }
    if requests.len() + groups.len() != final_agg.schema().fields().len() {
        return Err("aggregate output schema shape".into());
    }
    let properties = Arc::new(PlanProperties::new(
        EquivalenceProperties::new(final_agg.schema()),
        Partitioning::UnknownPartitioning(1),
        EmissionType::Final,
        Boundedness::Bounded,
    ));
    Ok(BitmapAggregateExec {
        scan,
        requests,
        groups,
        properties,
    })
}
#[derive(Debug)]
struct BitmapAggregateExec {
    scan: TelemetryExec,
    requests: Vec<Request>,
    groups: Vec<Group>,
    properties: Arc<PlanProperties>,
}
impl DisplayAs for BitmapAggregateExec {
    fn fmt_as(&self, _t: DisplayFormatType, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "BitmapAggregateExec: shards={}, operations={:?}, groups={:?}, all filters exact; Arrow telemetry rows=0; checked AggPartial merge",
            self.scan.keys.len(), self.requests, self.groups)?;
        write!(f, "; ")?;
        self.scan.fmt_as(_t, f)
    }
}
fn timestamp_ns(value: &ScalarValue) -> Result<i128> {
    let (value, factor) = match value {
        ScalarValue::TimestampSecond(Some(v), _) => (*v, 1_000_000_000),
        ScalarValue::TimestampMillisecond(Some(v), _) => (*v, 1_000_000),
        ScalarValue::TimestampMicrosecond(Some(v), _) => (*v, 1_000),
        ScalarValue::TimestampNanosecond(Some(v), _) => (*v, 1),
        _ => return Err(error("date_bin returned a non-timestamp or NULL")),
    };
    Ok(i128::from(value) * factor)
}
impl BitmapAggregateExec {
    fn group_values(&self, key: ShardKey, bucket: u64) -> Result<Vec<ScalarValue>> {
        if self.groups.is_empty() {
            return Ok(vec![]);
        }
        let vessel = self
            .scan
            .catalog
            .vessels
            .get(&key.vessel)
            .ok_or_else(|| error("aggregate vessel missing"))?;
        let ts = i64::try_from(
            i128::from(ti_contracts::EPOCH)
                + i128::from(bucket) * i128::from(self.scan.catalog.width_seconds),
        )
        .map_err(|_| error("aggregate timestamp overflow"))?;
        let arrays: Vec<ArrayRef> =
            self.scan
                .schema()
                .fields()
                .iter()
                .map(|f| match f.name().as_str() {
                    "vessel" | "entity" => Arc::new(StringArray::from(vec![vessel.urn.as_str()])) as ArrayRef,
                    "ts" => Arc::new(TimestampSecondArray::from(vec![ts]).with_timezone("UTC"))
                        as ArrayRef,
                    _ => new_null_array(f.data_type(), 1),
                })
                .collect();
        let batch = RecordBatch::try_new(self.scan.schema(), arrays)?;
        self.groups
            .iter()
            .map(|g| {
                let value = g.expr.evaluate(&batch)?;
                match value {
                    datafusion::logical_expr::ColumnarValue::Scalar(s) => Ok(s),
                    datafusion::logical_expr::ColumnarValue::Array(a) => {
                        ScalarValue::try_from_array(&a, 0)
                    }
                }
            })
            .collect()
    }
    fn batch(&self) -> Result<RecordBatch> {
        type Entry = (Vec<ScalarValue>, Vec<AggPartial>);
        let mut groups: BTreeMap<String, Entry> = BTreeMap::new();
        if self.groups.is_empty() {
            groups.insert(
                String::new(),
                (vec![], self.requests.iter().map(|r| empty(r.op)).collect()),
            );
        }
        let width_ns = i128::from(self.scan.catalog.width_seconds) * 1_000_000_000;
        for key in &self.scan.keys {
            let mut remaining = self.scan.selected_bitmap(*key)?;
            let base = u64::from(key.shard) << 16;
            while let Some(first) = remaining.min() {
                let bucket = base + u64::from(first);
                let values = self.group_values(*key, bucket)?;
                let selected = if let Some((index, group)) = self
                    .groups
                    .iter()
                    .enumerate()
                    .find(|(_, g)| g.stride_ns.is_some())
                {
                    let end_ns = timestamp_ns(&values[index])?
                        .checked_add(group.stride_ns.unwrap())
                        .ok_or_else(|| error("date_bin end overflow"))?;
                    let delta = end_ns - i128::from(ti_contracts::EPOCH) * 1_000_000_000;
                    // First bucket whose timestamp is >= end (ceil for off-grid origins).
                    let end_bucket =
                        delta.div_euclid(width_ns) + i128::from(delta.rem_euclid(width_ns) != 0);
                    if end_bucket <= i128::from(bucket) {
                        return Err(error("date_bin failed to advance"));
                    }
                    let local_end = (end_bucket - i128::from(base)).clamp(0, 65536) as u32;
                    let mut mask = RoaringBitmap::new();
                    mask.insert_range(first..local_end);
                    mask &= &remaining;
                    remaining.remove_range(first..local_end);
                    mask
                } else {
                    std::mem::take(&mut remaining)
                };
                let id = if values.is_empty() {
                    String::new()
                } else {
                    format!("{values:?}")
                };
                let (_, accumulated) = groups.entry(id).or_insert_with(|| {
                    (values, self.requests.iter().map(|r| empty(r.op)).collect())
                });
                for (request, total) in self.requests.iter().zip(accumulated) {
                    let value = self
                        .scan
                        .source
                        .agg(*key, &selected, request.field, request.op)
                        .map_err(crate::core_error)?;
                    merge(total, value)?;
                }
            }
        }
        let mut columns: Vec<Vec<ScalarValue>> = vec![vec![]; self.schema().fields().len()];
        for (_, (values, partials)) in groups {
            for (index, value) in values.into_iter().enumerate() {
                columns[index].push(value);
            }
            for (index, (request, partial)) in self.requests.iter().zip(partials).enumerate() {
                let value = match partial {
                    AggPartial::Count(v) => ScalarValue::Int64(Some(
                        i64::try_from(v).map_err(|_| error("SQL COUNT exceeds i64"))?,
                    )),
                    AggPartial::Sum { sum, count } => ScalarValue::Float64(
                        (count != 0).then(|| sum as f64 / 10f64.powi(i32::from(request.scale))),
                    ),
                    AggPartial::Min(v) | AggPartial::Max(v) => ScalarValue::Float64(
                        v.map(|v| v as f64 / 10f64.powi(i32::from(request.scale))),
                    ),
                };
                columns[self.groups.len() + index].push(value);
            }
        }
        let arrays = columns
            .into_iter()
            .zip(self.schema().fields())
            .map(|(values, field)| {
                if values.is_empty() {
                    Ok(new_empty_array(field.data_type()))
                } else {
                    ScalarValue::iter_to_array(values)
                }
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(RecordBatch::try_new(self.schema(), arrays)?)
    }
}
impl ExecutionPlan for BitmapAggregateExec {
    fn name(&self) -> &str {
        "BitmapAggregateExec"
    }
    fn properties(&self) -> &Arc<PlanProperties> {
        &self.properties
    }
    fn children(&self) -> Vec<&Arc<dyn ExecutionPlan>> {
        vec![]
    }
    fn with_new_children(
        self: Arc<Self>,
        children: Vec<Arc<dyn ExecutionPlan>>,
    ) -> Result<Arc<dyn ExecutionPlan>> {
        if !children.is_empty() {
            return Err(error("bitmap aggregate is a leaf"));
        }
        Ok(self)
    }
    fn apply_expressions(
        &self,
        _f: &mut dyn FnMut(
            &Arc<dyn PhysicalExpr>,
        ) -> Result<datafusion::common::tree_node::TreeNodeRecursion>,
    ) -> Result<datafusion::common::tree_node::TreeNodeRecursion> {
        Ok(datafusion::common::tree_node::TreeNodeRecursion::Continue)
    }
    fn execute(
        &self,
        partition: usize,
        _context: Arc<TaskContext>,
    ) -> Result<SendableRecordBatchStream> {
        if partition != 0 {
            return Err(error("invalid bitmap aggregate partition"));
        }
        let batch = self.batch();
        Ok(Box::pin(RecordBatchStreamAdapter::new(
            self.schema(),
            futures::stream::once(async move { batch }),
        )))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn partial_merge_checks_overflow_and_nulls() {
        assert!(merge(&mut AggPartial::Count(u64::MAX), AggPartial::Count(1)).is_err());
        assert!(merge(
            &mut AggPartial::Sum {
                sum: i128::MAX,
                count: 1
            },
            AggPartial::Sum { sum: 1, count: 1 }
        )
        .is_err());
        let mut v = AggPartial::Min(None);
        merge(&mut v, AggPartial::Min(Some(-5))).unwrap();
        assert_eq!(v, AggPartial::Min(Some(-5)));
        assert!(merge(&mut v, AggPartial::Max(None)).is_err());
    }
}
