use crate::catalog::{core_error, SqlCatalog};
use crate::classifier::{PlannedPredicate, PushdownClassifier};
use crate::materialize::BATCH_ROWS;
use async_trait::async_trait;
use datafusion::arrow::array::new_null_array;
use datafusion::arrow::datatypes::SchemaRef;
use datafusion::arrow::record_batch::{RecordBatch, RecordBatchOptions};
use datafusion::catalog::{Session, TableProvider};
use datafusion::common::{DataFusionError, Result};
use datafusion::execution::TaskContext;
use datafusion::logical_expr::{Expr, TableProviderFilterPushDown, TableType};
use datafusion::physical_expr::EquivalenceProperties;
use datafusion::physical_plan::execution_plan::{Boundedness, EmissionType};
use datafusion::physical_plan::stream::RecordBatchStreamAdapter;
use datafusion::physical_plan::{
    DisplayAs, DisplayFormatType, ExecutionPlan, Partitioning, PlanProperties,
    SendableRecordBatchStream,
};
use std::fmt::{Debug, Formatter};
use std::sync::{Arc, Mutex};
use ti_contracts::{Predicate, RoaringBitmap, ShardKey, ShardSource};

#[derive(Debug, Clone, Default)]
pub struct ScanReport {
    pub total_shards: usize,
    pub scanned_shards: usize,
    pub filters: Vec<(String, String, String)>,
    pub steps: Vec<(ShardKey, Vec<u64>)>,
    pub materialized_rows: u64,
}
/// Any frozen ShardSource works here. read() returns the frozen telemetry schema
/// for canonical selected fields and vessel/ts, in physical units. This provider
/// adds mean aliases and virtual columns to the SQL projection.
pub struct TelemetryProvider {
    pub source: Arc<dyn ShardSource>,
    pub catalog: Arc<SqlCatalog>,
    pub reports: Arc<Mutex<Vec<ScanReport>>>,
}
impl Debug for TelemetryProvider {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelemetryProvider")
            .field("fields", &self.catalog.fields.len())
            .finish()
    }
}
#[async_trait]
impl TableProvider for TelemetryProvider {
    fn schema(&self) -> SchemaRef {
        self.catalog.schema.clone()
    }
    fn table_type(&self) -> TableType {
        TableType::View
    }
    fn supports_filters_pushdown(
        &self,
        filters: &[&Expr],
    ) -> Result<Vec<TableProviderFilterPushDown>> {
        let classifier = PushdownClassifier {
            catalog: self.catalog.clone(),
        };
        let classes = filters
            .iter()
            .map(|e| classifier.classify(e))
            .collect::<Result<Vec<_>>>()?;
        // Planning records retain Unsupported conjuncts, which DataFusion
        // removes from the filters supplied to scan().
        self.reports
            .lock()
            .map_err(|_| DataFusionError::Execution("scan report lock poisoned".into()))?
            .push(ScanReport {
                filters: filters
                    .iter()
                    .zip(&classes)
                    .map(|(e, c)| (e.to_string(), format!("{:?}", c.class), c.reason.clone()))
                    .collect(),
                ..ScanReport::default()
            });
        Ok(classes.into_iter().map(|c| c.class).collect())
    }
    async fn scan(
        &self,
        _state: &dyn Session,
        projection: Option<&Vec<usize>>,
        filters: &[Expr],
        _limit: Option<usize>,
    ) -> Result<Arc<dyn ExecutionPlan>> {
        let classifier = PushdownClassifier {
            catalog: self.catalog.clone(),
        };
        let classes = filters
            .iter()
            .map(|e| classifier.classify(e))
            .collect::<Result<Vec<_>>>()?;
        let predicates: Vec<_> = classes.iter().filter_map(|c| c.predicate.clone()).collect();
        let all = self.source.shards(None, 0, u32::MAX);
        let keys: Vec<_> = all
            .iter()
            .filter(|key| predicates.iter().all(|p| p.may_match(**key)))
            .copied()
            .collect();
        let projection = projection
            .cloned()
            .unwrap_or_else(|| (0..self.catalog.schema.fields().len()).collect());
        let schema = Arc::new(self.catalog.schema.project(&projection)?);
        let mut field_ids = projection
            .iter()
            .filter_map(|i| {
                self.catalog
                    .columns
                    .get(self.catalog.schema.field(*i).name())
                    .copied()
            })
            .collect::<Vec<_>>();
        field_ids.sort();
        field_ids.dedup();
        let report = ScanReport {
            total_shards: all.len(),
            scanned_shards: keys.len(),
            filters: filters
                .iter()
                .zip(&classes)
                .map(|(e, c)| (e.to_string(), format!("{:?}", c.class), c.reason.clone()))
                .collect(),
            ..ScanReport::default()
        };
        let mut reports = self
            .reports
            .lock()
            .map_err(|_| DataFusionError::Execution("scan report lock poisoned".into()))?;
        let report_id = reports.len();
        reports.push(report);
        drop(reports);
        let properties = Arc::new(PlanProperties::new(
            EquivalenceProperties::new(schema.clone()),
            Partitioning::UnknownPartitioning(keys.len().max(1)),
            EmissionType::Incremental,
            Boundedness::Bounded,
        ));
        Ok(Arc::new(TelemetryExec {
            source: self.source.clone(),
            catalog: self.catalog.clone(),
            schema,
            keys,
            predicates,
            field_ids,
            properties,
            reports: self.reports.clone(),
            report_id,
        }))
    }
}

