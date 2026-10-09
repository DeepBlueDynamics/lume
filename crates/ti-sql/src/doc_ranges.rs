//! Conservative inner-join range pruning. The original join remains authoritative.
//! Read only materialized memory inputs; ignore residual docs filters (a superset).
use crate::{PlannedPredicate, TelemetryExec};
use datafusion::{
    arrow::{
        array::{Array, StringArray, TimestampSecondArray},
        datatypes::{DataType, TimeUnit},
        record_batch::RecordBatch,
    },
    common::{
        config::ConfigOptions,
        tree_node::{Transformed, TreeNode},
        JoinSide, JoinType, Result,
    },
    datasource::memory::{DataSourceExec, MemorySourceConfig},
    logical_expr::Operator,
    physical_expr::{
        expressions::{BinaryExpr, CastExpr, Column},
        PhysicalExpr, ScalarFunctionExpr,
    },
    physical_optimizer::PhysicalOptimizerRule,
    physical_plan::{
        filter::FilterExec,
        joins::{utils::JoinFilter, HashJoinExec},
        projection::ProjectionExec,
        ExecutionPlan,
    },
};
use std::{collections::BTreeMap, sync::Arc};
use ti_contracts::{Predicate, EPOCH};

const MAX_ROWS: usize = 4096;

