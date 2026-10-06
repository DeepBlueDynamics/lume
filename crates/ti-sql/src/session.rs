use crate::{core_error, rewrite_sql, ScanReport, SqlCatalog, TelemetryProvider};
use datafusion::arrow::array::{
    Array, ArrayRef, BooleanArray, StringArray, TimestampSecondArray, UInt32Array, UInt64Array,
    UInt8Array,
};
use datafusion::arrow::datatypes::{DataType, TimeUnit};
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::common::{DataFusionError, Result};
use datafusion::datasource::MemTable;
use datafusion::logical_expr::{create_udf, ColumnarValue, Volatility};
use datafusion::prelude::{SessionConfig, SessionContext};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use ti_contracts::{Predicate, RoaringBitmap, ShardKey, ShardSource};

pub struct SqlSession {
    context: SessionContext,
    pub catalog: Arc<SqlCatalog>,
    pub source: Arc<dyn ShardSource>,
    reports: Arc<Mutex<Vec<ScanReport>>>,
    aggregate_diagnostics: Arc<Mutex<Vec<String>>>,
}
fn timestamp(v: Vec<i64>) -> ArrayRef {
    Arc::new(TimestampSecondArray::from(v).with_timezone("UTC"))
}
fn strings(v: Vec<Option<String>>) -> ArrayRef {
    Arc::new(StringArray::from(v))
}
impl SqlSession {
    pub async fn new(source: Arc<dyn ShardSource>, catalog: Arc<SqlCatalog>) -> Result<Self> {
        Self::new_with_bitmap_aggregates(source, catalog, true).await
    }
    /// Disable the TI aggregate optimizer for correctness and performance comparisons.
    pub async fn new_with_bitmap_aggregates(source: Arc<dyn ShardSource>, catalog: Arc<SqlCatalog>, enabled: bool) -> Result<Self> {
        let context =
            SessionContext::new_with_config(SessionConfig::new().with_target_partitions(2));
        let mut state = context.state();
        datafusion::logical_expr::registry::FunctionRegistry::register_function_rewrite(
            &mut state,
            Arc::new(crate::analyzer::TimestampRewrite::default()),
        )?;
        let aggregate_diagnostics = Arc::new(Mutex::new(vec![]));
        if enabled {
            let mut rules = state.physical_optimizers().to_vec();
            rules.insert(0, Arc::new(crate::aggregate::BitmapAggregateRule { diagnostics: aggregate_diagnostics.clone() }));
            state = datafusion::execution::SessionStateBuilder::new_from_existing(state)
                .with_physical_optimizer_rules(rules).build();
        }
        let context = SessionContext::new_with_state(state);
        let reports = Arc::new(Mutex::new(vec![]));
        context.register_table(
            "telemetry",
            Arc::new(TelemetryProvider {
                source: source.clone(),
                catalog: catalog.clone(),
                reports: reports.clone(),
            }),
        )?;
        context.register_table(
            "docs",
            Arc::new(MemTable::try_new(
                ti_contracts::docs_schema(),
                vec![vec![]],
            )?),
        )?;
        let vessels: Vec<_> = catalog.vessels.values().collect();
        let batch = RecordBatch::try_new(
            ti_contracts::vessels_schema(),
            vec![
                Arc::new(UInt32Array::from(
                    vessels.iter().map(|v| v.ord).collect::<Vec<_>>(),
                )),
                Arc::new(StringArray::from_iter_values(
                    vessels.iter().map(|v| v.urn.as_str()),
                )),
                strings(vessels.iter().map(|v| v.name.clone()).collect()),
                strings(vessels.iter().map(|v| v.mmsi.clone()).collect()),
                timestamp(vessels.iter().map(|v| v.first_seen).collect()),
                timestamp(vessels.iter().map(|v| v.last_seen).collect()),
            ],
        )?;
        context.register_table(
            "vessels",
            Arc::new(MemTable::try_new(
                ti_contracts::vessels_schema(),
                vec![vec![batch]],
            )?),
        )?;
        let fields = &catalog.fields;
        let first = catalog
            .vessels
            .values()
            .map(|v| v.first_seen)
            .min()
            .unwrap_or(ti_contracts::EPOCH);
        let last = catalog
            .vessels
            .values()
            .map(|v| v.last_seen)
            .max()
            .unwrap_or(ti_contracts::EPOCH);
        let batch = RecordBatch::try_new(
            ti_contracts::paths_schema(),
            vec![
                Arc::new(StringArray::from_iter_values(
                    fields.iter().map(|f| f.path.as_str()),
                )),
                Arc::new(UInt32Array::from(
                    fields.iter().map(|f| f.id).collect::<Vec<_>>(),
                )),
                strings(
                    fields
                        .iter()
                        .map(|f| {
                            f.agg.as_ref().map(|_| {
                                crate::field_name(f)
                                    .rsplit('@')
                                    .next()
                                    .expect("aggregate suffix")
                                    .into()
                            })
                        })
                        .collect(),
                ),
                Arc::new(StringArray::from_iter_values(fields.iter().map(
                    |f| match f.kind {
                        ti_contracts::FieldKind::Presence => "presence",
                        ti_contracts::FieldKind::Set => "set",
                        ti_contracts::FieldKind::Bsi { .. } => "bsi",
                        ti_contracts::FieldKind::Count => "count",
                        ti_contracts::FieldKind::Geo { .. } => "geo",
                    },
                ))),
                strings(fields.iter().map(|f| f.units.clone()).collect()),
                Arc::new(UInt8Array::from(
                    fields
                        .iter()
                        .map(|f| match f.kind {
                            ti_contracts::FieldKind::Bsi { scale } => Some(scale),
                            _ => None,
                        })
                        .collect::<Vec<_>>(),
                )),
                Arc::new(UInt8Array::from(vec![None; fields.len()])),
                strings(vec![None; fields.len()]),
                timestamp(vec![first; fields.len()]),
                timestamp(vec![last; fields.len()]),
            ],
        )?;
        context.register_table(
            "paths",
            Arc::new(MemTable::try_new(
                ti_contracts::paths_schema(),
                vec![vec![batch]],
            )?),
        )?;
        let keys = source.shards(None, 0, u32::MAX);
        let mut starts = vec![];
        let mut ends = vec![];
        let mut urns = vec![];
        for key in &keys {
            urns.push(
                catalog
                    .vessels
                    .get(&key.vessel)
                    .ok_or_else(|| DataFusionError::Plan("shard missing vessel catalog".into()))?
                    .urn
                    .clone(),
            );
            let base = (key.shard as u64) << 16;
            let cols = source.eval(*key, &Predicate::All).map_err(core_error)?;
            let from = base + cols.min().unwrap_or(0) as u64;
            let to = base + cols.max().unwrap_or(0) as u64 + 1;
            let ts = |b: u64| {
                i64::try_from(
                    ti_contracts::EPOCH as i128 + b as i128 * catalog.width_seconds as i128,
                )
                .map_err(|_| DataFusionError::Execution("catalog timestamp overflow".into()))
            };
            starts.push(ts(from)?);
            ends.push(ts(to)?);
        }
        let batch = RecordBatch::try_new(
            ti_contracts::shards_schema(),
            vec![
                Arc::new(StringArray::from(urns)),
                Arc::new(UInt32Array::from(
                    keys.iter().map(|k| k.shard).collect::<Vec<_>>(),
                )),
                timestamp(starts),
                timestamp(ends),
                Arc::new(BooleanArray::from(vec![false; keys.len()])),
                Arc::new(UInt64Array::from(vec![0; keys.len()])),
                strings(vec![None; keys.len()]),
            ],
        )?;
        context.register_table(
            "shards",
            Arc::new(MemTable::try_new(
                ti_contracts::shards_schema(),
                vec![vec![batch]],
            )?),
        )?;
        context.register_udtf("intervals", Arc::new(crate::intervals::IntervalsFunction { catalog: catalog.clone() }));
        register_functions(&context, source.clone(), catalog.clone());
        Ok(Self {
            context,
            catalog,
            source,
            reports,
            aggregate_diagnostics,
        })
    }
    /// Install authoritative frozen W2 catalog batches or the W5 docs provider.
    /// SQL writes remain rejected by rewrite_sql.
    pub fn register_derived_table(
        &self,
        name: &str,
        provider: Arc<dyn datafusion::catalog::TableProvider>,
    ) -> Result<()> {
        let expected = match name {
            "docs" => ti_contracts::docs_schema(),
            "vessels" => ti_contracts::vessels_schema(),
            "paths" => ti_contracts::paths_schema(),
            "shards" => ti_contracts::shards_schema(),
            _ => {
                return Err(DataFusionError::Plan(
                    "only docs/vessels/paths/shards adapters may be registered here".into(),
                ))
            }
        };
        if provider.schema() != expected {
            return Err(DataFusionError::Plan(format!(
                "{name} adapter must use the frozen schema"
            )));
        }
        self.context.deregister_table(name)?;
        self.context.register_table(name, provider)?;
        Ok(())
    }
    pub async fn register_raw(&self, root: &std::path::Path) -> Result<()> {
        crate::raw::register_raw(&self.context, root).await
    }
    /// Prepare a read-only DataFrame. W7 can bind placeholders with DataFusion's
    /// with_param_values API; the same analyzer then protects timestamp precision.
    pub async fn prepare(&self, sql: &str) -> Result<datafusion::dataframe::DataFrame> {
        let sql = rewrite_sql(sql, &self.catalog)?;
        self.context.sql(&sql).await
    }
    pub async fn query(&self, sql: &str) -> Result<Vec<RecordBatch>> {
        let sql = rewrite_sql(sql, &self.catalog)?;
        let statements = datafusion::sql::sqlparser::parser::Parser::parse_sql(
            &datafusion::sql::sqlparser::dialect::GenericDialect {},
            &sql,
        )
        .map_err(|e| DataFusionError::Plan(e.to_string()))?;
        if let Some(datafusion::sql::sqlparser::ast::Statement::Explain { statement, .. }) =
            statements.first()
        {
            let details = self.explain(&statement.to_string()).await?;
            let schema = Arc::new(datafusion::arrow::datatypes::Schema::new(vec![
                datafusion::arrow::datatypes::Field::new("plan_type", DataType::Utf8, false),
                datafusion::arrow::datatypes::Field::new("plan", DataType::Utf8, false),
            ]));
            return Ok(vec![RecordBatch::try_new(
                schema,
                vec![
                    Arc::new(StringArray::from(vec!["lume_ti"])),
                    Arc::new(StringArray::from(vec![details])),
                ],
            )?]);
        }
        self.context.sql(&sql).await?.collect().await
    }
    pub async fn explain(&self, sql: &str) -> Result<String> {
        let first_report = self.reports()?.len();
        let first_aggregate = self.aggregate_diagnostics.lock().map_err(|_| DataFusionError::Execution("aggregate diagnostics lock poisoned".into()))?.len();
        let rewritten = rewrite_sql(sql, &self.catalog)?;
        let dataframe = self.context.sql(&rewritten).await?;
        let physical = dataframe.create_physical_plan().await?;
        let plan = datafusion::physical_plan::displayable(physical.as_ref())
            .indent(true)
            .to_string();
        // EXPLAIN includes measured counters by executing the same read-only plan.
        let _ =
            datafusion::physical_plan::collect(physical.clone(), self.context.task_ctx()).await?;
        let measured = datafusion::physical_plan::displayable(physical.as_ref())
            .indent(true)
            .to_string();
        let classes = self.reports()?[first_report..]
            .iter()
            .flat_map(|r| r.filters.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let aggregate_details = self.aggregate_diagnostics.lock().map_err(|_| DataFusionError::Execution("aggregate diagnostics lock poisoned".into()))?[first_aggregate..].join("\n");
        Ok(format!(
            "{plan}\nLume TI execution details:\n{measured}\nConjunct classes: {classes:?}\n{aggregate_details}"
        ))
    }
    pub fn reports(&self) -> Result<Vec<ScanReport>> {
        Ok(self
            .reports
            .lock()
            .map_err(|_| DataFusionError::Execution("scan report lock poisoned".into()))?
            .clone())
    }
}

fn register_functions(
    context: &SessionContext,
    source: Arc<dyn ShardSource>,
    catalog: Arc<SqlCatalog>,
) {
    // An opaque identity UDF prevents DF55's comparison simplifier from
    // narrowing fractional literals back to TimestampSecond.
    // ti_timestamp is held by the analyzer, not registered as a user function.
    let cache = Arc::new(Mutex::new(BTreeMap::<
        (ShardKey, String, String),
        RoaringBitmap,
    >::new()));
    context.register_udf(create_udf(
        "ti_match",
        vec![
            DataType::Utf8,
            DataType::Timestamp(TimeUnit::Second, Some("UTC".into())),
            DataType::Utf8,
            DataType::Utf8,
        ],
        DataType::Boolean,
        Volatility::Stable,
        Arc::new(move |args: &[ColumnarValue]| {
            let arrays = ColumnarValue::values_to_arrays(args)?;
            let string = |index: usize| {
                arrays[index]
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .ok_or_else(|| {
                        DataFusionError::Execution("ti_match string argument type".into())
                    })
            };
            let vessels = string(0)?;
            let kinds = string(2)?;
            let queries = string(3)?;
            let times = arrays[1]
                .as_any()
                .downcast_ref::<TimestampSecondArray>()
                .ok_or_else(|| {
                    DataFusionError::Execution("ti_match timestamp argument type".into())
                })?;
            let mut result = vec![];
            for row in 0..vessels.len() {
                if arrays.iter().any(|a| a.is_null(row)) {
                    result.push(None);
                    continue;
                }
                let vessel = catalog
                    .vessels
                    .values()
                    .find(|v| v.urn == vessels.value(row))
                    .ok_or_else(|| DataFusionError::Execution("ti_match missing vessel".into()))?
                    .ord;
                let bucket = ti_contracts::bucket_of(times.value(row), catalog.width_seconds)
                    .map_err(core_error)?;
                let key = ti_contracts::shard_key(vessel, bucket);
                let kind = kinds.value(row).to_string();
                let query = queries.value(row).to_string();
                let cache_key = (key, kind.clone(), query.clone());
                let mut cache = cache
                    .lock()
                    .map_err(|_| DataFusionError::Execution("text cache lock poisoned".into()))?;
                if !cache.contains_key(&cache_key) {
                    let hits = source
                        .eval(key, &Predicate::Text { kind, query })
                        .map_err(core_error)?;
                    cache.insert(cache_key.clone(), hits);
                }
                result.push(Some(
                    cache[&cache_key].contains(ti_contracts::local_col(bucket)),
                ));
            }
            Ok(ColumnarValue::Array(Arc::new(BooleanArray::from(result))))
        }),
    ));
    for (name, arity) in [("in_bbox", 4), ("within_nm", 3)] {
        context.register_udf(create_udf(
            name,
            vec![DataType::Float64; arity],
            DataType::Boolean,
            Volatility::Immutable,
            Arc::new(|_| {
                Err(DataFusionError::NotImplemented(
                    "exact geo refinement requires the W6 adapter".into(),
                ))
            }),
        ));
    }
}