#[derive(Clone)]
pub struct TelemetryExec {
    pub(crate) source: Arc<dyn ShardSource>,
    pub(crate) catalog: Arc<SqlCatalog>,
    schema: SchemaRef,
    pub(crate) keys: Vec<ShardKey>,
    predicates: Vec<PlannedPredicate>,
    field_ids: Vec<u32>,
    properties: Arc<PlanProperties>,
    reports: Arc<Mutex<Vec<ScanReport>>>,
    report_id: usize,
}
impl TelemetryExec {
    fn evaluate_bitmap(&self, key: ShardKey) -> Result<(RoaringBitmap, Vec<u64>)> {
        let mut cols = self.source.eval(key, &Predicate::All).map_err(core_error)?;
        let mut counts = vec![cols.len()];
        for p in &self.predicates {
            cols &= self
                .source
                .eval(key, &p.for_shard(key))
                .map_err(core_error)?;
            counts.push(cols.len());
        }
        Ok((cols, counts))
    }
    /// Evaluate rules without accumulating a diagnostic row per closed bucket.
    pub(crate) fn rule_bitmap(&self, key: ShardKey) -> Result<RoaringBitmap> {
        Ok(self.evaluate_bitmap(key)?.0)
    }
    pub(crate) fn selected_bitmap(&self, key: ShardKey) -> Result<RoaringBitmap> {
        let (cols, counts) = self.evaluate_bitmap(key)?;
        self.reports
            .lock()
            .map_err(|_| DataFusionError::Execution("scan report lock poisoned".into()))?
            [self.report_id]
            .steps
            .push((key, counts));
        Ok(cols)
    }
}
impl Debug for TelemetryExec {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelemetryExec")
            .field("shards", &self.keys)
            .finish()
    }
}
impl DisplayAs for TelemetryExec {
    fn fmt_as(&self, _t: DisplayFormatType, f: &mut Formatter<'_>) -> std::fmt::Result {
        let reports = self.reports.lock().map_err(|_| std::fmt::Error)?;
        let r = &reports[self.report_id];
        write!(f,"LumeTI: shards scanned={} pruned={}, filters={:?}, bitmap cardinalities={:?}, materialized rows={}",r.scanned_shards,r.total_shards-r.scanned_shards,r.filters,r.steps,r.materialized_rows)
    }
}
impl ExecutionPlan for TelemetryExec {
    fn apply_expressions(
        &self,
        _f: &mut dyn FnMut(
            &Arc<dyn datafusion::physical_expr::PhysicalExpr>,
        ) -> Result<datafusion::common::tree_node::TreeNodeRecursion>,
    ) -> Result<datafusion::common::tree_node::TreeNodeRecursion> {
        Ok(datafusion::common::tree_node::TreeNodeRecursion::Continue)
    }
    fn name(&self) -> &str {
        "LumeTI"
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
            return Err(DataFusionError::Plan("LumeTI is a leaf plan".into()));
        }
        Ok(self)
    }
    fn execute(
        &self,
        partition: usize,
        _context: Arc<TaskContext>,
    ) -> Result<SendableRecordBatchStream> {
        let Some(key) = self.keys.get(partition).copied() else {
            if self.keys.is_empty() && partition == 0 {
                return Ok(Box::pin(RecordBatchStreamAdapter::new(
                    self.schema.clone(),
                    futures::stream::empty(),
                )));
            }
            return Err(DataFusionError::Execution("invalid shard partition".into()));
        };
        let cols = self.selected_bitmap(key)?;
        let source = self.source.clone();
        let catalog = self.catalog.clone();
        let fields = self.field_ids.clone();
        let schema = self.schema.clone();
        let reports = self.reports.clone();
        let report_id = self.report_id;
        let columns = cols.iter().collect::<Vec<_>>();
        let stream = futures::stream::unfold((columns, 0usize), move |(columns, offset)| {
            let source = source.clone();
            let catalog = catalog.clone();
            let fields = fields.clone();
            let schema = schema.clone();
            let reports = reports.clone();
            async move {
                if offset >= columns.len() {
                    return None;
                }
                let end = (offset + BATCH_ROWS).min(columns.len());
                let selected: RoaringBitmap = columns[offset..end].iter().copied().collect();
                let batch = (|| {
                    let batch = source.read(key, &selected, &fields).map_err(core_error)?;
                    let arrays = schema
                        .fields()
                        .iter()
                        .map(|f| {
                            if let Ok(index) = batch.schema().index_of(f.name()) {
                                return Ok(batch.column(index).clone());
                            }
                            if let Some(field) = catalog.field(f.name()) {
                                let index = batch.schema().index_of(&crate::field_name(field))?;
                                return Ok(batch.column(index).clone());
                            }
                            if matches!(f.name().as_str(), "notes" | "logbook" | "alerts") {
                                return Ok(new_null_array(f.data_type(), batch.num_rows()));
                            }
                            Err(DataFusionError::Execution(format!(
                                "source read missing required column {}",
                                f.name()
                            )))
                        })
                        .collect::<Result<Vec<_>>>()?;
                    let out = RecordBatch::try_new_with_options(
                        schema.clone(),
                        arrays,
                        &RecordBatchOptions::new().with_row_count(Some(selected.len() as usize)),
                    )?;
                    reports.lock().map_err(|_| {
                        DataFusionError::Execution("scan report lock poisoned".into())
                    })?[report_id]
                        .materialized_rows += out.num_rows() as u64;
                    Ok(out)
                })();
                Some((batch, (columns, end)))
            }
        });
        Ok(Box::pin(RecordBatchStreamAdapter::new(
            self.schema.clone(),
            stream,
        )))
    }
}
