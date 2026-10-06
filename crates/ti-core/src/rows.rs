//! Original bitmap algorithms, with no copied FeatureBase/Pilosa code.
//! Mutation is clear/install. Magnitude comparison is the O'Neil/Quass-style
//! high-to-low equal-prefix partition; BETWEEN tracks both bounds in the same
//! row traversal. Signed order reverses magnitude order for negatives.
//! Sum uses signed slice popcounts; min/max greedily refine candidate prefixes.
//! Batch reconstruction intersects each slice once with the requested columns.

use std::collections::BTreeMap;
use ti_contracts::{CmpOp, Error, Result, RoaringBitmap};

pub const SHARD_COLUMNS: u32 = 65_536;

fn check_col(col: u32) -> Result<()> {
    if col >= SHARD_COLUMNS {
        return Err(Error::InvalidInput("local column exceeds 65535".into()));
    }
    Ok(())
}

fn check_row(row: &RoaringBitmap) -> Result<()> {
    if row.max().is_some_and(|col| col >= SHARD_COLUMNS) {
        return Err(Error::Corrupt("row column exceeds 65535".into()));
    }
    Ok(())
}

/// One existence row.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PresenceRow(RoaringBitmap);
impl PresenceRow {
    pub fn from_bitmap(bitmap: RoaringBitmap) -> Result<Self> {
        check_row(&bitmap)?;
        Ok(Self(bitmap))
    }

    pub fn bitmap(&self) -> &RoaringBitmap {
        &self.0
    }
    pub fn set(&mut self, col: u32) -> Result<()> {
        check_col(col)?;
        self.0.insert(col);
        Ok(())
    }
    pub fn clear(&mut self, col: u32) {
        self.0.remove(col);
    }
}

/// Stable UTF-8 dictionary and bitmap rows. Ordinary fields are single-valued;
/// source sets explicitly opt into multi-value membership.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SetField {
    dictionary: BTreeMap<String, u32>,
    rows: BTreeMap<u32, RoaringBitmap>,
    presence: RoaringBitmap,
    multi: bool,
}
impl SetField {
    /// Restore catalog dictionary IDs and portable rows without decoding columns.
    pub fn from_rows(
        dictionary: BTreeMap<String, u32>,
        rows: BTreeMap<u32, RoaringBitmap>,
        multi: bool,
    ) -> Result<Self> {
        let mut ids = std::collections::BTreeSet::new();
        for id in dictionary.values() {
            if !ids.insert(*id) {
                return Err(Error::Corrupt("duplicate dictionary row ID".into()));
            }
        }
        if rows.keys().any(|id| !ids.contains(id)) {
            return Err(Error::Corrupt("unknown dictionary row".into()));
        }
        let mut presence = RoaringBitmap::new();
        for row in rows.values() {
            check_row(row)?;
            presence |= row;
        }
        let mut result = Self {
            dictionary,
            rows,
            presence,
            multi,
        };
        for id in ids {
            result.rows.entry(id).or_default();
        }
        result.validate()?;
        Ok(result)
    }

