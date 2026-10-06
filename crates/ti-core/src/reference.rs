//! Independent scalar reference, using ordinary Rust comparisons and explicit
//! SQL truth tables. No bitmap row operations, prefix comparisons, or core
//! evaluator helpers are used. Intended for fixtures and correctness checks.

use std::collections::BTreeMap;
use ti_contracts::{CmpOp, Error, Predicate, Result, ShardKey};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Truth {
    True,
    False,
    Unknown,
}
impl Truth {
    pub fn negate(self) -> Self {
        match self {
            Self::True => Self::False,
            Self::False => Self::True,
            Self::Unknown => Self::Unknown,
        }
    }
    pub fn and(self, other: Self) -> Self {
        match (self, other) {
            (Self::False, _) | (_, Self::False) => Self::False,
            (Self::True, Self::True) => Self::True,
            _ => Self::Unknown,
        }
    }
    pub fn or(self, other: Self) -> Self {
        match (self, other) {
            (Self::True, _) | (_, Self::True) => Self::True,
            (Self::False, Self::False) => Self::False,
            _ => Self::Unknown,
        }
    }
    fn boolean(value: bool) -> Self {
        if value {
            Self::True
        } else {
            Self::False
        }
    }
}

pub fn compare(value: i64, op: CmpOp, lo: i64, hi: Option<i64>) -> Result<bool> {
    Ok(match op {
        CmpOp::Eq => value == lo,
        CmpOp::Ne => value != lo,
        CmpOp::Lt => value < lo,
        CmpOp::Le => value <= lo,
        CmpOp::Gt => value > lo,
        CmpOp::Ge => value >= lo,
        CmpOp::Between => {
            value >= lo
                && value
                    <= hi.ok_or_else(|| Error::InvalidInput("BETWEEN needs upper bound".into()))?
        }
    })
}

#[derive(Debug, Clone)]
pub enum ScalarField {
    Presence(Vec<bool>),
    Bsi(Vec<Option<i64>>),
    Set(Vec<Option<u32>>),
    Sources(Vec<Option<Vec<u32>>>),
    Count(Vec<Option<u64>>),
}
impl ScalarField {
    fn present(&self, col: usize) -> bool {
        match self {
            Self::Presence(v) => v[col],
            Self::Bsi(v) => v[col].is_some(),
            Self::Set(v) => v[col].is_some(),
            Self::Sources(v) => v[col].is_some(),
            Self::Count(v) => v[col].is_some(),
        }
    }
}

/// Scalar universe is derived from field presence, not allocated array length.
#[derive(Debug, Clone)]
pub struct ScalarShard {
    pub key: ShardKey,
    pub columns: usize,
    pub fields: BTreeMap<u32, ScalarField>,
}
impl ScalarShard {
    pub fn new(key: ShardKey, columns: usize) -> Self {
        assert!(columns <= 65536);
        Self {
            key,
            columns,
            fields: BTreeMap::new(),
        }
    }
    pub fn universe(&self) -> Vec<bool> {
        (0..self.columns)
            .map(|col| self.fields.values().any(|f| f.present(col)))
            .collect()
    }
    pub fn eval(&self, p: &Predicate) -> Result<Vec<Option<Truth>>> {
        let u = self.universe();
        (0..self.columns)
            .map(|col| {
                if u[col] {
                    self.at(p, col).map(Some)
                } else {
                    Ok(None)
                }
            })
            .collect()
    }
    fn field(&self, id: u32) -> Result<&ScalarField> {
        self.fields
            .get(&id)
            .ok_or_else(|| Error::NotFound(format!("field {id}")))
    }
    fn at(&self, p: &Predicate, col: usize) -> Result<Truth> {
        Ok(match p {
            Predicate::All => Truth::True,
            Predicate::None => Truth::False,
            Predicate::Present(id) => Truth::boolean(self.field(*id)?.present(col)),
            Predicate::SetEq {
                field,
                rows,
                negate,
            } => {
                let matches = match self.field(*field)? {
                    ScalarField::Set(v) => v[col].map(|value| rows.contains(&value)),
                    ScalarField::Sources(v) => v[col]
                        .as_ref()
                        .map(|values| values.iter().any(|value| rows.contains(value))),
                    _ => return Err(Error::InvalidInput("SetEq field".into())),
                };
                matches
                    .map(|m| Truth::boolean(if *negate { !m } else { m }))
                    .unwrap_or(Truth::Unknown)
            }
            Predicate::BsiCmp { field, op, lo, hi } => match self.field(*field)? {
                ScalarField::Bsi(v) => match v[col] {
                    Some(v) => Truth::boolean(compare(v, op.clone(), *lo, *hi)?),
                    None => Truth::Unknown,
                },
                ScalarField::Count(v) => match v[col] {
                    Some(v) => {
                        let v = v as i128;
                        let lo = *lo as i128;
                        Truth::boolean(match op {
                            CmpOp::Eq => v == lo,
                            CmpOp::Ne => v != lo,
                            CmpOp::Lt => v < lo,
                            CmpOp::Le => v <= lo,
                            CmpOp::Gt => v > lo,
                            CmpOp::Ge => v >= lo,
                            CmpOp::Between => {
                                v >= lo
                                    && v <= hi.ok_or_else(|| {
                                        Error::InvalidInput("BETWEEN upper".into())
                                    })? as i128
                            }
                        })
                    }
                    None => Truth::Unknown,
                },
                _ => return Err(Error::InvalidInput("BsiCmp field".into())),
            },
            Predicate::TsRange { from, to } => {
                let bucket = ((self.key.shard as u64) << 16) + col as u64;
                Truth::boolean(bucket >= *from as u64 && bucket <= *to as u64)
            }
            Predicate::Not(child) => self.at(child, col)?.negate(),
            Predicate::And(children) => {
                let mut out = Truth::True;
                for child in children {
                    out = out.and(self.at(child, col)?);
                }
                out
            }
            Predicate::Or(children) => {
                let mut out = Truth::False;
                for child in children {
                    out = out.or(self.at(child, col)?);
                }
                out
            }
            Predicate::Text { .. } | Predicate::GeoCover { .. } => {
                return Err(Error::Unsupported(
                    "reference uses injected fixture assertions for text/geo".into(),
                ))
            }
        })
    }
}

/// Scalar aggregate oracle, independent of bit slices.
pub fn aggregate(
    values: &[Option<i64>],
    selected: &[bool],
) -> (i128, Option<i64>, Option<i64>, u64) {
    assert_eq!(values.len(), selected.len());
    let samples: Vec<_> = values
        .iter()
        .zip(selected)
        .filter_map(|(v, keep)| if *keep { *v } else { None })
        .collect();
    (
        samples.iter().map(|v| *v as i128).sum(),
        samples.iter().min().copied(),
        samples.iter().max().copied(),
        samples.len() as u64,
    )
}
