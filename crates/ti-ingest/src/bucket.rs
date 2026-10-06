//! Bucketing accumulators and record synthesis.
//!
//! Enforces:
//! - Per-(vessel, path) accumulators: count, sum, min, max, last, sources
//! - Aggregate profiles: default (@mean/@min/@max), slow (@last), opt-in
//! - Ordinary set field single-value preference (D21)
//! - Multi-valued source set fields (`path$source`)

use std::collections::{BTreeMap, BTreeSet};
use ti_contracts::{
    to_fixed, Agg, BucketIx, BucketRecord, Catalog, FieldKind, FieldSpec, FieldValue, Result,
    TiConfig, VesselOrd,
};

/// Resolve aggregate column names for a numeric path.
/// 1. Exact match in `store_aggs`
/// 2. Glob match in `store_aggs`
/// 3. Fallback to `profiles.default` (+ `profiles.opt_in`)
pub fn resolve_aggs_for_path(
    path: &str,
    store_aggs: &BTreeMap<String, Vec<String>>,
    default_profiles: &ti_contracts::AggregateProfiles,
) -> Vec<String> {
    if !store_aggs.is_empty() {
        if let Some(aggs) = store_aggs.get(path) {
            return aggs.clone();
        }
        for (pattern, aggs) in store_aggs {
            if crate::normalize::matches_glob(pattern, path) {
                return aggs.clone();
            }
        }
    }

    let mut aggs_to_emit: Vec<String> = default_profiles.default.clone();
    for agg in &default_profiles.opt_in {
        if !aggs_to_emit.contains(agg) {
            aggs_to_emit.push(agg.clone());
        }
    }
    aggs_to_emit
}

#[derive(Debug, Clone)]
pub struct NumericAcc {
    pub count: u64,
    pub sum: f64,
    pub min: f64,
    pub max: f64,
    pub last: f64,
    pub first_ts: i64,
    pub last_ts: i64,
    pub scale: u8,
    pub sources: BTreeSet<String>,
}

#[derive(Debug, Clone)]
pub struct SetAcc {
    pub winning_value: String,
    pub winning_priority: usize,
    pub winning_ts: i64,
    pub sources: BTreeSet<String>,
}

#[derive(Debug, Clone, Default)]
pub struct CountAcc {
    pub count: u64,
    pub sources: BTreeSet<String>,
}

#[derive(Debug, Clone, Default)]
pub struct GeoAcc {
    pub cells: BTreeSet<u64>,
    pub sources: BTreeSet<String>,
}

#[derive(Debug, Clone, Default)]
pub struct BucketWindow {
    pub numeric: BTreeMap<String, NumericAcc>,
    pub set: BTreeMap<String, SetAcc>,
    pub count: BTreeMap<String, CountAcc>,
    pub geo: BTreeMap<String, GeoAcc>,
}

impl BucketWindow {
    pub fn is_empty(&self) -> bool {
        self.numeric.is_empty()
            && self.set.is_empty()
            && self.count.is_empty()
            && self.geo.is_empty()
    }

    pub fn add_numeric(&mut self, path: &str, value: f64, scale: u8, source: &str, ts: i64) {
        let acc = self.numeric.entry(path.to_string()).or_insert(NumericAcc {
            count: 0,
            sum: 0.0,
            min: value,
            max: value,
            last: value,
            first_ts: ts,
            last_ts: ts,
            scale,
            sources: BTreeSet::new(),
        });

        acc.count += 1;
        acc.sum += value;
        if value < acc.min {
            acc.min = value;
        }
        if value > acc.max {
            acc.max = value;
        }
        if ts >= acc.last_ts {
            acc.last = value;
            acc.last_ts = ts;
        }
        acc.sources.insert(source.to_string());
    }

    pub fn add_set(&mut self, path: &str, value: &str, priority: usize, source: &str, ts: i64) {
        let acc = self.set.entry(path.to_string()).or_insert_with(|| SetAcc {
            winning_value: value.to_string(),
            winning_priority: priority,
            winning_ts: ts,
            sources: BTreeSet::new(),
        });

        acc.sources.insert(source.to_string());

        // Lower priority number = higher precedence (D21)
        if priority < acc.winning_priority
            || (priority == acc.winning_priority && ts >= acc.winning_ts)
        {
            acc.winning_value = value.to_string();
            acc.winning_priority = priority;
            acc.winning_ts = ts;
        }
    }

    pub fn add_count(&mut self, path: &str, count: u64, source: &str) {
        let acc = self.count.entry(path.to_string()).or_default();
        acc.count += count;
        acc.sources.insert(source.to_string());
    }

    pub fn add_geo_cell(&mut self, path: &str, cell: u64, source: &str) {
        let acc = self.geo.entry(path.to_string()).or_default();
        acc.cells.insert(cell);
        acc.sources.insert(source.to_string());
    }

    /// Synthesize all accumulated data in this window into `BucketRecord`s.
    pub fn emit_records(
        &self,
        vessel: VesselOrd,
        bucket: BucketIx,
        rewrite: bool,
        config: &TiConfig,
        catalog: &dyn Catalog,
    ) -> Result<Vec<BucketRecord>> {
        self.emit_records_with_aggs(vessel, bucket, rewrite, None, config, catalog)
    }

