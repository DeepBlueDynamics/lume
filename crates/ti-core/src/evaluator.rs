//! Original evaluator: SQL Kleene masks (AND intersects true/unions false;
//! OR unions true/intersects false; NOT swaps masks). Unknown is the remainder.
//! Atomic in-memory apply stages a clone, validates groups, then replaces state.
//! This is a W1 fixture/core, not a durable sink. Geo covers carry inexactness;
//! any enclosing NOT is rejected rather than complementing a conservative cover.

use crate::{BsiField, CountField, PresenceRow, SetField, SHARD_COLUMNS};
use std::collections::BTreeMap;
use std::sync::Arc;
use ti_contracts::{
    AggOp, AggPartial, BucketRecord, Error, FieldKind, FieldSpec, FieldValue, Predicate,
    RecordBatch, Result, RoaringBitmap, ShardKey, ShardSource, TextIndex, VesselOrd,
};

fn contains_geo(p: &Predicate) -> bool {
    match p {
        Predicate::GeoCover { .. } => true,
        Predicate::Not(child) => contains_geo(child),
        Predicate::And(children) | Predicate::Or(children) => children.iter().any(contains_geo),
        _ => false,
    }
}
fn validate_negation(p: &Predicate) -> Result<()> {
    match p {
        Predicate::Not(child) => {
            if contains_geo(child) {
                return Err(Error::Unsupported(
                    "NOT of inexact geo cover requires exact refinement".into(),
                ));
            }
            validate_negation(child)
        }
        Predicate::And(children) | Predicate::Or(children) => {
            for child in children {
                validate_negation(child)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Disjoint truth masks, covering exactly the existing-bucket universe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TruthMasks {
    pub truth: RoaringBitmap,
    pub falsity: RoaringBitmap,
    pub unknown: RoaringBitmap,
    pub exact: bool,
}
impl TruthMasks {
    fn new(
        truth: RoaringBitmap,
        falsity: RoaringBitmap,
        universe: &RoaringBitmap,
        exact: bool,
    ) -> Self {
        let unknown = universe - (&truth | &falsity);
        Self {
            truth,
            falsity,
            unknown,
            exact,
        }
    }
}

/// W6 adapter; returned candidates are conservative shard-local columns.
/// It must return an error when it cannot provide a safe cover.
pub trait GeoIndex: Send + Sync {
    fn cover(&self, shard: ShardKey, field: u32, cells: &[u64]) -> Result<RoaringBitmap>;
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GeoField {
    presence: RoaringBitmap,
    rows: BTreeMap<u64, RoaringBitmap>,
}
impl GeoField {
    pub fn presence(&self) -> &RoaringBitmap {
        &self.presence
    }
    pub fn rows(&self) -> &BTreeMap<u64, RoaringBitmap> {
        &self.rows
    }
    fn clear(&mut self, col: u32) {
        self.presence.remove(col);
        for row in self.rows.values_mut() {
            row.remove(col);
        }
    }
    fn set(&mut self, col: u32, cells: &[u64]) {
        self.presence.insert(col);
        for cell in cells {
            self.rows.entry(*cell).or_default().insert(col);
        }
    }
    fn cover(&self, cells: &[u64]) -> RoaringBitmap {
        let mut out = RoaringBitmap::new();
        for cell in cells {
            if let Some(row) = self.rows.get(cell) {
                out |= row;
            }
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldData {
    Presence(PresenceRow),
    Set(SetField),
    Bsi(BsiField),
    Count(CountField),
    Geo(GeoField),
}
impl FieldData {
    pub fn presence(&self) -> &RoaringBitmap {
        match self {
            Self::Presence(f) => f.bitmap(),
            Self::Set(f) => f.presence(),
            Self::Bsi(f) => f.exists(),
            Self::Count(f) => f.exists(),
            Self::Geo(f) => f.presence(),
        }
    }
    fn clear(&mut self, col: u32) {
        match self {
            Self::Presence(f) => f.clear(col),
            Self::Set(f) => f.clear(col),
            Self::Bsi(f) => f.clear(col),
            Self::Count(f) => f.clear(col),
            Self::Geo(f) => f.clear(col),
        }
    }
}

/// One registered in-memory shard, using only shard-local columns.
#[derive(Debug, Clone)]
pub struct MemoryShard {
    pub key: ShardKey,
    fields: BTreeMap<u32, FieldData>,
    specs: BTreeMap<u32, FieldSpec>,
}
impl MemoryShard {
    pub fn new(key: ShardKey) -> Result<Self> {
        if key.shard > 65535 {
            return Err(Error::InvalidInput("shard exceeds bucket space".into()));
        }
        Ok(Self {
            key,
            fields: BTreeMap::new(),
            specs: BTreeMap::new(),
        })
    }
    pub fn fields(&self) -> &BTreeMap<u32, FieldData> {
        &self.fields
    }
    pub fn field(&self, id: u32) -> Result<&FieldData> {
        self.fields
            .get(&id)
            .ok_or_else(|| Error::NotFound(format!("field {id}")))
    }
    pub fn register_field(&mut self, spec: FieldSpec) -> Result<()> {
        if let Some(old) = self.specs.get(&spec.id) {
            return if old == &spec {
                Ok(())
            } else {
                Err(Error::InvalidInput("field ID conflict".into()))
            };
        }
        if self
            .specs
            .values()
            .any(|f| f.path == spec.path && f.agg == spec.agg)
        {
            return Err(Error::InvalidInput(
                "field identity already registered".into(),
            ));
        }
        let field = match spec.kind {
            FieldKind::Presence => FieldData::Presence(PresenceRow::default()),
            FieldKind::Set => FieldData::Set(if spec.path.ends_with("$source") {
                SetField::source_set()
            } else {
                SetField::new()
            }),
            FieldKind::Bsi { scale } => FieldData::Bsi(BsiField::new(scale)?),
            FieldKind::Count => FieldData::Count(CountField::new()),
            FieldKind::Geo { .. } => FieldData::Geo(GeoField::default()),
        };
        self.fields.insert(spec.id, field);
        self.specs.insert(spec.id, spec);
        Ok(())
    }
    pub fn register_set_value(&mut self, field: u32, row: u32, value: &str) -> Result<()> {
        match self.fields.get_mut(&field) {
            Some(FieldData::Set(f)) => f.register(row, value),
            Some(_) => Err(Error::InvalidInput("field is not Set".into())),
            None => Err(Error::NotFound(format!("field {field}"))),
        }
    }
    pub fn universe(&self) -> RoaringBitmap {
        let mut out = RoaringBitmap::new();
        for f in self.fields.values() {
            out |= f.presence();
        }
        out
    }
    /// Atomic in-memory transaction; WAL/durable publication belongs to W2.
    pub fn apply(&mut self, records: &[BucketRecord]) -> Result<()> {
        ti_contracts::validate_clear_records(records)?;
        let mut groups: BTreeMap<(u32, u32), Vec<&BucketRecord>> = BTreeMap::new();
        for rec in records {
            if rec.vessel != self.key.vessel || rec.bucket >> 16 != self.key.shard {
                return Err(Error::InvalidInput(
                    "record belongs to another shard".into(),
                ));
            }
            self.field(rec.field)?;
            groups
                .entry((rec.bucket & 0xffff, rec.field))
                .or_default()
                .push(rec);
        }
        let mut staged = self.clone();
        for ((col, id), group) in groups {
            let rewrite = group[0].rewrite;
            if group.iter().any(|rec| rec.rewrite != rewrite) {
                return Err(Error::InvalidInput("mixed rewrite flags".into()));
            }
            let field = staged.fields.get_mut(&id).expect("validated field");
            let distinct: Vec<_> = group.iter().map(|r| &r.value).collect();
            let scalar = matches!(field, FieldData::Bsi(_) | FieldData::Count(_))
                || matches!(field,FieldData::Set(f) if !f.is_multi());
            if scalar && distinct.iter().any(|v| *v != distinct[0]) {
                return Err(Error::InvalidInput(
                    "multiple distinct scalar values in a group".into(),
                ));
            }
            if rewrite {
                field.clear(col);
            }
            for rec in group {
                match (&mut *field, &rec.value) {
                    (_, FieldValue::Clear) => {}
                    (FieldData::Presence(f), FieldValue::Present) => f.set(col)?,
                    (FieldData::Set(f), FieldValue::SetValue(row)) => f.set(col, *row)?,
                    (FieldData::Bsi(f), FieldValue::Int(value)) => {
                        if !rewrite
                            && f.exists().contains(col)
                            && f.values(&[col].into_iter().collect())[0] != Some(*value)
                        {
                            return Err(Error::InvalidInput(
                                "numeric replacement requires rewrite".into(),
                            ));
                        }
                        f.set(col, *value)?;
                    }
                    (FieldData::Count(f), FieldValue::Int(value)) if *value >= 0 => {
                        if !rewrite
                            && f.exists().contains(col)
                            && f.values(&[col].into_iter().collect())[0] != Some(*value as u64)
                        {
                            return Err(Error::InvalidInput(
                                "count replacement requires rewrite".into(),
                            ));
                        }
                        f.set(col, *value as u64)?;
                    }
                    (FieldData::Geo(f), FieldValue::Cells(cells)) => f.set(col, cells),
                    _ => return Err(Error::InvalidInput("field/value encoding mismatch".into())),
                }
            }
        }
        *self = staged;
        Ok(())
    }
    pub fn eval_masks(
        &self,
        p: &Predicate,
        text: Option<&dyn TextIndex>,
        geo: Option<&dyn GeoIndex>,
    ) -> Result<TruthMasks> {
        validate_negation(p)?;
        self.evaluate(p, &self.universe(), text, geo)
    }
    fn evaluate(
        &self,
        p: &Predicate,
        u: &RoaringBitmap,
        text: Option<&dyn TextIndex>,
        geo: Option<&dyn GeoIndex>,
    ) -> Result<TruthMasks> {
        let exact = |hits: RoaringBitmap, present: &RoaringBitmap| {
            TruthMasks::new(&hits & u, (present - &hits) & u, u, true)
        };
        Ok(match p {
            Predicate::All => exact(u.clone(), u),
            Predicate::None => exact(RoaringBitmap::new(), u),
            Predicate::Present(id) => exact(self.field(*id)?.presence().clone(), u),
            Predicate::SetEq {
                field,
                rows,
                negate,
            } => match self.field(*field)? {
                FieldData::Set(f) => exact(f.membership(rows, *negate), f.presence()),
                _ => return Err(Error::InvalidInput("SetEq needs Set field".into())),
            },
            Predicate::BsiCmp { field, op, lo, hi } => match self.field(*field)? {
                FieldData::Bsi(f) => exact(f.compare(op.clone(), *lo, *hi, u)?, f.exists()),
                FieldData::Count(f) => exact(f.compare(op.clone(), *lo, *hi, u)?, f.exists()),
                _ => return Err(Error::InvalidInput("BsiCmp needs numeric field".into())),
            },
            Predicate::TsRange { from, to } => {
                let mut mask = RoaringBitmap::new();
                let start = (self.key.shard as u64) << 16;
                if from <= to {
                    let lo = (*from as u64).max(start);
                    let hi = (*to as u64).min(start + SHARD_COLUMNS as u64 - 1);
                    if lo <= hi {
                        mask.insert_range((lo - start) as u32..=(hi - start) as u32);
                    }
                }
                exact(mask, u)
            }
            Predicate::Text { kind, query } => {
                let index = text
                    .ok_or_else(|| Error::Unsupported("Text requires injected TextIndex".into()))?;
                let start = self.key.shard << 16;
                let end = start | 0xffff;
                let hits = index.match_buckets(self.key.vessel, kind, query, start, end)?;
                let local = hits
                    .iter()
                    .filter(|id| {
                        (*id >> 32) == self.key.vessel as u64
                            && (*id as u32) >> 16 == self.key.shard
                    })
                    .map(|id| id as u32 & 0xffff)
                    .collect();
                exact(local, u)
            }
            Predicate::GeoCover { field, cells } => {
                let f = match self.field(*field)? {
                    FieldData::Geo(f) => f,
                    _ => return Err(Error::InvalidInput("GeoCover needs Geo field".into())),
                };
                let hits = match geo {
                    Some(index) => index.cover(self.key, *field, cells)?,
                    None => f.cover(cells),
                };
                let hits = hits & f.presence() & u;
                TruthMasks::new(hits.clone(), (f.presence() - &hits) & u, u, false)
            }
            Predicate::Not(child) => {
                let m = self.evaluate(child, u, text, geo)?;
                if !m.exact {
                    return Err(Error::Unsupported(
                        "NOT of inexact geo cover requires exact refinement".into(),
                    ));
                }
                TruthMasks::new(m.falsity, m.truth, u, true)
            }
            Predicate::And(children) | Predicate::Or(children) => {
                let and = matches!(p, Predicate::And(_));
                let mut t = if and { u.clone() } else { RoaringBitmap::new() };
                let mut f = if and { RoaringBitmap::new() } else { u.clone() };
                let mut is_exact = !contains_geo(p);
                for child in children {
                    // Only decisive two-valued results short-circuit. An empty
                    // true mask with UNKNOWN columns is not decisive.
                    if (and && f == *u) || (!and && t == *u) {
                        break;
                    }
                    let m = self.evaluate(child, u, text, geo)?;
                    if and {
                        t &= m.truth;
                        f |= m.falsity;
                    } else {
                        t |= m.truth;
                        f &= m.falsity;
                    }
                    is_exact &= m.exact;
                }
                TruthMasks::new(t, f, u, is_exact)
            }
        })
    }
    pub fn aggregate(&self, cols: &RoaringBitmap, field: u32, op: AggOp) -> Result<AggPartial> {
        let cols = cols & &self.universe();
        if op == AggOp::CountAll {
            return Ok(AggPartial::Count(cols.len()));
        }
        let f = self.field(field)?;
        if op == AggOp::Count {
            return Ok(AggPartial::Count((cols & f.presence()).len()));
        }
        match f {
            FieldData::Bsi(f) => Ok(match op {
                AggOp::Sum => AggPartial::Sum {
                    sum: f.sum(&cols),
                    count: (&cols & f.exists()).len(),
                },
                AggOp::Min => AggPartial::Min(f.min(&cols)),
                AggOp::Max => AggPartial::Max(f.max(&cols)),
                _ => unreachable!(),
            }),
            FieldData::Count(f) => Ok(match op {
                AggOp::Sum => AggPartial::Sum {
                    sum: f.sum(&cols),
                    count: (&cols & f.exists()).len(),
                },
                AggOp::Min => AggPartial::Min(
                    f.min(&cols)
                        .map(|v| i64::try_from(v).map_err(|_| Error::Overflow("count minimum")))
                        .transpose()?,
                ),
                AggOp::Max => AggPartial::Max(
                    f.max(&cols)
                        .map(|v| i64::try_from(v).map_err(|_| Error::Overflow("count maximum")))
                        .transpose()?,
                ),
                _ => unreachable!(),
            }),
            _ => Err(Error::InvalidInput(
                "numeric aggregate needs BSI/Count field".into(),
            )),
        }
    }
}

/// ShardSource-shaped W1 fixture. Arrow materialization belongs to W4 and
/// intentionally returns Unsupported here; eval and agg are complete core paths.
#[derive(Default)]
pub struct MemorySource {
    shards: BTreeMap<ShardKey, MemoryShard>,
    text: Option<Arc<dyn TextIndex>>,
    geo: Option<Arc<dyn GeoIndex>>,
}
impl MemorySource {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_text(mut self, text: Arc<dyn TextIndex>) -> Self {
        self.text = Some(text);
        self
    }
    pub fn with_geo(mut self, geo: Arc<dyn GeoIndex>) -> Self {
        self.geo = Some(geo);
        self
    }
    pub fn insert(&mut self, shard: MemoryShard) -> Option<MemoryShard> {
        self.shards.insert(shard.key, shard)
    }
    pub fn shard(&self, key: ShardKey) -> Result<&MemoryShard> {
        self.shards
            .get(&key)
            .ok_or_else(|| Error::NotFound("shard".into()))
    }
    pub fn shard_mut(&mut self, key: ShardKey) -> Result<&mut MemoryShard> {
        self.shards
            .get_mut(&key)
            .ok_or_else(|| Error::NotFound("shard".into()))
    }
}
impl ShardSource for MemorySource {
    fn shards(&self, vessels: Option<&[VesselOrd]>, from: u32, to: u32) -> Vec<ShardKey> {
        if from > to {
            return vec![];
        }
        self.shards
            .keys()
            .filter(|key| {
                vessels.is_none_or(|v| v.contains(&key.vessel))
                    && key.shard >= from >> 16
                    && key.shard <= to >> 16
            })
            .copied()
            .collect()
    }
    fn eval(&self, shard: ShardKey, p: &Predicate) -> Result<RoaringBitmap> {
        Ok(self
            .shard(shard)?
            .eval_masks(p, self.text.as_deref(), self.geo.as_deref())?
            .truth)
    }
    fn read(
        &self,
        _shard: ShardKey,
        _cols: &RoaringBitmap,
        _fields: &[u32],
    ) -> Result<RecordBatch> {
        Err(Error::Unsupported(
            "Arrow materialization belongs to W4; use row batch reconstruction".into(),
        ))
    }
    fn agg(
        &self,
        shard: ShardKey,
        cols: &RoaringBitmap,
        field: u32,
        op: AggOp,
    ) -> Result<AggPartial> {
        self.shard(shard)?.aggregate(cols, field, op)
    }
}
