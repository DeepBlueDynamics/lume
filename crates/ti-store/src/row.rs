//! In-memory and on-disk roaring row storage for Lume TI.
//!
//! Enforces:
//! - Local column space 0..=65535 per shard
//! - Sign-magnitude BSI encoding with O'Neil/Quass bit-sliced range evaluation
//! - Unsigned Count BSI without sign row
//! - Ordinary Sets (single-valued per bucket, pairwise disjoint rows unioning to presence per D21)
//! - Multi-valued source sets ($source)
//! - Geo cell membership
//! - Canonical portable roaring serialization for `.rbm` files per spec 14 §80

use std::collections::BTreeMap;
use std::io::{Read, Write};
use ti_contracts::{CmpOp, Error, FieldKind, Result, RoaringBitmap};

pub const SHARD_COLUMNS: u32 = 65_536;

pub fn check_col(col: u32) -> Result<()> {
    if col >= SHARD_COLUMNS {
        return Err(Error::InvalidInput("local column exceeds 65535".into()));
    }
    Ok(())
}

pub fn check_row(row: &RoaringBitmap) -> Result<()> {
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
        self.rows.entry(row).or_default();
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
pub struct Magnitude {
    pub exists: RoaringBitmap,
    pub bits: Vec<RoaringBitmap>,
}

#[derive(Debug, Clone)]
pub struct Partition {
    pub less: RoaringBitmap,
    pub equal: RoaringBitmap,
    pub greater: RoaringBitmap,
}

impl Magnitude {
    pub fn from_rows(exists: RoaringBitmap, bits: Vec<RoaringBitmap>) -> Result<Self> {
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

    pub fn set(&mut self, col: u32, value: u64) -> Result<()> {
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

    pub fn clear(&mut self, col: u32) {
        self.exists.remove(col);
        for row in &mut self.bits {
            row.remove(col);
        }
    }

    /// Compute all threshold partitions while visiting each depth row once.
    pub fn partitions(&self, bounds: &[u64], filter: &RoaringBitmap) -> Vec<Partition> {
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

    pub fn extreme(&self, filter: &RoaringBitmap, max: bool) -> Option<u64> {
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

    pub fn reconstruct(&self, cols: &RoaringBitmap) -> Vec<Option<u64>> {
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

    pub fn signed_partitions(&self, bounds: &[i64], filter: &RoaringBitmap) -> Vec<Partition> {
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

    pub fn clear(&mut self, col: u32) {
        self.presence.remove(col);
        for row in self.rows.values_mut() {
            row.remove(col);
        }
    }

    pub fn set(&mut self, col: u32, cells: &[u64]) {
        self.presence.insert(col);
        for cell in cells {
            self.rows.entry(*cell).or_default().insert(col);
        }
    }

    pub fn cover(&self, cells: &[u64]) -> RoaringBitmap {
        let mut out = RoaringBitmap::new();
        for cell in cells {
            if let Some(row) = self.rows.get(cell) {
                out |= row;
            }
        }
        out
    }

    pub fn values(&self, col: u32) -> Vec<u64> {
        self.rows
            .iter()
            .filter_map(|(cell, row)| row.contains(col).then_some(*cell))
            .collect()
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

    pub fn clear(&mut self, col: u32) {
        match self {
            Self::Presence(f) => f.clear(col),
            Self::Set(f) => f.clear(col),
            Self::Bsi(f) => f.clear(col),
            Self::Count(f) => f.clear(col),
            Self::Geo(f) => f.clear(col),
        }
    }
}

/// One raw row envelope in a `.rbm` file.
/// Kind:
/// 0 = presence (key = 0)
/// 1 = set (key = dictionary row ID)
/// 2 = exists (key = 0)
/// 3 = sign (key = 0)
/// 4 = magnitude-bit (key = bit index 0..63)
/// 5 = geo (key = H3 cell ID)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowEnvelope {
    pub kind: u8,
    pub key: u64,
    pub bitmap: RoaringBitmap,
}

impl FieldData {
    /// Collect sorted rows `(kind, key, bitmap)` for `.rbm` serialization.
    pub fn to_row_envelopes(&self) -> Vec<RowEnvelope> {
        let mut out = Vec::new();
        match self {
            Self::Presence(p) => {
                out.push(RowEnvelope {
                    kind: 0,
                    key: 0,
                    bitmap: p.bitmap().clone(),
                });
            }
            Self::Set(s) => {
                for (row_id, bm) in s.rows() {
                    out.push(RowEnvelope {
                        kind: 1,
                        key: *row_id as u64,
                        bitmap: bm.clone(),
                    });
                }
            }
            Self::Bsi(b) => {
                out.push(RowEnvelope {
                    kind: 2,
                    key: 0,
                    bitmap: b.exists().clone(),
                });
                out.push(RowEnvelope {
                    kind: 3,
                    key: 0,
                    bitmap: b.sign().clone(),
                });
                for (i, bit) in b.bits().iter().enumerate() {
                    out.push(RowEnvelope {
                        kind: 4,
                        key: i as u64,
                        bitmap: bit.clone(),
                    });
                }
            }
            Self::Count(c) => {
                out.push(RowEnvelope {
                    kind: 2,
                    key: 0,
                    bitmap: c.exists().clone(),
                });
                for (i, bit) in c.bits().iter().enumerate() {
                    out.push(RowEnvelope {
                        kind: 4,
                        key: i as u64,
                        bitmap: bit.clone(),
                    });
                }
            }
            Self::Geo(g) => {
                for (cell, bm) in g.rows() {
                    out.push(RowEnvelope {
                        kind: 5,
                        key: *cell,
                        bitmap: bm.clone(),
                    });
                }
            }
        }
        // Strict sort order: (kind, key) ascending
        out.sort_by_key(|r| (r.kind, r.key));
        out
    }

    /// Encode all rows into portable format bytes.
    pub fn encode_rows<W: Write>(&self, writer: &mut W) -> Result<u32> {
        let envelopes = self.to_row_envelopes();
        for row in &envelopes {
            writer.write_all(&[row.kind])?;
            writer.write_all(&row.key.to_le_bytes())?;
            let mut payload = Vec::new();
            row.bitmap
                .serialize_into(&mut payload)
                .map_err(|e| Error::Corrupt(format!("roaring serialization error: {}", e)))?;
            let len = payload.len() as u64;
            writer.write_all(&len.to_le_bytes())?;
            writer.write_all(&payload)?;
        }
        Ok(envelopes.len() as u32)
    }

    /// Decode rows from portable format into FieldData.
    pub fn decode_rows<R: Read>(
        reader: &mut R,
        kind: &FieldKind,
        row_count: u32,
        dictionary: Option<&BTreeMap<String, u32>>,
        multi: bool,
    ) -> Result<Self> {
        let mut envelopes = Vec::with_capacity(row_count as usize);
        for _ in 0..row_count {
            let mut kind_buf = [0u8; 1];
            reader.read_exact(&mut kind_buf)?;
            let row_kind = kind_buf[0];

            let mut key_buf = [0u8; 8];
            reader.read_exact(&mut key_buf)?;
            let key = u64::from_le_bytes(key_buf);

            let mut len_buf = [0u8; 8];
            reader.read_exact(&mut len_buf)?;
            let len = u64::from_le_bytes(len_buf) as usize;

            let mut payload = vec![0u8; len];
            reader.read_exact(&mut payload)?;
            let bitmap = RoaringBitmap::deserialize_from(&payload[..])
                .map_err(|e| Error::Corrupt(format!("roaring deserialization error: {}", e)))?;

            envelopes.push(RowEnvelope {
                kind: row_kind,
                key,
                bitmap,
            });
        }

        match kind {
            FieldKind::Presence => {
                let bm = envelopes
                    .into_iter()
                    .find(|r| r.kind == 0)
                    .map(|r| r.bitmap)
                    .unwrap_or_default();
                Ok(FieldData::Presence(PresenceRow::from_bitmap(bm)?))
            }
            FieldKind::Set => {
                let mut rows = BTreeMap::new();
                for r in envelopes {
                    if r.kind == 1 {
                        rows.insert(r.key as u32, r.bitmap);
                    }
                }
                let dict = dictionary.cloned().unwrap_or_default();
                Ok(FieldData::Set(SetField::from_rows(dict, rows, multi)?))
            }
            FieldKind::Bsi { scale } => {
                let mut exists = RoaringBitmap::new();
                let mut sign = RoaringBitmap::new();
                let mut bits_map: BTreeMap<usize, RoaringBitmap> = BTreeMap::new();

                for r in envelopes {
                    match r.kind {
                        2 => exists = r.bitmap,
                        3 => sign = r.bitmap,
                        4 => {
                            bits_map.insert(r.key as usize, r.bitmap);
                        }
                        _ => {}
                    }
                }
                let max_bit = bits_map.keys().copied().max().map(|m| m + 1).unwrap_or(0);
                let mut bits = vec![RoaringBitmap::new(); max_bit];
                for (idx, bm) in bits_map {
                    bits[idx] = bm;
                }
                Ok(FieldData::Bsi(BsiField::from_rows(
                    *scale, exists, sign, bits,
                )?))
            }
            FieldKind::Count => {
                let mut exists = RoaringBitmap::new();
                let mut bits_map: BTreeMap<usize, RoaringBitmap> = BTreeMap::new();

                for r in envelopes {
                    match r.kind {
                        2 => exists = r.bitmap,
                        4 => {
                            bits_map.insert(r.key as usize, r.bitmap);
                        }
                        _ => {}
                    }
                }
                let max_bit = bits_map.keys().copied().max().map(|m| m + 1).unwrap_or(0);
                let mut bits = vec![RoaringBitmap::new(); max_bit];
                for (idx, bm) in bits_map {
                    bits[idx] = bm;
                }
                Ok(FieldData::Count(CountField::from_rows(exists, bits)?))
            }
            FieldKind::Geo { .. } => {
                let mut rows = BTreeMap::new();
                let mut presence = RoaringBitmap::new();
                for r in envelopes {
                    if r.kind == 5 {
                        presence |= &r.bitmap;
                        rows.insert(r.key, r.bitmap);
                    }
                }
                Ok(FieldData::Geo(GeoField { presence, rows }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_presence_row() {
        let mut pr = PresenceRow::default();
        pr.set(10).unwrap();
        pr.set(20).unwrap();
        assert!(pr.bitmap().contains(10));
        assert!(pr.bitmap().contains(20));
        assert!(!pr.bitmap().contains(30));
        pr.clear(10);
        assert!(!pr.bitmap().contains(10));
        assert!(pr.set(65536).is_err());
    }

    #[test]
    fn test_bsi_signed_comparison_and_aggregation() {
        let mut bsi = BsiField::new(3).unwrap();
        bsi.set(0, -100).unwrap();
        bsi.set(1, 0).unwrap();
        bsi.set(2, 200).unwrap();
        bsi.set(3, 50).unwrap();

        let universe: RoaringBitmap = [0, 1, 2, 3].into_iter().collect();

        // < 0
        let lt_zero = bsi.compare(CmpOp::Lt, 0, None, &universe).unwrap();
        assert_eq!(lt_zero.iter().collect::<Vec<_>>(), vec![0]);

        // >= 0
        let ge_zero = bsi.compare(CmpOp::Ge, 0, None, &universe).unwrap();
        assert_eq!(ge_zero.iter().collect::<Vec<_>>(), vec![1, 2, 3]);

        // Between 0 and 100
        let between = bsi.compare(CmpOp::Between, 0, Some(100), &universe).unwrap();
        assert_eq!(between.iter().collect::<Vec<_>>(), vec![1, 3]);

        // Aggregation
        assert_eq!(bsi.sum(&universe), 150);
        assert_eq!(bsi.min(&universe), Some(-100));
        assert_eq!(bsi.max(&universe), Some(200));

        let vals = bsi.values(&universe);
        assert_eq!(vals, vec![Some(-100), Some(0), Some(200), Some(50)]);
    }

    #[test]
    fn test_row_encoding_and_decoding_roundtrip() {
        let mut bsi = BsiField::new(2).unwrap();
        bsi.set(5, -42).unwrap();
        bsi.set(10, 100).unwrap();
        let orig = FieldData::Bsi(bsi);

        let mut buf = Vec::new();
        let count = orig.encode_rows(&mut buf).unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        let decoded =
            FieldData::decode_rows(&mut cursor, &FieldKind::Bsi { scale: 2 }, count, None, false)
                .unwrap();

        assert_eq!(orig, decoded);
    }
}