    /// Synthesize all accumulated data in this window into `BucketRecord`s, using custom per-path aggregates if provided.
    pub fn emit_records_with_aggs(
        &self,
        vessel: VesselOrd,
        bucket: BucketIx,
        rewrite: bool,
        store_aggs: Option<&BTreeMap<String, Vec<String>>>,
        config: &TiConfig,
        catalog: &dyn Catalog,
    ) -> Result<Vec<BucketRecord>> {
        let mut records = Vec::new();

        // 1. Numeric accumulators
        for (path, acc) in &self.numeric {
            if acc.count == 0 {
                continue;
            }

            let aggs_to_emit = match store_aggs {
                Some(aggs) => resolve_aggs_for_path(path, aggs, &config.profiles),
                None => {
                    let mut a = config.profiles.default.clone();
                    for agg in &config.profiles.opt_in {
                        if !a.contains(agg) {
                            a.push(agg.clone());
                        }
                    }
                    a
                }
            };

            for agg_name in &aggs_to_emit {
                match agg_name.as_str() {
                    "mean" => {
                        let mean_val = acc.sum / (acc.count as f64);
                        let fixed = to_fixed(mean_val, acc.scale)?;
                        let field_id = catalog.register_field(&FieldSpec {
                            id: 0,
                            path: path.clone(),
                            agg: Some(Agg::Mean),
                            kind: FieldKind::Bsi { scale: acc.scale },
                            units: None,
                        })?;
                        records.push(BucketRecord {
                            vessel,
                            bucket,
                            field: field_id,
                            value: FieldValue::Int(fixed),
                            rewrite,
                        });
                    }
                    "min" => {
                        let fixed = to_fixed(acc.min, acc.scale)?;
                        let field_id = catalog.register_field(&FieldSpec {
                            id: 0,
                            path: path.clone(),
                            agg: Some(Agg::Min),
                            kind: FieldKind::Bsi { scale: acc.scale },
                            units: None,
                        })?;
                        records.push(BucketRecord {
                            vessel,
                            bucket,
                            field: field_id,
                            value: FieldValue::Int(fixed),
                            rewrite,
                        });
                    }
                    "max" => {
                        let fixed = to_fixed(acc.max, acc.scale)?;
                        let field_id = catalog.register_field(&FieldSpec {
                            id: 0,
                            path: path.clone(),
                            agg: Some(Agg::Max),
                            kind: FieldKind::Bsi { scale: acc.scale },
                            units: None,
                        })?;
                        records.push(BucketRecord {
                            vessel,
                            bucket,
                            field: field_id,
                            value: FieldValue::Int(fixed),
                            rewrite,
                        });
                    }
                    "last" => {
                        let fixed = to_fixed(acc.last, acc.scale)?;
                        let field_id = catalog.register_field(&FieldSpec {
                            id: 0,
                            path: path.clone(),
                            agg: Some(Agg::Last),
                            kind: FieldKind::Bsi { scale: acc.scale },
                            units: None,
                        })?;
                        records.push(BucketRecord {
                            vessel,
                            bucket,
                            field: field_id,
                            value: FieldValue::Int(fixed),
                            rewrite,
                        });
                    }
                    "count" => {
                        let field_id = catalog.register_field(&FieldSpec {
                            id: 0,
                            path: path.clone(),
                            agg: Some(Agg::Count),
                            kind: FieldKind::Count,
                            units: None,
                        })?;
                        records.push(BucketRecord {
                            vessel,
                            bucket,
                            field: field_id,
                            value: FieldValue::Int(acc.count as i64),
                            rewrite,
                        });
                    }
                    _ => {}
                }
            }

            // Emit path$source multi-valued set
            let source_path = format!("{path}$source");
            let source_field = catalog.register_field(&FieldSpec {
                id: 0,
                path: source_path,
                agg: None,
                kind: FieldKind::Set,
                units: None,
            })?;
            for src in &acc.sources {
                let row_id = catalog.register_set_value(source_field, src)?;
                records.push(BucketRecord {
                    vessel,
                    bucket,
                    field: source_field,
                    value: FieldValue::SetValue(row_id),
                    rewrite,
                });
            }
        }

        // 2. Set accumulators (ordinary set single-valued per D21)
        for (path, acc) in &self.set {
            let field_id = catalog.register_field(&FieldSpec {
                id: 0,
                path: path.clone(),
                agg: None,
                kind: FieldKind::Set,
                units: None,
            })?;
            let row_id = catalog.register_set_value(field_id, &acc.winning_value)?;
            records.push(BucketRecord {
                vessel,
                bucket,
                field: field_id,
                value: FieldValue::SetValue(row_id),
                rewrite,
            });

            // Emit path$source
            let source_path = format!("{path}$source");
            let source_field = catalog.register_field(&FieldSpec {
                id: 0,
                path: source_path,
                agg: None,
                kind: FieldKind::Set,
                units: None,
            })?;
            for src in &acc.sources {
                let s_row_id = catalog.register_set_value(source_field, src)?;
                records.push(BucketRecord {
                    vessel,
                    bucket,
                    field: source_field,
                    value: FieldValue::SetValue(s_row_id),
                    rewrite,
                });
            }
        }

        // 3. Count accumulators
        for (path, acc) in &self.count {
            let field_id = catalog.register_field(&FieldSpec {
                id: 0,
                path: path.clone(),
                agg: None,
                kind: FieldKind::Count,
                units: None,
            })?;
            records.push(BucketRecord {
                vessel,
                bucket,
                field: field_id,
                value: FieldValue::Int(acc.count as i64),
                rewrite,
            });
        }

        // 4. Geo accumulators
        for (path, acc) in &self.geo {
            let field_id = catalog.register_field(&FieldSpec {
                id: 0,
                path: path.clone(),
                agg: None,
                kind: FieldKind::Geo { res: 7 },
                units: None,
            })?;
            records.push(BucketRecord {
                vessel,
                bucket,
                field: field_id,
                value: FieldValue::Cells(acc.cells.iter().cloned().collect()),
                rewrite,
            });
        }

        Ok(records)
    }
}