#[derive(Debug)]
pub(crate) struct DocRangeRule;
impl PhysicalOptimizerRule for DocRangeRule {
    fn name(&self) -> &str {
        "lume_document_ranges"
    }
    fn schema_check(&self) -> bool {
        true
    }
    fn optimize(
        &self,
        plan: Arc<dyn ExecutionPlan>,
        _: &ConfigOptions,
    ) -> Result<Arc<dyn ExecutionPlan>> {
        Ok(plan
            .transform_down(|plan| {
                let Some(join) = plan.downcast_ref::<HashJoinExec>() else {
                    return Ok(Transformed::no(plan));
                };
                if *join.join_type() != JoinType::Inner {
                    return Ok(Transformed::no(plan));
                }
                for (side, input, docs) in [
                    (JoinSide::Right, join.right(), join.left()),
                    (JoinSide::Left, join.left(), join.right()),
                ] {
                    let Some(scan) = input.downcast_ref::<TelemetryExec>() else {
                        continue;
                    };
                    let Some(predicate) = candidate(join, side, docs, scan)? else {
                        continue;
                    };
                    let pruned =
                        Arc::new(scan.prune_document_ranges(predicate)?) as Arc<dyn ExecutionPlan>;
                    let children = if side == JoinSide::Right {
                        vec![join.left().clone(), pruned]
                    } else {
                        vec![pruned, join.right().clone()]
                    };
                    // Rebuild using the standard tree API, preserving all join settings.
                    return Ok(Transformed::yes(plan.clone().replace_children(
                        children,
                        datafusion::physical_plan::ReplaceChildrenOptions::new(
                            datafusion::physical_plan::ChildrenPropertiesMode::Recompute,
                        ),
                    )?));
                }
                Ok(Transformed::no(plan))
            })?
            .data)
    }
}
// Peel only lossless timestamp wrappers. Source timestamps are verified as seconds.
fn column(expr: &Arc<dyn PhysicalExpr>) -> Option<&Column> {
    if let Some(c) = expr.downcast_ref::<Column>() {
        return Some(c);
    }
    if let Some(c) = expr.downcast_ref::<CastExpr>() {
        if matches!(c.cast_type(), DataType::Timestamp(_, _)) {
            return column(c.expr());
        }
    }
    if let Some(f) = expr.downcast_ref::<ScalarFunctionExpr>() {
        if f.name() == "ti_timestamp" && f.args().len() == 1 {
            return column(&f.args()[0]);
        }
    }
    None
}
fn bound(
    filter: &JoinFilter,
    side: JoinSide,
    expr: &Arc<dyn PhysicalExpr>,
) -> Option<(usize, bool)> {
    let b = expr.downcast_ref::<BinaryExpr>()?;
    let left = column(b.left())?;
    let right = column(b.right())?;
    let li = filter.column_indices().get(left.index())?;
    let ri = filter.column_indices().get(right.index())?;
    let (time, doc, op) = if li.side == side && ri.side != side {
        (li, ri, *b.op())
    } else if ri.side == side && li.side != side {
        let op = match b.op() {
            Operator::Gt => Operator::Lt,
            Operator::GtEq => Operator::LtEq,
            Operator::Lt => Operator::Gt,
            Operator::LtEq => Operator::GtEq,
            _ => return None,
        };
        (ri, li, op)
    } else {
        return None;
    };
    // Caller checks the actual telemetry column and docs field types.
    let _ = time;
    match op {
        Operator::Gt | Operator::GtEq => Some((doc.index, true)),
        Operator::Lt | Operator::LtEq => Some((doc.index, false)),
        _ => None,
    }
}
fn conjuncts(expr: &Arc<dyn PhysicalExpr>, out: &mut Vec<Arc<dyn PhysicalExpr>>) {
    if let Some(b) = expr.downcast_ref::<BinaryExpr>() {
        if *b.op() == Operator::And {
            conjuncts(b.left(), out);
            conjuncts(b.right(), out);
            return;
        }
    }
    out.push(expr.clone());
}
fn memory_batches(plan: &Arc<dyn ExecutionPlan>) -> Result<Option<Vec<RecordBatch>>> {
    if let Some(source) = plan.downcast_ref::<DataSourceExec>() {
        let Some(memory) = source.data_source().downcast_ref::<MemorySourceConfig>() else {
            return Ok(None);
        };
        if memory
            .partitions()
            .iter()
            .flatten()
            .map(RecordBatch::num_rows)
            .sum::<usize>()
            > MAX_ROWS
        {
            return Ok(None);
        }
        return memory
            .partitions()
            .iter()
            .flatten()
            .map(|b| {
                match memory.projection() {
                    Some(p) => b.project(p),
                    None => Ok(b.clone()),
                }
                .map_err(Into::into)
            })
            .collect::<Result<Vec<_>>>()
            .map(Some);
    }
    if let Some(filter) = plan.downcast_ref::<FilterExec>() {
        let Some(batches) = memory_batches(filter.input())? else {
            return Ok(None);
        };
        // Ignoring the predicate and fetch is conservative, including volatile filters.
        return batches
            .iter()
            .map(|b| match filter.projection() {
                Some(p) => b.project(p.as_ref()).map_err(Into::into),
                None => Ok(b.clone()),
            })
            .collect::<Result<Vec<_>>>()
            .map(Some);
    }
    if let Some(projection) = plan.downcast_ref::<ProjectionExec>() {
        let indices = projection
            .expr()
            .iter()
            .map(|p| p.expr.downcast_ref::<Column>().map(Column::index))
            .collect::<Option<Vec<_>>>();
        let Some(indices) = indices else {
            return Ok(None);
        };
        let Some(batches) = memory_batches(projection.input())? else {
            return Ok(None);
        };
        return batches
            .iter()
            .map(|b| b.project(&indices).map_err(Into::into))
            .collect::<Result<Vec<_>>>()
            .map(Some);
    }
    Ok(None)
}
fn candidate(
    join: &HashJoinExec,
    side: JoinSide,
    docs: &Arc<dyn ExecutionPlan>,
    scan: &TelemetryExec,
) -> Result<Option<PlannedPredicate>> {
    let Some(filter) = join.filter() else {
        return Ok(None);
    };
    let scan_schema = scan.schema();
    let docs_schema = docs.schema();
    let mut vessel = None;
    for (l, r) in join.on() {
        let (t, d) = if side == JoinSide::Right {
            (r, l)
        } else {
            (l, r)
        };
        if let (Some(t), Some(d)) = (t.downcast_ref::<Column>(), d.downcast_ref::<Column>()) {
            if matches!(
                scan_schema.field(t.index()).name().as_str(),
                "vessel" | "entity"
            ) && matches!(docs_schema.field(d.index()).data_type(), DataType::Utf8)
            {
                vessel = Some(d.index());
            }
        }
    }
    let Some(vessel) = vessel else {
        return Ok(None);
    };
    let mut terms = vec![];
    conjuncts(filter.expression(), &mut terms);
    let mut start = None;
    let mut end = None;
    for expr in &terms {
        let Some((index, lower)) = bound(filter, side, expr) else {
            continue;
        };
        let b = expr.downcast_ref::<BinaryExpr>().unwrap();
        let t = [b.left(), b.right()]
            .into_iter()
            .filter_map(column)
            .find(|c| filter.column_indices()[c.index()].side == side)
            .unwrap();
        let t = filter.column_indices()[t.index()].index;
        if scan_schema.field(t).name() != "ts"
            || !matches!(
                scan_schema.field(t).data_type(),
                DataType::Timestamp(TimeUnit::Second, _)
            )
            || !matches!(
                docs_schema.field(index).data_type(),
                DataType::Timestamp(TimeUnit::Second, _)
            )
        {
            continue;
        }
        if lower {
            start = Some(index);
        } else {
            end = Some(index);
        }
    }
    let (Some(start), Some(end)) = (start, end) else {
        return Ok(None);
    };
    let Some(batches) = memory_batches(docs)? else {
        return Ok(None);
    };
    let mut ranges: BTreeMap<u32, Vec<(u32, u32)>> = BTreeMap::new();
    let width = i128::from(scan.catalog.width_seconds);
    for batch in batches {
        let Some(vessels) = batch.column(vessel).as_any().downcast_ref::<StringArray>() else {
            return Ok(None);
        };
        let Some(starts) = batch
            .column(start)
            .as_any()
            .downcast_ref::<TimestampSecondArray>()
        else {
            return Ok(None);
        };
        let Some(ends) = batch
            .column(end)
            .as_any()
            .downcast_ref::<TimestampSecondArray>()
        else {
            return Ok(None);
        };
        for row in 0..batch.num_rows() {
            if starts.is_null(row) || ends.is_null(row) {
                return Ok(None);
            }
            if vessels.is_null(row) {
                continue;
            }
            let Some(v) = scan
                .catalog
                .vessels
                .values()
                .find(|v| v.urn == vessels.value(row))
            else {
                continue;
            };
            let low = i128::from(starts.value(row)) - i128::from(EPOCH);
            let high = i128::from(ends.value(row)) - i128::from(EPOCH);
            let from = low.div_euclid(width) + i128::from(low.rem_euclid(width) != 0);
            let to = high.div_euclid(width);
            if from > to || to < 0 || from > i128::from(u32::MAX) {
                continue;
            }
            ranges
                .entry(v.ord)
                .or_default()
                .push((from.max(0) as u32, to.min(i128::from(u32::MAX)) as u32));
        }
    }
    let mut predicates = vec![];
    for (vessel, mut spans) in ranges {
        spans.sort_unstable();
        let mut merged: Vec<(u32, u32)> = vec![];
        for (from, to) in spans {
            if let Some(last) = merged.last_mut() {
                if from <= last.1.saturating_add(1) {
                    last.1 = last.1.max(to);
                    continue;
                }
            }
            merged.push((from, to));
        }
        predicates.push(PlannedPredicate::And(vec![
            PlannedPredicate::Vessel(vec![vessel]),
            PlannedPredicate::Or(
                merged
                    .into_iter()
                    .map(|(from, to)| PlannedPredicate::Bitmap(Predicate::TsRange { from, to }))
                    .collect(),
            ),
        ]));
    }
    Ok(Some(PlannedPredicate::Or(predicates)))
}
