//! Projection-aware Arrow materialization using W1 batched reconstruction.
//! The generic provider consumes ShardSource::read; this adapter implements that
//! frozen read boundary for the W1 MemorySource fixture, without changing contracts.
use crate::catalog::SqlCatalog;
use datafusion::arrow::{
    array::{
        builder::{ListBuilder, StringBuilder, UInt64Builder},
        new_null_array, ArrayRef, BooleanArray, Float64Array, StringArray, TimestampSecondArray,
        UInt64Array,
    },
    datatypes::DataType,
    record_batch::{RecordBatch, RecordBatchOptions},
};
use std::collections::BTreeSet;
use std::sync::Arc;
use ti_contracts::{
    AggOp, AggPartial, Error, Predicate, Result, RoaringBitmap, ShardKey, ShardSource, VesselOrd,
};
use ti_core::{FieldData, MemorySource};

pub const BATCH_ROWS: usize = 8192;

pub struct FixtureSource {
    pub memory: MemorySource,
    pub catalog: Arc<SqlCatalog>,
}
impl FixtureSource {
    fn batch(&self, key: ShardKey, cols: &RoaringBitmap, fields: &[u32]) -> Result<RecordBatch> {
        let shard = self.memory.shard(key)?;
        let vessel = self
            .catalog
            .vessels
            .get(&key.vessel)
            .ok_or_else(|| Error::NotFound("vessel URN".into()))?;
        let ids: BTreeSet<_> = fields.iter().copied().collect();
        let selected: Vec<_> = self
            .catalog
            .fields
            .iter()
            .filter(|f| ids.contains(&f.id))
            .cloned()
            .collect();
        if selected.len() != ids.len() {
            return Err(Error::NotFound("projected field".into()));
        }
        let schema = ti_contracts::telemetry_schema(&selected)?;
        let mut arrays: Vec<ArrayRef> = vec![
            Arc::new(StringArray::from_iter_values(std::iter::repeat_n(
                vessel.urn.as_str(),
                cols.len() as usize,
            ))),
            Arc::new(
                TimestampSecondArray::from_iter_values(
                    cols.iter()
                        .map(|col| {
                            let bucket = ((key.shard as u64) << 16) | col as u64;
                            i64::try_from(
                                ti_contracts::EPOCH as i128
                                    + bucket as i128 * self.catalog.width_seconds as i128,
                            )
                            .map_err(|_| Error::Overflow("materialized timestamp"))
                        })
                        .collect::<Result<Vec<_>>>()?,
                )
                .with_timezone("UTC"),
            ),
        ];
        let mut ordered = selected;
        ordered.sort_by_key(|f| f.id);
        for spec in &ordered {
            let data = shard.field(spec.id)?;
            let array: ArrayRef = match data {
                FieldData::Presence(f) => Arc::new(BooleanArray::from(
                    cols.iter()
                        .map(|c| f.bitmap().contains(c).then_some(true))
                        .collect::<Vec<_>>(),
                )),
                FieldData::Bsi(f) => Arc::new(Float64Array::from(
                    f.values(cols)
                        .into_iter()
                        .map(|v| v.map(|v| v as f64 / 10f64.powi(f.scale() as i32)))
                        .collect::<Vec<_>>(),
                )),
                FieldData::Count(f) => Arc::new(UInt64Array::from(f.values(cols))),
                FieldData::Set(f) => {
                    let dictionary = self
                        .catalog
                        .dictionaries
                        .get(&spec.id)
                        .ok_or_else(|| Error::NotFound(format!("dictionary {}", spec.id)))?;
                    if f.is_multi() {
                        let mut builder = ListBuilder::new(StringBuilder::new());
                        for col in cols.iter() {
                            if !f.presence().contains(col) {
                                builder.append(false);
                                continue;
                            }
                            let mut values = f
                                .values(col)
                                .into_iter()
                                .map(|id| {
                                    dictionary.get(&id).cloned().ok_or_else(|| {
                                        Error::NotFound(format!("dictionary row {id}"))
                                    })
                                })
                                .collect::<Result<Vec<_>>>()?;
                            values.sort();
                            values.dedup();
                            for value in values {
                                builder.values().append_value(value);
                            }
                            builder.append(true);
                        }
                        // Frozen schemas use non-null list items.
                        builder = builder.with_field(Arc::new(
                            datafusion::arrow::datatypes::Field::new("item", DataType::Utf8, false),
                        ));
                        Arc::new(builder.finish())
                    } else {
                        let mut values = Vec::new();
                        for col in cols.iter() {
                            values.push(
                                f.values(col)
                                    .first()
                                    .map(|id| {
                                        dictionary.get(id).cloned().ok_or_else(|| {
                                            Error::NotFound(format!("dictionary row {id}"))
                                        })
                                    })
                                    .transpose()?,
                            );
                        }
                        Arc::new(StringArray::from(values))
                    }
                }
                FieldData::Geo(f) => {
                    let mut builder = ListBuilder::new(UInt64Builder::new()).with_field(Arc::new(
                        datafusion::arrow::datatypes::Field::new("item", DataType::UInt64, false),
                    ));
                    for col in cols.iter() {
                        if !f.presence().contains(col) {
                            builder.append(false);
                            continue;
                        }
                        for (cell, row) in f.rows() {
                            if row.contains(col) {
                                builder.values().append_value(*cell);
                            }
                        }
                        builder.append(true);
                    }
                    Arc::new(builder.finish())
                }
            };
            arrays.push(array.clone());
            if ti_contracts::mean_alias_enabled(spec, &ordered) {
                arrays.push(array);
            }
        }
        for _ in 0..3 {
            arrays.push(new_null_array(&DataType::Utf8, cols.len() as usize));
        }
        RecordBatch::try_new_with_options(
            schema,
            arrays,
            &RecordBatchOptions::new().with_row_count(Some(cols.len() as usize)),
        )
        .map_err(Error::from)
    }
}
impl ShardSource for FixtureSource {
    fn shards(&self, vessels: Option<&[VesselOrd]>, from: u32, to: u32) -> Vec<ShardKey> {
        self.memory.shards(vessels, from, to)
    }
    fn eval(&self, key: ShardKey, p: &Predicate) -> Result<RoaringBitmap> {
        self.memory.eval(key, p)
    }
    fn read(&self, key: ShardKey, cols: &RoaringBitmap, fields: &[u32]) -> Result<RecordBatch> {
        self.batch(key, cols, fields)
    }
    fn agg(
        &self,
        key: ShardKey,
        cols: &RoaringBitmap,
        field: u32,
        op: AggOp,
    ) -> Result<AggPartial> {
        self.memory.agg(key, cols, field, op)
    }
}
