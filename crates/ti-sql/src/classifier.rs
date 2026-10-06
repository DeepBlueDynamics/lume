//! Per-expression pushdown with SQL null preservation and conservative shard pruning.
//! Numeric literals pass through the shared fixed-point converter. Integer range
//! boundaries account for Float64 reconstruction collisions, so Exact filters
//! agree with the physical values exposed by the frozen Arrow schema.

use crate::catalog::SqlCatalog;
use datafusion::arrow::datatypes::{DataType, TimeUnit};
use datafusion::common::{Result, ScalarValue};
use datafusion::logical_expr::{Expr, Operator, TableProviderFilterPushDown};
use std::sync::Arc;
use ti_contracts::{CmpOp, FieldKind, Predicate, ShardKey};

#[derive(Debug, Clone)]
pub enum PlannedPredicate {
    Bitmap(Predicate),
    Vessel(Vec<u32>),
    And(Vec<Self>),
    Or(Vec<Self>),
    Not(Box<Self>),
}
impl PlannedPredicate {
    pub fn for_shard(&self, key: ShardKey) -> Predicate {
        match self {
            Self::Bitmap(p) => p.clone(),
            Self::Vessel(v) => {
                if v.contains(&key.vessel) {
                    Predicate::All
                } else {
                    Predicate::None
                }
            }
            Self::And(v) => Predicate::And(v.iter().map(|p| p.for_shard(key)).collect()),
            Self::Or(v) => Predicate::Or(v.iter().map(|p| p.for_shard(key)).collect()),
            Self::Not(p) => Predicate::Not(Box::new(p.for_shard(key))),
        }
    }
    /// Conservative proof that a shard cannot match. OR retains either child's
    /// candidates; NOT is never pruned by complementing a possible bitmap hit.
    pub fn may_match(&self, key: ShardKey) -> bool {
        match self {
            Self::Vessel(v) => v.contains(&key.vessel),
            Self::Bitmap(Predicate::None) => false,
            Self::Bitmap(Predicate::TsRange { from, to }) => {
                from <= to && key.shard >= from >> 16 && key.shard <= to >> 16
            }
            Self::And(v) => v.iter().all(|p| p.may_match(key)),
            Self::Or(v) => v.iter().any(|p| p.may_match(key)),
            _ => true,
        }
    }
}
#[derive(Debug, Clone)]
pub struct Classification {
    pub class: TableProviderFilterPushDown,
    pub predicate: Option<PlannedPredicate>,
    pub reason: String,
}
fn unsupported(reason: impl Into<String>) -> Classification {
    Classification {
        class: TableProviderFilterPushDown::Unsupported,
        predicate: None,
        reason: reason.into(),
    }
}
fn exact(p: PlannedPredicate) -> Classification {
    Classification {
        class: TableProviderFilterPushDown::Exact,
        predicate: Some(p),
        reason: "exact bitmap translation".into(),
    }
}
fn bitmap(p: Predicate) -> Classification {
    exact(PlannedPredicate::Bitmap(p))
}
fn column(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::Column(c) => Some(&c.name),
        Expr::ScalarFunction(f) if f.name() == "ti_timestamp" && f.args.len() == 1 => {
            column(&f.args[0])
        }
        Expr::Cast(c) if matches!(c.field.data_type(), DataType::Timestamp(_, _)) => {
            column(&c.expr)
        }
        _ => None,
    }
}
fn literal(expr: &Expr) -> Option<ScalarValue> {
    match expr {
        Expr::Literal(v, _) => Some(v.clone()),
        Expr::Cast(c) => literal(&c.expr)?.cast_to(c.field.data_type()).ok(),
        Expr::Negative(e) => match literal(e)? {
            ScalarValue::Int64(Some(v)) => v.checked_neg().map(|v| ScalarValue::Int64(Some(v))),
            ScalarValue::Float64(Some(v)) => Some(ScalarValue::Float64(Some(-v))),
            _ => None,
        },
        _ => None,
    }
}
fn string(v: &ScalarValue) -> Option<String> {
    match v {
        ScalarValue::Utf8(Some(s))
        | ScalarValue::LargeUtf8(Some(s))
        | ScalarValue::Utf8View(Some(s)) => Some(s.clone()),
        _ => None,
    }
}
fn numeric(v: &ScalarValue) -> Option<f64> {
    match v {
        ScalarValue::Float64(Some(v)) => Some(*v),
        ScalarValue::Float32(Some(v)) => Some(*v as f64),
        ScalarValue::Int64(Some(v)) => Some(*v as f64),
        ScalarValue::Int32(Some(v)) => Some(*v as f64),
        ScalarValue::UInt64(Some(v)) => Some(*v as f64),
        ScalarValue::UInt32(Some(v)) => Some(*v as f64),
        _ => None,
    }
}
fn operation(op: Operator) -> Option<CmpOp> {
    match op {
        Operator::Eq => Some(CmpOp::Eq),
        Operator::NotEq => Some(CmpOp::Ne),
        Operator::Lt => Some(CmpOp::Lt),
        Operator::LtEq => Some(CmpOp::Le),
        Operator::Gt => Some(CmpOp::Gt),
        Operator::GtEq => Some(CmpOp::Ge),
        _ => None,
    }
}
fn reverse(op: CmpOp) -> CmpOp {
    match op {
        CmpOp::Lt => CmpOp::Gt,
        CmpOp::Le => CmpOp::Ge,
        CmpOp::Gt => CmpOp::Lt,
        CmpOp::Ge => CmpOp::Le,
        p => p,
    }
}
fn bounded_range(low: i128, high: i128) -> Predicate {
    if low > high || high < 0 || low > u32::MAX as i128 {
        Predicate::None
    } else {
        Predicate::TsRange {
            from: low.max(0) as u32,
            to: high.min(u32::MAX as i128) as u32,
        }
    }
}
fn timestamp_ns(v: &ScalarValue) -> Option<i128> {
    match v {
        ScalarValue::TimestampSecond(Some(t), _) => Some(*t as i128 * 1_000_000_000),
        ScalarValue::TimestampMillisecond(Some(t), _) => Some(*t as i128 * 1_000_000),
        ScalarValue::TimestampMicrosecond(Some(t), _) => Some(*t as i128 * 1000),
        ScalarValue::TimestampNanosecond(Some(t), _) => Some(*t as i128),
        v if string(v).is_some() => timestamp_ns(
            &v.cast_to(&DataType::Timestamp(
                TimeUnit::Nanosecond,
                Some("UTC".into()),
            ))
            .ok()?,
        ),
        _ => None,
    }
}
fn time_compare(width: u64, op: CmpOp, value: &ScalarValue) -> Option<Predicate> {
    let elapsed = timestamp_ns(value)? - ti_contracts::EPOCH as i128 * 1_000_000_000;
    let width = width as i128 * 1_000_000_000;
    let floor = elapsed.div_euclid(width);
    let ceil = floor + i128::from(elapsed.rem_euclid(width) != 0);
    Some(match op {
        CmpOp::Eq => {
            if floor == ceil {
                bounded_range(floor, floor)
            } else {
                Predicate::None
            }
        }
        CmpOp::Ne => Predicate::Not(Box::new(if floor == ceil {
            bounded_range(floor, floor)
        } else {
            Predicate::None
        })),
        CmpOp::Lt => bounded_range(0, ceil - 1),
        CmpOp::Le => bounded_range(0, floor),
        CmpOp::Gt => bounded_range(floor + 1, u32::MAX as i128),
        CmpOp::Ge => bounded_range(ceil, u32::MAX as i128),
        CmpOp::Between => return None,
    })
}
/// First integer whose reconstructed physical value is >= target (or > target).
fn float_boundary(scale: u8, target: f64, strict: bool) -> i128 {
    let mut lo = i64::MIN as i128;
    let mut hi = i64::MAX as i128 + 1;
    let factor = 10f64.powi(scale as i32);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let value = mid as i64 as f64 / factor;
        if if strict {
            value > target
        } else {
            value >= target
        } {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    lo
}
fn numeric_range(field: u32, lo: i128, hi: i128) -> Predicate {
    if lo > hi || lo > i64::MAX as i128 || hi < i64::MIN as i128 {
        return Predicate::BsiCmp {
            field,
            op: CmpOp::Between,
            lo: 1,
            hi: Some(0),
        };
    }
    Predicate::BsiCmp {
        field,
        op: CmpOp::Between,
        lo: lo.max(i64::MIN as i128) as i64,
        hi: Some(hi.min(i64::MAX as i128) as i64),
    }
}
fn bsi_compare(field: u32, scale: u8, op: CmpOp, value: f64) -> Option<Predicate> {
    let fixed = ti_contracts::to_fixed(value, scale).ok()?;
    let target = ti_contracts::from_fixed(fixed, scale).ok()?;
    let ge = float_boundary(scale, target, false);
    let gt = float_boundary(scale, target, true);
    Some(match op {
        CmpOp::Eq => numeric_range(field, ge, gt - 1),
        CmpOp::Ne => Predicate::Not(Box::new(numeric_range(field, ge, gt - 1))),
        CmpOp::Lt => numeric_range(field, i64::MIN as i128, ge - 1),
        CmpOp::Le => numeric_range(field, i64::MIN as i128, gt - 1),
        CmpOp::Gt => numeric_range(field, gt, i64::MAX as i128),
        CmpOp::Ge => numeric_range(field, ge, i64::MAX as i128),
        CmpOp::Between => return None,
    })
}

#[derive(Debug, Clone)]
pub struct PushdownClassifier {
    pub catalog: Arc<SqlCatalog>,
}
impl PushdownClassifier {
    pub fn classify(&self, expr: &Expr) -> Result<Classification> {
        Ok(match expr {
            Expr::BinaryExpr(b) if matches!(b.op, Operator::And | Operator::Or) => {
                let l = self.classify(&b.left)?;
                let r = self.classify(&b.right)?;
                if l.class == TableProviderFilterPushDown::Unsupported
                    || r.class == TableProviderFilterPushDown::Unsupported
                {
                    unsupported("compound includes unsupported expression")
                } else {
                    let class = if l.class == TableProviderFilterPushDown::Inexact
                        || r.class == TableProviderFilterPushDown::Inexact
                    {
                        TableProviderFilterPushDown::Inexact
                    } else {
                        TableProviderFilterPushDown::Exact
                    };
                    let children = vec![
                        l.predicate.expect("supported"),
                        r.predicate.expect("supported"),
                    ];
                    Classification {
                        class,
                        predicate: Some(if b.op == Operator::And {
                            PlannedPredicate::And(children)
                        } else {
                            PlannedPredicate::Or(children)
                        }),
                        reason: "compound class propagates children".into(),
                    }
                }
            }
            Expr::Not(child) => {
                let c = self.classify(child)?;
                if c.class != TableProviderFilterPushDown::Exact {
                    unsupported("NOT cannot complement inexact/unsupported expression")
                } else {
                    exact(PlannedPredicate::Not(Box::new(c.predicate.expect("exact"))))
                }
            }
            Expr::IsNull(child) | Expr::IsNotNull(child) => {
                let Some(name) = column(child) else {
                    return Ok(unsupported("null test is not a column"));
                };
                let p = if name == "vessel" || name == "ts" {
                    Predicate::All
                } else if let Some(f) = self.catalog.field(name) {
                    Predicate::Present(f.id)
                } else {
                    return Ok(unsupported("virtual column null test"));
                };
                bitmap(if matches!(expr, Expr::IsNull(_)) {
                    Predicate::Not(Box::new(p))
                } else {
                    p
                })
            }
            Expr::BinaryExpr(b) => {
                let Some(mut op) = operation(b.op) else {
                    return Ok(unsupported("arithmetic or non-comparison operator"));
                };
                let pair = if let (Some(c), Some(v)) = (column(&b.left), literal(&b.right)) {
                    Some((c, v))
                } else if let (Some(c), Some(v)) = (column(&b.right), literal(&b.left)) {
                    op = reverse(op);
                    Some((c, v))
                } else {
                    None
                };
                match pair {
                    Some((name, value)) => self.comparison(name, op, &value),
                    None => unsupported("comparison is not column versus literal"),
                }
            }
            Expr::Between(b) => {
                let Some(name) = column(&b.expr) else {
                    return Ok(unsupported("BETWEEN is not column"));
                };
                let (Some(low), Some(high)) = (literal(&b.low), literal(&b.high)) else {
                    return Ok(unsupported("BETWEEN bounds are not literals"));
                };
                let l = self.comparison(name, CmpOp::Ge, &low);
                let r = self.comparison(name, CmpOp::Le, &high);
                if l.class != TableProviderFilterPushDown::Exact
                    || r.class != TableProviderFilterPushDown::Exact
                {
                    unsupported("BETWEEN bounds unsupported")
                } else {
                    let p = PlannedPredicate::And(vec![
                        l.predicate.expect("exact"),
                        r.predicate.expect("exact"),
                    ]);
                    exact(if b.negated {
                        PlannedPredicate::Not(Box::new(p))
                    } else {
                        p
                    })
                }
            }
            Expr::InList(list) => {
                let Some(name) = column(&list.expr) else {
                    return Ok(unsupported("IN is not a column"));
                };
                let values: Option<Vec<_>> = list
                    .list
                    .iter()
                    .map(|e| literal(e).filter(|v| !v.is_null()))
                    .collect();
                let Some(values) = values else {
                    return Ok(unsupported("nullable/nonliteral IN stays in DataFusion"));
                };
                let items: Vec<_> = values
                    .iter()
                    .map(|v| self.comparison(name, CmpOp::Eq, v))
                    .collect();
                if items
                    .iter()
                    .any(|p| p.class != TableProviderFilterPushDown::Exact)
                {
                    unsupported("IN values unsupported")
                } else {
                    let p = PlannedPredicate::Or(
                        items
                            .into_iter()
                            .map(|p| p.predicate.expect("exact"))
                            .collect(),
                    );
                    exact(if list.negated {
                        PlannedPredicate::Not(Box::new(p))
                    } else {
                        p
                    })
                }
            }
            Expr::ScalarFunction(f) if f.name() == "array_has" || f.name() == "array_contains" => {
                if f.args.len() != 2 {
                    return Ok(unsupported("array membership arity"));
                }
                match (
                    column(&f.args[0]),
                    literal(&f.args[1]).and_then(|v| string(&v)),
                ) {
                    (Some(name), Some(value)) if self.catalog.source_column(name) => {
                        self.comparison(name, CmpOp::Eq, &ScalarValue::Utf8(Some(value)))
                    }
                    _ => unsupported("array membership is not a source column versus string"),
                }
            }
            Expr::ScalarFunction(f) if f.name() == "ti_match" => {
                if f.args.len() != 4
                    || column(&f.args[0]) != Some("vessel")
                    || column(&f.args[1]) != Some("ts")
                {
                    return Ok(unsupported("ti_match row identity"));
                }
                match (
                    literal(&f.args[2]).and_then(|v| string(&v)),
                    literal(&f.args[3]).and_then(|v| string(&v)),
                ) {
                    (Some(kind), Some(query))
                        if ["notes", "logbook", "alerts"].contains(&kind.as_str()) =>
                    {
                        bitmap(Predicate::Text { kind, query })
                    }
                    _ => unsupported("ti_match requires literal kind/query"),
                }
            }
            Expr::ScalarFunction(f) if f.name() == "match" => {
                if f.args.len() != 2 {
                    return Ok(unsupported("match arity"));
                }
                match (
                    column(&f.args[0]),
                    literal(&f.args[1]).and_then(|v| string(&v)),
                ) {
                    (Some(kind @ ("notes" | "logbook" | "alerts")), Some(query)) => {
                        bitmap(Predicate::Text {
                            kind: kind.into(),
                            query,
                        })
                    }
                    _ => unsupported("match needs a virtual text column and literal query"),
                }
            }
            Expr::ScalarFunction(f) if matches!(f.name(), "ti_in_bbox" | "ti_within_nm") => {
                self.geo(f.name(), &f.args)
            }
            Expr::Literal(ScalarValue::Boolean(Some(v)), _) => {
                bitmap(if *v { Predicate::All } else { Predicate::None })
            }
            _ => unsupported("expression is outside the frozen bitmap IR"),
        })
    }
    fn geo(&self, name: &str, args: &[Expr]) -> Classification {
        let arity = if name == "ti_in_bbox" { 6 } else { 5 };
        if args.len() != arity {
            return unsupported("geo arity");
        }
        let Some(lat) = column(&args[0]).and_then(|n| self.catalog.field(n)) else {
            return unsupported("geo latitude is not indexed");
        };
        let Some(lon) = column(&args[1]).and_then(|n| self.catalog.field(n)) else {
            return unsupported("geo longitude is not indexed");
        };
        if lat.path != "navigation.position.latitude" || lon.path != "navigation.position.longitude" {
            return unsupported("geo coordinates are not the indexed position");
        }
        let values = args[2..].iter().map(|a| literal(a).and_then(|v| numeric(&v))).collect::<Option<Vec<_>>>();
        let Some(v) = values else {
            return unsupported("geo bounds must be non-null numeric literals");
        };
        let bbox = if name == "ti_in_bbox" {
            ti_geo::Bbox::new(v[0], v[1], v[2], v[3])
        } else {
            ti_geo::radius_bbox(v[0], v[1], v[2])
        };
        let Ok(bbox) = bbox else { return unsupported("invalid geo bounds remain residual"); };
        let bound = |field: &ti_contracts::FieldSpec, op, value| {
            let FieldKind::Bsi { scale } = field.kind else { return None; };
            bsi_compare(field.id, scale, op, value)
        };
        let Some(lat_low) = bound(lat, CmpOp::Ge, bbox.lat_min) else { return unsupported("geo latitude bound"); };
        let Some(lat_high) = bound(lat, CmpOp::Le, bbox.lat_max) else { return unsupported("geo latitude bound"); };
        let mut longitude = Vec::new();
        for (low, high) in bbox.longitude_spans() {
            let (Some(low_p), Some(high_p)) = (bound(lon, CmpOp::Ge, low), bound(lon, CmpOp::Le, high)) else {
                return unsupported("geo longitude bound");
            };
            longitude.push(Predicate::And(vec![low_p, high_p]));
            // +180 and -180 name the same meridian, including on stored rows.
            if low == -180.0 { longitude.push(bound(lon, CmpOp::Eq, 180.0).expect("valid bound")); }
            if high == 180.0 { longitude.push(bound(lon, CmpOp::Eq, -180.0).expect("valid bound")); }
        }
        let envelope = Predicate::And(vec![lat_low, lat_high, Predicate::Or(longitude)]);
        let mut candidates = vec![envelope];
        let mut reason = "geo BSI safety envelope (no H3 field); exact latitude/longitude residual".to_string();
        if let Some(field) = self.catalog.fields.iter().find(|f| f.path == "navigation.position" && matches!(f.kind, FieldKind::Geo { .. })) {
            let cells = if name == "ti_in_bbox" {
                ti_geo::bbox_cover(v[0], v[1], v[2], v[3])
            } else {
                ti_geo::radius_cover(v[0], v[1], v[2])
            };
            // The BSI envelope independently proves candidate completeness if
            // cover construction fails or the store contains only legacy cells.
            match cells {
                Ok(cells) => {
                    candidates.push(Predicate::GeoCover { field: field.id, cells });
                    reason = "geo H3 cover OR BSI safety envelope; exact latitude/longitude residual".into();
                }
                Err(error) => {
                    reason = format!("geo cover unavailable ({error}); BSI safety envelope and exact residual");
                }
            }
        }
        Classification {
            class: TableProviderFilterPushDown::Inexact,
            predicate: Some(PlannedPredicate::Bitmap(Predicate::Or(candidates))),
            reason,
        }
    }
    fn comparison(&self, name: &str, op: CmpOp, value: &ScalarValue) -> Classification {
        if value.is_null() {
            return unsupported("null literal preserves SQL UNKNOWN in DataFusion");
        }
        if name == "vessel" {
            let Some(value) = string(value) else {
                return unsupported("vessel requires string literal");
            };
            if !matches!(op, CmpOp::Eq | CmpOp::Ne) {
                return unsupported("vessel ordering unsupported");
            }
            let ids = self
                .catalog
                .vessels
                .iter()
                .filter_map(|(id, v)| (v.urn == value).then_some(*id))
                .collect();
            let p = PlannedPredicate::Vessel(ids);
            return exact(if op == CmpOp::Ne {
                PlannedPredicate::Not(Box::new(p))
            } else {
                p
            });
        }
        if name == "ts" {
            return time_compare(self.catalog.width_seconds, op, value)
                .map(bitmap)
                .unwrap_or_else(|| unsupported("timestamp literal is not parseable"));
        }
        let Some(field) = self.catalog.field(name) else {
            return unsupported("unregistered/virtual column comparison");
        };
        match field.kind {
            FieldKind::Set if matches!(op, CmpOp::Eq | CmpOp::Ne) => {
                let Some(value) = string(value) else {
                    return unsupported("set comparison requires string");
                };
                bitmap(Predicate::SetEq {
                    field: field.id,
                    rows: self.catalog.row_ids(field.id, &[value]),
                    negate: op == CmpOp::Ne,
                })
            }
            FieldKind::Bsi { scale } => numeric(value)
                .and_then(|v| bsi_compare(field.id, scale, op, v))
                .map(bitmap)
                .unwrap_or_else(|| unsupported("numeric literal outside fixed-point domain")),
            FieldKind::Count => match value {
                ScalarValue::Int64(Some(v)) => bitmap(Predicate::BsiCmp {
                    field: field.id,
                    op,
                    lo: *v,
                    hi: None,
                }),
                ScalarValue::UInt64(Some(v)) if *v <= i64::MAX as u64 => {
                    bitmap(Predicate::BsiCmp {
                        field: field.id,
                        op,
                        lo: *v as i64,
                        hi: None,
                    })
                }
                _ => unsupported("count literal requires exact signed-integer IR representation"),
            },
            _ => unsupported("field encoding/operator not supported"),
        }
    }
}