    pub fn new() -> Self {
        Self::default()
    }
    pub fn source_set() -> Self {
        Self {
            multi: true,
            ..Self::default()
        }
    }
    pub fn is_multi(&self) -> bool {
        self.multi
    }
    pub fn dictionary(&self) -> &BTreeMap<String, u32> {
        &self.dictionary
    }
    pub fn rows(&self) -> &BTreeMap<u32, RoaringBitmap> {
        &self.rows
    }
    pub fn presence(&self) -> &RoaringBitmap {
        &self.presence
    }
    /// Import the row ID assigned by the catalog without renumbering it.
    pub fn register(&mut self, row: u32, value: &str) -> Result<()> {
        if self.dictionary.get(value).is_some_and(|id| *id != row)
            || self
                .dictionary
                .iter()
                .any(|(s, id)| *id == row && s != value)
        {
            return Err(Error::InvalidInput(
                "set dictionary identity conflict".into(),
            ));
        }
        self.dictionary.insert(value.into(), row);
        self.rows.entry(row).or_default();
        Ok(())
    }
    /// Install one value, replacing an ordinary field's old value.
    pub fn set(&mut self, col: u32, row: u32) -> Result<()> {
        check_col(col)?;
        if !self.rows.contains_key(&row) {
            return Err(Error::NotFound(format!("set row {row}")));
        }
        if !self.multi {
            self.clear(col);
        }
        self.rows.get_mut(&row).expect("validated row").insert(col);
        self.presence.insert(col);
        Ok(())
    }
    pub fn clear(&mut self, col: u32) {
        for row in self.rows.values_mut() {
            row.remove(col);
        }
        self.presence.remove(col);
    }
    pub fn membership(&self, rows: &[u32], negate: bool) -> RoaringBitmap {
        let mut hits = RoaringBitmap::new();
        for id in rows {
            if let Some(row) = self.rows.get(id) {
                hits |= row;
            }
        }
        if negate {
            &self.presence - &hits
        } else {
            hits
        }
    }
    pub fn values(&self, col: u32) -> Vec<u32> {
        self.rows
            .iter()
            .filter_map(|(id, row)| row.contains(col).then_some(*id))
            .collect()
    }
    pub fn validate(&self) -> Result<()> {
        if self.multi {
            let mut union = RoaringBitmap::new();
            for row in self.rows.values() {
                union |= row;
            }
            if union != self.presence {
                return Err(Error::Corrupt("source rows differ from presence".into()));
            }
            Ok(())
        } else {
            ti_contracts::validate_ordinary_set_rows(
                &self.presence,
                &self.rows.values().cloned().collect::<Vec<_>>(),
            )
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Magnitude {
    exists: RoaringBitmap,
    bits: Vec<RoaringBitmap>,
}
#[derive(Debug, Clone)]
struct Partition {
    less: RoaringBitmap,
    equal: RoaringBitmap,
    greater: RoaringBitmap,
}
impl Magnitude {
    fn from_rows(exists: RoaringBitmap, bits: Vec<RoaringBitmap>) -> Result<Self> {
        check_row(&exists)?;
        if bits.len() > 64 {
            return Err(Error::Corrupt("BSI depth exceeds 64".into()));
        }
        for bit in &bits {
            check_row(bit)?;
            if !(bit - &exists).is_empty() {
                return Err(Error::Corrupt("magnitude row outside exists".into()));
            }
        }
        Ok(Self { exists, bits })
    }

    fn set(&mut self, col: u32, value: u64) -> Result<()> {
        check_col(col)?;
        self.clear(col);
        let depth = (64 - value.leading_zeros()) as usize;
        self.bits
            .resize_with(self.bits.len().max(depth), RoaringBitmap::new);
        self.exists.insert(col);
        for (i, row) in self.bits.iter_mut().enumerate() {
            if value & (1u64 << i) != 0 {
                row.insert(col);
            }
        }
        Ok(())
    }
    fn clear(&mut self, col: u32) {
        self.exists.remove(col);
        for row in &mut self.bits {
            row.remove(col);
        }
    }
    /// Compute all threshold partitions while visiting each depth row once.
    fn partitions(&self, bounds: &[u64], filter: &RoaringBitmap) -> Vec<Partition> {
        let domain = &self.exists & filter;
        let mut out: Vec<_> = bounds
            .iter()
            .map(|bound| {
                let exceeds = (64 - bound.leading_zeros()) as usize > self.bits.len();
                Partition {
                    less: if exceeds {
                        domain.clone()
                    } else {
                        RoaringBitmap::new()
                    },
                    equal: if exceeds {
                        RoaringBitmap::new()
                    } else {
                        domain.clone()
                    },
                    greater: RoaringBitmap::new(),
                }
            })
            .collect();
        for (bit, row) in self.bits.iter().enumerate().rev() {
            for (bound, p) in bounds.iter().zip(&mut out) {
                if p.equal.is_empty() {
                    continue;
                }
                if bound & (1u64 << bit) != 0 {
                    p.less |= &p.equal - row;
                    p.equal &= row;
                } else {
                    p.greater |= &p.equal & row;
                    p.equal -= row;
                }
            }
        }
        out
    }
    /// Greedy digit-prefix selection, no per-column value decoding.
    fn extreme(&self, filter: &RoaringBitmap, max: bool) -> Option<u64> {
        let mut candidates = &self.exists & filter;
        if candidates.is_empty() {
            return None;
        }
        let mut magnitude = 0;
        for (i, row) in self.bits.iter().enumerate().rev() {
            let ones = &candidates & row;
            let zeroes = &candidates - row;
            if (max && !ones.is_empty()) || (!max && zeroes.is_empty()) {
                magnitude |= 1u64 << i;
                candidates = ones;
            } else {
                candidates = zeroes;
            }
        }
        Some(magnitude)
    }
    fn reconstruct(&self, cols: &RoaringBitmap) -> Vec<Option<u64>> {
        let ordered: Vec<_> = cols.iter().collect();
        let mut values: Vec<_> = ordered
            .iter()
            .map(|c| self.exists.contains(*c).then_some(0u64))
            .collect();
        let index: BTreeMap<_, _> = ordered.iter().enumerate().map(|(i, c)| (*c, i)).collect();
        for (bit, row) in self.bits.iter().enumerate() {
            for col in (row & cols).iter() {
                if let Some(value) = &mut values[index[&col]] {
                    *value |= 1u64 << bit;
                }
            }
        }
        values
    }
}

/// Signed fixed-point sign-magnitude field. Stored integers span all of i64;
/// scale affects only physical conversion, not bitmap algorithms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BsiField {
    scale: u8,
    magnitude: Magnitude,
    sign: RoaringBitmap,
}
impl BsiField {
    /// Restore sign-magnitude rows; reject negative zero and out-of-range magnitudes.
    pub fn from_rows(
        scale: u8,
        exists: RoaringBitmap,
        sign: RoaringBitmap,
        bits: Vec<RoaringBitmap>,
    ) -> Result<Self> {
        Self::new(scale)?;
        let magnitude = Magnitude::from_rows(exists, bits)?;
        check_row(&sign)?;
        if !(&sign - &magnitude.exists).is_empty() {
            return Err(Error::Corrupt("sign outside exists".into()));
        }
        let mut nonzero = RoaringBitmap::new();
        for bit in &magnitude.bits {
            nonzero |= bit;
        }
        if !(&sign - &nonzero).is_empty() {
            return Err(Error::Corrupt("negative zero".into()));
        }
        if let Some(high) = magnitude.bits.get(63) {
            if !(high - &sign).is_empty() {
                return Err(Error::Corrupt("positive magnitude exceeds i64".into()));
            }
            for low in &magnitude.bits[..63] {
                if !(low & high).is_empty() {
                    return Err(Error::Corrupt("negative magnitude exceeds i64".into()));
                }
            }
        }
        Ok(Self {
            scale,
            magnitude,
            sign,
        })
    }

    pub fn new(scale: u8) -> Result<Self> {
        if scale > 18 {
            return Err(Error::InvalidInput("bsi.scale exceeds 18".into()));
        }
        Ok(Self {
            scale,
            magnitude: Magnitude::default(),
            sign: RoaringBitmap::new(),
        })
    }
    pub fn scale(&self) -> u8 {
        self.scale
    }
    pub fn depth(&self) -> usize {
        self.magnitude.bits.len()
    }
    pub fn exists(&self) -> &RoaringBitmap {
        &self.magnitude.exists
    }
    pub fn sign(&self) -> &RoaringBitmap {
        &self.sign
    }
    pub fn bits(&self) -> &[RoaringBitmap] {
        &self.magnitude.bits
    }
    pub fn set(&mut self, col: u32, value: i64) -> Result<()> {
        self.magnitude.set(col, value.unsigned_abs())?;
        self.sign.remove(col);
        if value < 0 {
            self.sign.insert(col);
        }
        Ok(())
    }
    pub fn clear(&mut self, col: u32) {
        self.magnitude.clear(col);
        self.sign.remove(col);
    }
    fn signed_partitions(&self, bounds: &[i64], filter: &RoaringBitmap) -> Vec<Partition> {
        let magnitudes: Vec<_> = bounds.iter().map(|v| v.unsigned_abs()).collect();
        let neg = &self.sign & filter;
        let pos = (&self.magnitude.exists & filter) - &neg;
        self.magnitude
            .partitions(&magnitudes, filter)
            .into_iter()
            .zip(bounds)
            .map(|(p, bound)| {
                if *bound < 0 {
                    Partition {
                        less: &p.greater & &neg,
                        equal: &p.equal & &neg,
                        greater: (&p.less & &neg) | &pos,
                    }
                } else {
                    Partition {
                        less: (&p.less & &pos) | &neg,
                        equal: &p.equal & &pos,
                        greater: &p.greater & &pos,
                    }
                }
            })
            .collect()
    }
    pub fn compare(
        &self,
        op: CmpOp,
        lo: i64,
        hi: Option<i64>,
        filter: &RoaringBitmap,
    ) -> Result<RoaringBitmap> {
        if op == CmpOp::Between {
            let hi = hi.ok_or_else(|| Error::InvalidInput("BETWEEN needs upper bound".into()))?;
            if lo > hi {
                return Ok(RoaringBitmap::new());
            }
            let mut parts = self.signed_partitions(&[lo, hi], filter).into_iter();
            let low = parts.next().expect("two bounds");
            let high = parts.next().expect("two bounds");
            return Ok((low.greater | low.equal) & (high.less | high.equal));
        }
        let p = self
            .signed_partitions(&[lo], filter)
            .pop()
            .expect("one bound");
        Ok(match op {
            CmpOp::Eq => p.equal,
            CmpOp::Ne => p.less | p.greater,
            CmpOp::Lt => p.less,
            CmpOp::Le => p.less | p.equal,
            CmpOp::Gt => p.greater,
            CmpOp::Ge => p.greater | p.equal,
            CmpOp::Between => unreachable!(),
        })
    }
    pub fn sum(&self, filter: &RoaringBitmap) -> i128 {
        let selected = &self.magnitude.exists & filter;
        let negative = &self.sign & &selected;
        self.magnitude
            .bits
            .iter()
            .enumerate()
            .map(|(i, row)| {
                let all = (row & &selected).len() as i128;
                let neg = (row & &negative).len() as i128;
                (all - 2 * neg) * (1i128 << i)
            })
            .sum()
    }
    pub fn min(&self, filter: &RoaringBitmap) -> Option<i64> {
        let neg = &self.sign & filter;
        if !neg.is_empty() {
            self.magnitude
                .extreme(&neg, true)
                .map(|v| (-(v as i128)) as i64)
        } else {
            self.magnitude.extreme(filter, false).map(|v| v as i64)
        }
    }
    pub fn max(&self, filter: &RoaringBitmap) -> Option<i64> {
        let pos = (&self.magnitude.exists & filter) - &self.sign;
        if !pos.is_empty() {
            self.magnitude.extreme(&pos, true).map(|v| v as i64)
        } else {
            self.magnitude
                .extreme(filter, false)
                .map(|v| (-(v as i128)) as i64)
        }
    }
    /// Results follow ascending requested columns, including nulls.
    pub fn values(&self, cols: &RoaringBitmap) -> Vec<Option<i64>> {
        self.magnitude
            .reconstruct(cols)
            .into_iter()
            .zip(cols.iter())
            .map(|(v, c)| {
                v.map(|n| {
                    if self.sign.contains(c) {
                        (-(n as i128)) as i64
                    } else {
                        n as i64
                    }
                })
            })
            .collect()
    }
}

/// Unsigned count BSI, with no allocated sign row; supports the full u64 domain.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CountField {
    magnitude: Magnitude,
}
impl CountField {
    pub fn from_rows(exists: RoaringBitmap, bits: Vec<RoaringBitmap>) -> Result<Self> {
        Ok(Self {
            magnitude: Magnitude::from_rows(exists, bits)?,
        })
    }

