//! Shard management: open mutable shards, sealed immutable shards, and .rbm serialization.
//!
//! Enforces:
//! - Local column addressing 0..=65535
//! - Bit-sliced predicate evaluation and aggregation over roaring rows
//! - Deterministic .rbm output (byte-identical files and equal BLAKE3 hash from identical input)
//! - Canonical BLAKE3 hash over versioned field files via `canonical_shard_input`
//! - Shard repair: late data creates v<N+1>

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;

use arrow_array::builder::{
    BooleanBuilder, Float64Builder, ListBuilder, StringBuilder, UInt64Builder,
};
use arrow_array::{Array, RecordBatch, StringArray, TimestampSecondArray};
use arrow_schema::{DataType, Field, Schema, TimeUnit};
use ti_contracts::{
    canonical_shard_input, validate_clear_records, AggOp, AggPartial, BucketIx, BucketRecord,
    Catalog, Error, FieldKind, FieldSpec, FieldValue, Predicate, Result, RoaringBitmap,
    ShardFileHeader, ShardKey, ShardManifestEntry, TextIndex, VesselOrd, EPOCH, FORMAT_VERSION,
    SHARD_MAGIC,
};

use crate::row::{BsiField, CountField, FieldData, GeoField, PresenceRow, SetField, SHARD_COLUMNS};

/// Disjoint truth masks covering the shard's existing bucket universe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TruthMasks {
    pub truth: RoaringBitmap,
    pub falsity: RoaringBitmap,
    pub unknown: RoaringBitmap,
    pub exact: bool,
}