    pub fn new() -> Self {
        Self::default()
    }
    pub fn exists(&self) -> &RoaringBitmap {
        &self.magnitude.exists
    }
    pub fn bits(&self) -> &[RoaringBitmap] {
        &self.magnitude.bits
    }
    pub fn depth(&self) -> usize {
        self.magnitude.bits.len()
    }
    pub fn set(&mut self, col: u32, value: u64) -> Result<()> {
        self.magnitude.set(col, value)
    }
    pub fn clear(&mut self, col: u32) {
        self.magnitude.clear(col);
    }
    pub fn values(&self, cols: &RoaringBitmap) -> Vec<Option<u64>> {
        self.magnitude.reconstruct(cols)
    }
    pub fn sum(&self, filter: &RoaringBitmap) -> i128 {
        let selected = &self.magnitude.exists & filter;
        self.magnitude
            .bits
            .iter()
            .enumerate()
            .map(|(i, row)| (row & &selected).len() as i128 * (1i128 << i))
            .sum()
    }
    pub fn min(&self, filter: &RoaringBitmap) -> Option<u64> {
        self.magnitude.extreme(filter, false)
    }
    pub fn max(&self, filter: &RoaringBitmap) -> Option<u64> {
        self.magnitude.extreme(filter, true)
    }
    /// The frozen Predicate carries signed thresholds: negative literals are below all counts.
    pub fn compare(
        &self,
        op: CmpOp,
        lo: i64,
        hi: Option<i64>,
        filter: &RoaringBitmap,
    ) -> Result<RoaringBitmap> {
        let bounds = if op == CmpOp::Between {
            vec![
                lo,
                hi.ok_or_else(|| Error::InvalidInput("BETWEEN needs upper bound".into()))?,
            ]
        } else {
            vec![lo]
        };
        let unsigned: Vec<_> = bounds.iter().map(|v| (*v).max(0) as u64).collect();
        let domain = &self.magnitude.exists & filter;
        let parts: Vec<_> = self
            .magnitude
            .partitions(&unsigned, filter)
            .into_iter()
            .zip(&bounds)
            .map(|(p, b)| {
                if *b < 0 {
                    Partition {
                        less: RoaringBitmap::new(),
                        equal: RoaringBitmap::new(),
                        greater: domain.clone(),
                    }
                } else {
                    p
                }
            })
            .collect();
        let p = &parts[0];
        Ok(match op {
            CmpOp::Eq => p.equal.clone(),
            CmpOp::Ne => &p.less | &p.greater,
            CmpOp::Lt => p.less.clone(),
            CmpOp::Le => &p.less | &p.equal,
            CmpOp::Gt => p.greater.clone(),
            CmpOp::Ge => &p.greater | &p.equal,
            CmpOp::Between => {
                if lo > bounds[1] {
                    RoaringBitmap::new()
                } else {
                    (&p.greater | &p.equal) & (&parts[1].less | &parts[1].equal)
                }
            }
        })
    }
}