impl TruthMasks {
    pub fn new(
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

/// Shard data holding roaring fields and metadata for local columns 0..=65535.
#[derive(Debug, Clone)]
pub struct ShardData {
    pub key: ShardKey,
    pub fields: BTreeMap<u32, FieldData>,
    pub specs: BTreeMap<u32, FieldSpec>,
    pub from: BucketIx,
    pub to: BucketIx,
}

impl ShardData {
    pub fn new(key: ShardKey) -> Self {
        let base = key.shard << 16;
        Self {
            key,
            fields: BTreeMap::new(),
            specs: BTreeMap::new(),
            from: base,
            to: base | 0xffff,
        }
    }

    pub fn register_field(&mut self, spec: FieldSpec) -> Result<()> {
        if let Some(old) = self.specs.get(&spec.id) {
            if old == &spec {
                return Ok(());
            } else {
                return Err(Error::InvalidInput("field ID conflict".into()));
            }
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

    pub fn eval_masks(&self, p: &Predicate, text: Option<&dyn TextIndex>) -> Result<TruthMasks> {
        let u = self.universe();
        self.evaluate(p, &u, text)
    }

    fn evaluate(
        &self,
        p: &Predicate,
        u: &RoaringBitmap,
        text: Option<&dyn TextIndex>,
    ) -> Result<TruthMasks> {
        let exact = |hits: RoaringBitmap, present: &RoaringBitmap| {
            TruthMasks::new(&hits & u, (present - &hits) & u, u, true)
        };

        Ok(match p {
            Predicate::All => exact(u.clone(), u),
            Predicate::None => exact(RoaringBitmap::new(), u),
            Predicate::Present(id) => {
                if let Some(f) = self.fields.get(id) {
                    exact(f.presence().clone(), u)
                } else {
                    // Unknown field has no presence -> false for all existing buckets
                    exact(RoaringBitmap::new(), u)
                }
            }
            Predicate::SetEq {
                field,
                rows,
                negate,
            } => match self.fields.get(field) {
                Some(FieldData::Set(f)) => exact(f.membership(rows, *negate), f.presence()),
                Some(_) => return Err(Error::InvalidInput("SetEq requires Set field".into())),
                None => exact(RoaringBitmap::new(), &RoaringBitmap::new()),
            },
            Predicate::BsiCmp { field, op, lo, hi } => match self.fields.get(field) {
                Some(FieldData::Bsi(f)) => exact(f.compare(op.clone(), *lo, *hi, u)?, f.exists()),
                Some(FieldData::Count(f)) => exact(f.compare(op.clone(), *lo, *hi, u)?, f.exists()),
                Some(_) => return Err(Error::InvalidInput("BsiCmp requires numeric field".into())),
                None => exact(RoaringBitmap::new(), &RoaringBitmap::new()),
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
                let local: RoaringBitmap = hits
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
                let f = match self.fields.get(field) {
                    Some(FieldData::Geo(f)) => f,
                    Some(_) => {
                        return Err(Error::InvalidInput("GeoCover requires Geo field".into()))
                    }
                    None => return Ok(exact(RoaringBitmap::new(), &RoaringBitmap::new())),
                };
                let hits = f.cover(cells);
                let hits = hits & f.presence() & u;
                TruthMasks::new(hits.clone(), (f.presence() - &hits) & u, u, false)
            }
            Predicate::Not(child) => {
                let m = self.evaluate(child, u, text)?;
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
                let mut is_exact = true;
                for child in children {
                    if (and && f == *u) || (!and && t == *u) {
                        break;
                    }
                    let m = self.evaluate(child, u, text)?;
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

        let f = self
            .fields
            .get(&field)
            .ok_or_else(|| Error::NotFound(format!("field {field}")))?;

        if op == AggOp::Count {
            return Ok(AggPartial::Count((cols & f.presence()).len()));
        }

        match f {
            FieldData::Bsi(b) => Ok(match op {
                AggOp::Sum => AggPartial::Sum {
                    sum: b.sum(&cols),
                    count: (&cols & b.exists()).len(),
                },
                AggOp::Min => AggPartial::Min(b.min(&cols)),
                AggOp::Max => AggPartial::Max(b.max(&cols)),
                _ => unreachable!(),
            }),
            FieldData::Count(c) => Ok(match op {
                AggOp::Sum => AggPartial::Sum {
                    sum: c.sum(&cols),
                    count: (&cols & c.exists()).len(),
                },
                AggOp::Min => AggPartial::Min(
                    c.min(&cols)
                        .map(|v| i64::try_from(v).map_err(|_| Error::Overflow("count minimum")))
                        .transpose()?,
                ),
                AggOp::Max => AggPartial::Max(
                    c.max(&cols)
                        .map(|v| i64::try_from(v).map_err(|_| Error::Overflow("count maximum")))
                        .transpose()?,
                ),
                _ => unreachable!(),
            }),
            _ => Err(Error::InvalidInput(
                "numeric aggregate requires BSI or Count field".into(),
            )),
        }
    }

    pub fn materialize(
        &self,
        vessel_urn: &str,
        cols: &RoaringBitmap,
        requested_fields: &[u32],
        width_seconds: u64,
        catalog: &dyn Catalog,
    ) -> Result<RecordBatch> {
        let sorted_cols: Vec<u32> = cols.iter().collect();
        let num_rows = sorted_cols.len();

        let mut arrow_fields = Vec::new();
        let mut arrow_columns: Vec<Arc<dyn Array>> = Vec::new();

        arrow_fields.push(Field::new("vessel", DataType::Utf8, false));
        let vessel_arr = StringArray::from(vec![vessel_urn; num_rows]);
        arrow_columns.push(Arc::new(vessel_arr));

        arrow_fields.push(Field::new(
            "ts",
            DataType::Timestamp(TimeUnit::Second, Some("UTC".into())),
            false,
        ));
        let ts_values: Vec<i64> = sorted_cols
            .iter()
            .map(|&c| {
                let bucket = (self.key.shard << 16) | c;
                EPOCH + (bucket as i64) * (width_seconds as i64)
            })
            .collect();
        let ts_arr = TimestampSecondArray::from(ts_values).with_timezone("UTC");
        arrow_columns.push(Arc::new(ts_arr));

        for &fid in requested_fields {
            let spec = self
                .specs
                .get(&fid)
                .cloned()
                .or_else(|| catalog.field(fid).ok())
                .ok_or_else(|| Error::NotFound(format!("field {fid}")))?;

            let name = match &spec.agg {
                Some(a) => format!("{}@{}", spec.path, format!("{:?}", a).to_lowercase()),
                None => spec.path.clone(),
            };

            let field_data = self.fields.get(&fid);

            match &spec.kind {
                FieldKind::Presence => {
                    arrow_fields.push(Field::new(name, DataType::Boolean, true));
                    let mut b = BooleanBuilder::with_capacity(num_rows);
                    for &c in &sorted_cols {
                        if let Some(FieldData::Presence(p)) = field_data {
                            if p.bitmap().contains(c) {
                                b.append_value(true);
                            } else {
                                b.append_null();
                            }
                        } else {
                            b.append_null();
                        }
                    }
                    arrow_columns.push(Arc::new(b.finish()));
                }
                FieldKind::Set if spec.path.ends_with("$source") => {
                    arrow_fields.push(Field::new(
                        name,
                        DataType::List(Arc::new(Field::new("item", DataType::Utf8, false))),
                        true,
                    ));
                    let mut b = ListBuilder::new(StringBuilder::new());
                    for &c in &sorted_cols {
                        if let Some(FieldData::Set(s)) = field_data {
                            let row_ids = s.values(c);
                            if row_ids.is_empty() {
                                b.append_null();
                            } else {
                                let values_builder = b.values();
                                for rid in row_ids {
                                    if let Ok(val) = catalog.set_value(fid, rid) {
                                        values_builder.append_value(&val);
                                    }
                                }
                                b.append(true);
                            }
                        } else {
                            b.append_null();
                        }
                    }
                    arrow_columns.push(Arc::new(b.finish()));
                }
                FieldKind::Set => {
                    arrow_fields.push(Field::new(name, DataType::Utf8, true));
                    let mut b = StringBuilder::with_capacity(num_rows, num_rows * 8);
                    for &c in &sorted_cols {
                        if let Some(FieldData::Set(s)) = field_data {
                            let row_ids = s.values(c);
                            if let Some(&first_rid) = row_ids.first() {
                                if let Ok(val) = catalog.set_value(fid, first_rid) {
                                    b.append_value(&val);
                                } else {
                                    b.append_null();
                                }
                            } else {
                                b.append_null();
                            }
                        } else {
                            b.append_null();
                        }
                    }
                    arrow_columns.push(Arc::new(b.finish()));
                }
                FieldKind::Bsi { scale } => {
                    arrow_fields.push(Field::new(name, DataType::Float64, true));
                    let factor = 10_f64.powi(*scale as i32);
                    let mut b = Float64Builder::with_capacity(num_rows);
                    if let Some(FieldData::Bsi(bsi)) = field_data {
                        let reconstructed = bsi.values(cols);
                        for opt in reconstructed {
                            match opt {
                                Some(val) => b.append_value((val as f64) / factor),
                                None => b.append_null(),
                            }
                        }
                    } else {
                        b.append_nulls(num_rows);
                    }
                    arrow_columns.push(Arc::new(b.finish()));
                }
                FieldKind::Count => {
                    arrow_fields.push(Field::new(name, DataType::UInt64, true));
                    let mut b = UInt64Builder::with_capacity(num_rows);
                    if let Some(FieldData::Count(count)) = field_data {
                        let reconstructed = count.values(cols);
                        for opt in reconstructed {
                            match opt {
                                Some(val) => b.append_value(val),
                                None => b.append_null(),
                            }
                        }
                    } else {
                        b.append_nulls(num_rows);
                    }
                    arrow_columns.push(Arc::new(b.finish()));
                }
                FieldKind::Geo { .. } => {
                    arrow_fields.push(Field::new(
                        name,
                        DataType::List(Arc::new(Field::new("item", DataType::UInt64, false))),
                        true,
                    ));
                    let mut b = ListBuilder::new(UInt64Builder::new());
                    for &c in &sorted_cols {
                        if let Some(FieldData::Geo(g)) = field_data {
                            let cells = g.values(c);
                            if cells.is_empty() {
                                b.append_null();
                            } else {
                                let values_builder = b.values();
                                for cell in cells {
                                    values_builder.append_value(cell);
                                }
                                b.append(true);
                            }
                        } else {
                            b.append_null();
                        }
                    }
                    arrow_columns.push(Arc::new(b.finish()));
                }
            }
        }

        let schema = Arc::new(Schema::new(arrow_fields));
        RecordBatch::try_new(schema, arrow_columns).map_err(Error::Arrow)
    }
}

/// An open, mutable in-memory shard with dirty tracking and WAL coordination.
pub struct OpenShard {
    pub data: ShardData,
    pub dirty: bool,
    pub has_data: bool,
}

impl OpenShard {
    pub fn new(key: ShardKey) -> Self {
        Self {
            data: ShardData::new(key),
            dirty: false,
            has_data: false,
        }
    }

    pub fn register_field(&mut self, spec: FieldSpec) -> Result<()> {
        self.data.register_field(spec)
    }

    pub fn register_set_value(&mut self, field: u32, row: u32, value: &str) -> Result<()> {
        self.data.register_set_value(field, row, value)
    }

    pub fn apply(&mut self, records: &[BucketRecord]) -> Result<()> {
        validate_clear_records(records)?;

        let mut groups: BTreeMap<(u32, u32), Vec<&BucketRecord>> = BTreeMap::new();
        for rec in records {
            if rec.vessel != self.data.key.vessel || rec.bucket >> 16 != self.data.key.shard {
                return Err(Error::InvalidInput(
                    "record belongs to another shard".into(),
                ));
            }
            if !self.data.fields.contains_key(&rec.field) {
                return Err(Error::NotFound(format!("field {}", rec.field)));
            }
            groups
                .entry((rec.bucket & 0xffff, rec.field))
                .or_default()
                .push(rec);
        }

        let mut staged = self.data.clone();
        for ((col, id), group) in groups {
            let rewrite = group[0].rewrite;
            if group.iter().any(|rec| rec.rewrite != rewrite) {
                return Err(Error::InvalidInput("mixed rewrite flags".into()));
            }

            let field = staged.fields.get_mut(&id).expect("validated field");
            let distinct: Vec<_> = group.iter().map(|r| &r.value).collect();
            let scalar = matches!(field, FieldData::Bsi(_) | FieldData::Count(_))
                || matches!(field, FieldData::Set(f) if !f.is_multi());

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

        self.data = staged;
        self.dirty = true;
        self.has_data = true;
        Ok(())
    }

    /// Flush dirty fields to `shards/<vessel>/<shard>/open/<field>.rbm`.
    pub fn flush_to(
        &mut self,
        store_root: &Path,
        vessel_urn: &str,
        width_seconds: u64,
    ) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }

        let open_dir = store_root
            .join("shards")
            .join(self.data.key.vessel.to_string())
            .join(self.data.key.shard.to_string())
            .join("open");
        fs::create_dir_all(&open_dir)?;

        for (field_id, field_data) in &self.data.fields {
            let envelopes = field_data.to_row_envelopes();
            let header = ShardFileHeader {
                version: FORMAT_VERSION,
                vessel_urn: vessel_urn.to_string(),
                shard: self.data.key.shard,
                field: *field_id,
                width_seconds,
                row_count: envelopes.len() as u32,
            };

            let mut bytes = header.encode()?;
            field_data.encode_rows(&mut bytes)?;

            let target = open_dir.join(format!("{field_id}.rbm"));
            let tmp = open_dir.join(format!("{field_id}.tmp.{}", std::process::id()));
            {
                let mut f = OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(&tmp)?;
                f.write_all(&bytes)?;
                f.flush()?;
                f.sync_all()?;
            }
            fs::rename(&tmp, &target)?;
            #[cfg(unix)]
            {
                if let Ok(dir_file) = File::open(&open_dir) {
                    let _ = dir_file.sync_all();
                }
            }
        }

        self.dirty = false;
        Ok(())
    }

    /// Load an open shard from `shards/<vessel>/<shard>/open`.
    pub fn load_open(
        store_root: &Path,
        vessel: VesselOrd,
        shard_no: u32,
        catalog: &dyn Catalog,
    ) -> Result<Option<Self>> {
        let open_dir = store_root
            .join("shards")
            .join(vessel.to_string())
            .join(shard_no.to_string())
            .join("open");

        if !open_dir.exists() {
            return Ok(None);
        }

        let key = ShardKey {
            vessel,
            shard: shard_no,
        };
        let mut data = ShardData::new(key);
        let mut loaded_any = false;

        for entry in fs::read_dir(&open_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("rbm") {
                let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                if let Ok(field_id) = stem.parse::<u32>() {
                    let mut file = match File::open(&path) {
                        Ok(f) => f,
                        Err(_) => continue,
                    };
                    let mut magic = [0u8; 8];
                    if file.read_exact(&mut magic).is_err() || magic != SHARD_MAGIC {
                        continue;
                    }

                    let mut ver_buf = [0u8; 2];
                    if file.read_exact(&mut ver_buf).is_err() {
                        continue;
                    }

                    let mut urn_len_buf = [0u8; 4];
                    if file.read_exact(&mut urn_len_buf).is_err() {
                        continue;
                    }
                    let urn_len = u32::from_le_bytes(urn_len_buf) as usize;
                    let mut urn_buf = vec![0u8; urn_len];
                    if file.read_exact(&mut urn_buf).is_err() {
                        continue;
                    }

                    let mut shard_buf = [0u8; 4];
                    if file.read_exact(&mut shard_buf).is_err() {
                        continue;
                    }

                    let mut field_buf = [0u8; 4];
                    if file.read_exact(&mut field_buf).is_err() {
                        continue;
                    }

                    let mut width_buf = [0u8; 8];
                    if file.read_exact(&mut width_buf).is_err() {
                        continue;
                    }

                    let mut row_count_buf = [0u8; 4];
                    if file.read_exact(&mut row_count_buf).is_err() {
                        continue;
                    }
                    let row_count = u32::from_le_bytes(row_count_buf);

                    let spec = match catalog.field(field_id) {
                        Ok(s) => s,
                        Err(_) => continue,
                    };
                    let dict = if spec.kind == FieldKind::Set {
                        let mut dict = BTreeMap::new();
                        let mut r = 0u32;
                        while let Ok(val) = catalog.set_value(field_id, r) {
                            dict.insert(val, r);
                            r += 1;
                        }
                        Some(dict)
                    } else {
                        None
                    };

                    let is_multi = spec.path.ends_with("$source");
                    if let Ok(field_data) = FieldData::decode_rows(
                        &mut file,
                        &spec.kind,
                        row_count,
                        dict.as_ref(),
                        is_multi,
                    ) {
                        data.fields.insert(field_id, field_data);
                        data.specs.insert(field_id, spec);
                        loaded_any = true;
                    }
                }
            }
        }

        if loaded_any {
            Ok(Some(Self {
                data,
                dirty: false,
                has_data: true,
            }))
        } else {
            Ok(None)
        }
    }

    /// Publish an immutable version `v<version>` and return its manifest entry.
    pub fn seal_to(
        &mut self,
        store_root: &Path,
        vessel_urn: &str,
        version: u64,
        width_seconds: u64,
    ) -> Result<ShardManifestEntry> {
        let shard_dir = store_root
            .join("shards")
            .join(self.data.key.vessel.to_string())
            .join(self.data.key.shard.to_string());
        let version_dir = shard_dir.join(format!("v{version}"));
        fs::create_dir_all(&version_dir)?;

        let mut file_pairs = Vec::new();
        let mut total_bytes = 0u64;

        // Ascending field ID order
        for (field_id, field_data) in &self.data.fields {
            let envelopes = field_data.to_row_envelopes();
            let header = ShardFileHeader {
                version: FORMAT_VERSION,
                vessel_urn: vessel_urn.to_string(),
                shard: self.data.key.shard,
                field: *field_id,
                width_seconds,
                row_count: envelopes.len() as u32,
            };

            let mut bytes = header.encode()?;
            field_data.encode_rows(&mut bytes)?;

            let file_path = version_dir.join(format!("{field_id}.rbm"));
            let tmp = version_dir.join(format!("{field_id}.tmp.{}", std::process::id()));
            {
                let mut f = OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(&tmp)?;
                f.write_all(&bytes)?;
                f.flush()?;
                f.sync_all()?;
            }
            fs::rename(&tmp, &file_path)?;
            #[cfg(unix)]
            {
                if let Ok(dir_file) = File::open(&version_dir) {
                    let _ = dir_file.sync_all();
                }
            }

            total_bytes += bytes.len() as u64;
            file_pairs.push((*field_id, bytes));
        }

        // Canonical BLAKE3 digest per spec 14 §81
        let canonical_bytes = canonical_shard_input(&file_pairs)?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(&canonical_bytes);
        let hash = *hasher.finalize().as_bytes();

        // Clean up open staging dir if present
        let open_dir = shard_dir.join("open");
        if open_dir.exists() {
            let _ = fs::remove_dir_all(open_dir);
        }

        self.dirty = false;

        let entry = ShardManifestEntry {
            key: self.data.key,
            version,
            from: self.data.from,
            to: self.data.to,
            bytes: total_bytes,
            hash,
        };

        Ok(entry)
    }
}

/// A sealed, immutable shard reading from disk files.
pub struct SealedShard {
    pub key: ShardKey,
    pub version: u64,
    pub data: ShardData,
}

impl SealedShard {
    /// Load a sealed shard from `shards/<vessel>/<shard>/v<version>`.
    pub fn load(
        store_root: &Path,
        vessel: VesselOrd,
        shard_no: u32,
        version: u64,
        catalog: &dyn Catalog,
    ) -> Result<Self> {
        let version_dir = store_root
            .join("shards")
            .join(vessel.to_string())
            .join(shard_no.to_string())
            .join(format!("v{version}"));

        if !version_dir.exists() {
            return Err(Error::NotFound(format!("sealed shard v{version}")));
        }

        let key = ShardKey {
            vessel,
            shard: shard_no,
        };
        let mut data = ShardData::new(key);

        for entry in fs::read_dir(&version_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("rbm") {
                let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                if let Ok(field_id) = stem.parse::<u32>() {
                    let mut file = File::open(&path)?;
                    let mut magic = [0u8; 8];
                    file.read_exact(&mut magic)?;
                    if magic != SHARD_MAGIC {
                        return Err(Error::Corrupt("invalid shard magic".into()));
                    }

                    let mut ver_buf = [0u8; 2];
                    file.read_exact(&mut ver_buf)?;
                    let ver = u16::from_le_bytes(ver_buf);
                    if ver != FORMAT_VERSION {
                        return Err(Error::Unsupported(format!("shard version {ver}")));
                    }

                    let mut urn_len_buf = [0u8; 4];
                    file.read_exact(&mut urn_len_buf)?;
                    let urn_len = u32::from_le_bytes(urn_len_buf) as usize;
                    let mut urn_buf = vec![0u8; urn_len];
                    file.read_exact(&mut urn_buf)?;

                    let mut shard_buf = [0u8; 4];
                    file.read_exact(&mut shard_buf)?;

                    let mut field_buf = [0u8; 4];
                    file.read_exact(&mut field_buf)?;

                    let mut width_buf = [0u8; 8];
                    file.read_exact(&mut width_buf)?;

                    let mut row_count_buf = [0u8; 4];
                    file.read_exact(&mut row_count_buf)?;
                    let row_count = u32::from_le_bytes(row_count_buf);

                    let spec = catalog.field(field_id)?;
                    let dict = if spec.kind == FieldKind::Set {
                        // Load dictionary from catalog
                        let mut dict = BTreeMap::new();
                        let mut r = 0u32;
                        while let Ok(val) = catalog.set_value(field_id, r) {
                            dict.insert(val, r);
                            r += 1;
                        }
                        Some(dict)
                    } else {
                        None
                    };

                    let is_multi = spec.path.ends_with("$source");
                    let field_data = FieldData::decode_rows(
                        &mut file,
                        &spec.kind,
                        row_count,
                        dict.as_ref(),
                        is_multi,
                    )?;

                    data.fields.insert(field_id, field_data);
                    data.specs.insert(field_id, spec);
                }
            }
        }

        Ok(Self { key, version, data })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use ti_contracts::Agg;

    struct FixtureCatalog {
        spec: FieldSpec,
    }
    impl Catalog for FixtureCatalog {
        fn register_vessel(&self, _: &ti_contracts::VesselSpec) -> Result<VesselOrd> {
            Ok(0)
        }
        fn vessel_urn(&self, _: VesselOrd) -> Result<String> {
            Ok("vessels.urn:mrn:signalk:uuid:test-boat".into())
        }
        fn register_field(&self, _: &FieldSpec) -> Result<u32> {
            Ok(0)
        }
        fn field(&self, _: u32) -> Result<FieldSpec> {
            Ok(self.spec.clone())
        }
        fn fields(&self) -> Result<Vec<FieldSpec>> {
            Ok(vec![self.spec.clone()])
        }
        fn register_set_value(&self, _: u32, _: &str) -> Result<u32> {
            Ok(0)
        }
        fn set_value(&self, _: u32, _: u32) -> Result<String> {
            Ok("val".into())
        }
        fn set_source_priority(&self, _: &ti_contracts::SourcePriority) -> Result<()> {
            Ok(())
        }
        fn source_priority(&self, _: &str) -> Result<Option<ti_contracts::SourcePriority>> {
            Ok(None)
        }
    }

    #[test]
    fn test_open_shard_apply_flush_and_seal_determinism() {
        let dir1 = tempdir().unwrap();
        let dir2 = tempdir().unwrap();

        let urn = "vessels.urn:mrn:signalk:uuid:test-boat";
        let key = ShardKey {
            vessel: 0,
            shard: 0,
        };

        let spec = FieldSpec {
            id: 0,
            path: "navigation.speedOverGround".into(),
            agg: Some(Agg::Mean),
            kind: FieldKind::Bsi { scale: 3 },
            units: Some("m/s".into()),
        };

        let recs = vec![
            BucketRecord {
                vessel: 0,
                bucket: 10,
                field: 0,
                value: FieldValue::Int(5400),
                rewrite: false,
            },
            BucketRecord {
                vessel: 0,
                bucket: 11,
                field: 0,
                value: FieldValue::Int(5600),
                rewrite: false,
            },
        ];

        // Run 1
        let mut shard1 = OpenShard::new(key);
        shard1.register_field(spec.clone()).unwrap();
        shard1.apply(&recs).unwrap();
        let entry1 = shard1.seal_to(dir1.path(), urn, 1, 10).unwrap();

        // Run 2
        let mut shard2 = OpenShard::new(key);
        shard2.register_field(spec.clone()).unwrap();
        shard2.apply(&recs).unwrap();
        let entry2 = shard2.seal_to(dir2.path(), urn, 1, 10).unwrap();

        // Compare entries and BLAKE3 hashes
        assert_eq!(entry1.hash, entry2.hash);
        assert_eq!(entry1.bytes, entry2.bytes);

        // Compare file bytes on disk directly
        let file1 = dir1.path().join("shards/0/0/v1/0.rbm");
        let file2 = dir2.path().join("shards/0/0/v1/0.rbm");
        let bytes1 = fs::read(&file1).unwrap();
        let bytes2 = fs::read(&file2).unwrap();
        assert_eq!(bytes1, bytes2);

        // Load sealed shard
        let cat = FixtureCatalog { spec };
        let sealed = SealedShard::load(dir1.path(), 0, 0, 1, &cat).unwrap();
        assert_eq!(sealed.data.universe().len(), 2);
    }
}
