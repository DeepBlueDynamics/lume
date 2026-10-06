# 10. Frozen contracts and PR rules

Spec pp. 20–24. Owner: [W0](../lanes/W0-contracts.md).

Nine crates, nine agent lanes. W0 freezes the shared contracts in week 1; every lane builds against these traits and fixtures. All TI crates live under `crates/ti-*`, behind the root `ti` feature.

## Contracts (W0 freeze)

Changing a boundary below requires a contracts PR approved by the integrator. Behavioral rules are frozen in [14-semantics.md](14-semantics.md). Each code block mirrors the corresponding source file exactly, excluding its `#[cfg(test)]` module. The files form one crate; the blocks are not a single concatenated module.

### [lib.rs](../../crates/ti-contracts/src/lib.rs)

```rust
//! Shared types and boundaries for Lume TI lanes.
//!
//! Frozen behavioral rules are in plan/spec/14-semantics.md.
//! This crate contains contracts and checked addressing/conversion helpers only.

#![forbid(unsafe_code)]

pub use arrow_array::RecordBatch;
pub use arrow_schema;
pub use roaring::{RoaringBitmap, RoaringTreemap};

use serde::{Deserialize, Serialize};

mod catalog;
mod config;
mod engine;
mod envelopes;
mod schemas;
pub use catalog::*;
pub use config::*;
pub use engine::*;
pub use envelopes::*;
pub use schemas::*;

mod helpers;
pub use helpers::*;

/// Dense store-local vessel ordinal, resolved from a Signal K URN.
pub type VesselOrd = u32;
/// Time bucket index relative to EPOCH, with a fixed deployment width.
pub type BucketIx = u32; // floor((t - EPOCH) / W)
/// Global column identity: vessel in the high 32 bits, bucket in the low bits.
pub type ColumnId = u64; // (vessel as u64) << 32 | bucket

/// One vessel and a 65,536-bucket window.
#[derive(Clone, Copy, Debug, Hash, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct ShardKey {
    /// Store-local vessel ordinal.
    pub vessel: VesselOrd,
    /// Bucket index shifted right by 16.
    pub shard: u32,
} // shard = bucket >> 16

/// Storage encoding for a field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldKind {
    /// A single exists bitmap.
    Presence,
    /// A bitmap per dictionary value.
    Set,
    /// Signed fixed-point bit slices; scale is decimal digits.
    Bsi { scale: u8 },
    /// Unsigned event-count bit slices.
    Count,
    /// H3 cell membership at the given resolution.
    Geo { res: u8 },
}

/// Catalog definition of one indexed field and its physical units.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldSpec {
    /// Stable field ID.
    pub id: u32,
    /// Exact Signal K path.
    pub path: String,
    /// Optional bucket aggregate, absent for categorical fields.
    pub agg: Option<Agg>,
    /// Storage encoding.
    pub kind: FieldKind,
    /// Signal K physical units when known.
    pub units: Option<String>,
}
/// Ingest-time bucket aggregate or derived event counter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Agg {
    /// Arithmetic mean.
    Mean,
    /// Minimum sample.
    Min,
    /// Maximum sample.
    Max,
    /// Last sample by event time.
    Last,
    /// Sample count.
    Count,
    /// Transitions into the configured started state.
    Starts,
    /// Boolean rising-edge count.
    Edges,
}

/// Output of the bucketer; input to the store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BucketRecord {
    /// Store-local vessel ordinal.
    pub vessel: VesselOrd,
    /// Global bucket index.
    pub bucket: BucketIx,
    /// Registered field ID.
    pub field: u32,
    /// Encoded field value.
    pub value: FieldValue,
    /// Replace the whole bucket/field group rather than accumulate.
    pub rewrite: bool,
}
/// One encoded value; set dictionaries are maintained outside this record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldValue {
    /// Delete this bucket/field's column. Valid only in a rewrite group with no other values.
    Clear,
    /// Mark field presence.
    Present,
    /// Dictionary row ID.
    SetValue(u32 /* row id */),
    /// Signed fixed-point value or nonnegative count.
    Int(i64),
    /// Occupied H3 cell IDs.
    Cells(Vec<u64>),
}

/// The only language between SQL and bitmaps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Predicate {
    /// Existing bucket universe.
    All,
    /// Empty result.
    None,
    /// Field presence test.
    Present(u32),
    /// Union of matching rows, or presence minus that union.
    SetEq {
        field: u32,
        rows: Vec<u32>,
        negate: bool,
    },
    /// Signed fixed-point range comparison.
    BsiCmp {
        field: u32,
        op: CmpOp,
        lo: i64,
        hi: Option<i64>,
    }, // hi for BETWEEN
    /// Inclusive global bucket range.
    TsRange { from: BucketIx, to: BucketIx }, // inclusive
    /// Documents mapped to bucket columns.
    Text { kind: String, query: String },
    /// Conservative H3 cell cover.
    GeoCover { field: u32, cells: Vec<u64> },
    /// Conjunction; empty is All.
    And(Vec<Predicate>),
    /// Disjunction; empty is None.
    Or(Vec<Predicate>),
    /// SQL negation preserving unknown values.
    Not(Box<Predicate>),
}
/// Signed fixed-point comparison; Between includes both endpoints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CmpOp {
    /// Equal.
    Eq,
    /// Not equal.
    Ne,
    /// Less than.
    Lt,
    /// Less than or equal.
    Le,
    /// Greater than.
    Gt,
    /// Greater than or equal.
    Ge,
    /// Inclusive lower and upper bounds.
    Between,
}

/// Query boundary over shard-local columns; implementations must be thread safe.
pub trait ShardSource: Send + Sync {
    /// Enumerate candidate shards for the optional vessels and inclusive bucket range.
    fn shards(&self, vessels: Option<&[VesselOrd]>, from: BucketIx, to: BucketIx) -> Vec<ShardKey>;
    /// Return SQL-true local columns for a predicate; missing values preserve unknown internally.
    fn eval(&self, shard: ShardKey, p: &Predicate) -> Result<RoaringBitmap>; // local cols
    /// Materialize selected local columns and requested fields into an Arrow batch.
    fn read(&self, shard: ShardKey, cols: &RoaringBitmap, fields: &[u32]) -> Result<RecordBatch>;
    /// Compute a mergeable contribution for one query-time operation.
    fn agg(
        &self,
        shard: ShardKey,
        cols: &RoaringBitmap,
        field: u32,
        a: AggOp,
    ) -> Result<AggPartial>;
}
/// Ordered ingest boundary; apply/flush/seal semantics are frozen in spec 14.
pub trait ShardSink: Send {
    /// Apply one ordered record transaction through the WAL before publication.
    fn apply(&mut self, recs: &[BucketRecord]) -> Result<()>;
    /// Durably persist rows and catalog dependencies before retiring WAL records.
    fn flush(&mut self) -> Result<()>;
    /// Durably publish an immutable shard version and return its manifest entry.
    fn seal(&mut self, key: ShardKey) -> Result<ShardManifestEntry>;
}
/// Text hits mapped to global columns in an inclusive bucket range.
pub trait TextIndex: Send + Sync {
    /// Map documents of this kind onto global ColumnIds in the inclusive bucket range.
    fn match_buckets(
        &self,
        vessel: VesselOrd,
        kind: &str,
        q: &str,
        from: BucketIx,
        to: BucketIx,
    ) -> Result<RoaringTreemap>; // global ColumnIds
}

/// Query-time aggregation, distinct from the ingest-time `Agg` profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum AggOp {
    /// Count selected existing buckets; the field argument is ignored.
    CountAll,
    /// Count selected buckets with a value for the requested field.
    Count,
    /// Sum fixed-point integers before converting back to physical units.
    Sum,
    /// Minimum fixed-point integer.
    Min,
    /// Maximum fixed-point integer.
    Max,
}

/// Mergeable shard contribution. Sum uses i128 to accommodate u32 bucket counts.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum AggPartial {
    /// Bucket or non-null value count.
    Count(u64),
    /// Integer sum and non-null count; an empty input has count zero.
    Sum { sum: i128, count: u64 },
    /// Minimum; None means no non-null values.
    Min(Option<i64>),
    /// Maximum; None means no non-null values.
    Max(Option<i64>),
}

/// Identity of one immutable, sealed shard version.
/// Hash is the raw 32-byte BLAKE3 digest; see TransferIdentity for federation identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ShardManifestEntry {
    /// Vessel and shard number in this store's ordinal space.
    pub key: ShardKey,
    /// Monotonically increasing repair version, starting at one.
    pub version: u64,
    /// Inclusive first bucket covered by the shard.
    pub from: BucketIx,
    /// Inclusive last bucket covered by the shard.
    pub to: BucketIx,
    /// Total persisted shard bytes.
    pub bytes: u64,
    /// Canonical shard content digest.
    pub hash: [u8; 32],
}

/// Errors shared by contract implementations; I/O and Arrow errors retain causes.
#[derive(Debug)]
pub enum Error {
    /// Invalid caller data or configuration, with a diagnostic.
    InvalidInput(String),
    /// Addressing, scaling or arithmetic exceeded the representable range.
    Overflow(&'static str),
    /// Persistent data failed validation.
    Corrupt(String),
    /// A required catalog, field or shard entry was absent.
    NotFound(String),
    /// A requested capability cannot be implemented by this source.
    Unsupported(String),
    /// Store or transport I/O failed.
    Io(std::io::Error),
    /// Arrow batch construction or reading failed.
    Arrow(arrow_schema::ArrowError),
}

/// The result type used by all TI contract traits.
pub type Result<T> = std::result::Result<T, Error>;

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidInput(s) => write!(f, "invalid input: {s}"),
            Self::Overflow(s) => write!(f, "overflow: {s}"),
            Self::Corrupt(s) => write!(f, "corrupt data: {s}"),
            Self::NotFound(s) => write!(f, "not found: {s}"),
            Self::Unsupported(s) => write!(f, "unsupported: {s}"),
            Self::Io(e) => write!(f, "{e}"),
            Self::Arrow(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Arrow(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<arrow_schema::ArrowError> for Error {
    fn from(value: arrow_schema::ArrowError) -> Self {
        Self::Arrow(value)
    }
}
```

### [catalog.rs](../../crates/ti-contracts/src/catalog.rs)

```rust
use crate::{Error, FieldSpec, Result, VesselOrd};

/// Metadata needed to register a vessel or remap an imported vessel ordinal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VesselSpec {
    /// Canonical Signal K vessel context URN; never vessels.self.
    pub urn: String,
    /// Human-readable vessel name, if known.
    pub name: Option<String>,
    /// MMSI when known.
    pub mmsi: Option<String>,
}

/// Normalize source identity to the server sourceRef used by all catalog keys.
/// Prefer live $source, then Parquet source_label, then a source-object-derived
/// ID supplied by W3 using the pinned server getSourceId rules. Missing is None.
pub fn normalized_source_label(
    live_ref: Option<&str>,
    parquet_ref: Option<&str>,
    derived_ref: Option<&str>,
) -> Option<String> {
    [live_ref, parquet_ref, derived_ref]
        .into_iter()
        .flatten()
        .find(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Notification states accepted by the Signal K schema; normal is also the
/// explicit cleared state from the server v2 API (null clear remains Clear).
pub const NOTIFICATION_STATES: [&str; 6] =
    ["nominal", "normal", "alert", "warn", "alarm", "emergency"];

/// Ordered source preference for an exact Signal K path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcePriority {
    /// Exact base path.
    pub path: String,
    /// Most preferred source first; unknown sources follow in first-seen order.
    pub sources: Vec<String>,
}

/// Store-local, durable catalog boundary. Implementations serialize registrations.
/// IDs never change or get reused within a store. Registration is idempotent;
/// field metadata conflicts must return InvalidInput rather than silently change encoding.
pub trait Catalog: Send + Sync {
    /// Resolve/register a canonical URN, allocating a dense store-local ordinal.
    fn register_vessel(&self, vessel: &VesselSpec) -> Result<VesselOrd>;
    /// Look up a canonical URN by ordinal.
    fn vessel_urn(&self, vessel: VesselOrd) -> Result<String>;
    /// Register (path, agg) identity and encoding metadata; input id is ignored; returns stable field ID.
    fn register_field(&self, field: &FieldSpec) -> Result<u32>;
    /// Read field metadata with its assigned ID.
    fn field(&self, id: u32) -> Result<FieldSpec>;
    /// List fields in increasing field-ID order.
    fn fields(&self) -> Result<Vec<FieldSpec>>;
    /// Register a case-sensitive UTF-8 value in this field's dictionary.
    /// Valid for Set fields only; registration precedes emitting SetValue.
    fn register_set_value(&self, field: u32, value: &str) -> Result<u32>;
    /// Read a value from the field-local row-ID dictionary.
    fn set_value(&self, field: u32, row: u32) -> Result<String>;
    /// Replace an exact path's preferred-source ordering.
    fn set_source_priority(&self, priority: &SourcePriority) -> Result<()>;
    /// Read preferences; None means use first-seen ordering.
    fn source_priority(&self, path: &str) -> Result<Option<SourcePriority>>;
}

/// Remap an imported vessel by URN, never by a foreign store's ordinal.
/// The importer rewrites ordinal-bearing manifest keys, WAL records and ColumnIds.
pub fn remap_vessel(
    source: &dyn Catalog,
    destination: &dyn Catalog,
    foreign: VesselOrd,
) -> Result<VesselOrd> {
    destination.register_vessel(&VesselSpec {
        urn: source.vessel_urn(foreign)?,
        name: None,
        mmsi: None,
    })
}

/// Validate the deletion marker before a sink mutates state.
/// Clear must be rewrite-only and the sole value for its bucket/field group.
pub fn validate_clear_records(records: &[crate::BucketRecord]) -> Result<()> {
    for rec in records {
        if rec.value == crate::FieldValue::Clear
            && (!rec.rewrite
                || records
                    .iter()
                    .filter(|other| {
                        other.vessel == rec.vessel
                            && other.bucket == rec.bucket
                            && other.field == rec.field
                    })
                    .count()
                    != 1)
        {
            return Err(Error::InvalidInput(
                "FieldValue::Clear requires rewrite=true and a singleton group".into(),
            ));
        }
    }
    Ok(())
}

/// Check the ordinary-set invariant before publishing a shard: disjoint rows
/// whose union is exactly field presence. Multi-valued source sets use a different invariant.
pub fn validate_ordinary_set_rows(
    presence: &crate::RoaringBitmap,
    rows: &[crate::RoaringBitmap],
) -> Result<()> {
    let mut union = crate::RoaringBitmap::new();
    for row in rows {
        if !(row & &union).is_empty() {
            return Err(Error::Corrupt("ordinary set rows overlap".into()));
        }
        union |= row;
    }
    if &union != presence {
        return Err(Error::Corrupt(
            "ordinary set rows do not match presence".into(),
        ));
    }
    Ok(())
}
```

### [schemas.rs](../../crates/ti-contracts/src/schemas.rs)

```rust
use crate::{Error, FieldKind, FieldSpec, Result};
use arrow_schema::{DataType, Field, Schema, SchemaRef, TimeUnit};
use std::collections::BTreeSet;
use std::sync::Arc;

fn timestamp() -> DataType {
    DataType::Timestamp(TimeUnit::Second, Some("UTC".into()))
}
fn list(item: DataType) -> DataType {
    DataType::List(Arc::new(Field::new("item", item, false)))
}
fn schema(fields: Vec<Field>) -> SchemaRef {
    Arc::new(Schema::new(fields))
}

/// Telemetry columns: vessel URN and UTC bucket start, followed by field-ID order.
/// Fields are nullable; source sets are List<Utf8>, numeric BSIs are physical Float64.
/// Mean fields also expose their bare path as an alias. Virtual text-kind columns follow.
pub fn telemetry_schema(fields: &[FieldSpec]) -> Result<SchemaRef> {
    let mut ordered: Vec<_> = fields.iter().collect();
    ordered.sort_by_key(|f| f.id);
    let mut names = BTreeSet::from(["vessel".to_string(), "ts".to_string()]);
    let mut out = vec![
        Field::new("vessel", DataType::Utf8, false),
        Field::new("ts", timestamp(), false),
    ];
    for f in ordered {
        let name = match &f.agg {
            Some(agg) => format!(
                "{}@{}",
                f.path,
                match agg {
                    crate::Agg::Mean => "mean",
                    crate::Agg::Min => "min",
                    crate::Agg::Max => "max",
                    crate::Agg::Last => "last",
                    crate::Agg::Count => "count",
                    crate::Agg::Starts => "starts",
                    crate::Agg::Edges => "edges",
                }
            ),
            None => f.path.clone(),
        };
        if !names.insert(name.clone()) {
            return Err(Error::InvalidInput(format!(
                "telemetry.{name}: duplicate/reserved column"
            )));
        }
        let kind = match &f.kind {
            FieldKind::Presence => DataType::Boolean,
            FieldKind::Set if f.path.ends_with("$source") => list(DataType::Utf8),
            FieldKind::Set => DataType::Utf8,
            FieldKind::Bsi { .. } => DataType::Float64,
            FieldKind::Count => DataType::UInt64,
            FieldKind::Geo { .. } => list(DataType::UInt64),
        };
        out.push(Field::new(name, kind.clone(), true));
        if f.agg == Some(crate::Agg::Mean) {
            if !names.insert(f.path.clone()) {
                return Err(Error::InvalidInput(format!(
                    "telemetry.{}: alias collision",
                    f.path
                )));
            }
            out.push(Field::new(&f.path, kind, true));
        }
    }
    for name in ["notes", "logbook", "alerts"] {
        if !names.insert(name.into()) {
            return Err(Error::InvalidInput(format!(
                "telemetry.{name}: reserved text column"
            )));
        }
        out.push(Field::new(name, DataType::Utf8, true));
    }
    Ok(schema(out))
}

/// Documents handoff: stable ID, vessel URN, kind, time range, title/body and query-only score.
pub fn docs_schema() -> SchemaRef {
    schema(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("vessel", DataType::Utf8, false),
        Field::new("kind", DataType::Utf8, false),
        Field::new("ts_start", timestamp(), false),
        Field::new("ts_end", timestamp(), true),
        Field::new("title", DataType::Utf8, false),
        Field::new("body", DataType::Utf8, false),
        Field::new("score", DataType::Float64, true),
    ])
}

/// Vessel catalog; ordinal is store-local, urn is federation-stable.
pub fn vessels_schema() -> SchemaRef {
    schema(vec![
        Field::new("ord", DataType::UInt32, false),
        Field::new("urn", DataType::Utf8, false),
        Field::new("name", DataType::Utf8, true),
        Field::new("mmsi", DataType::Utf8, true),
        Field::new("first_seen", timestamp(), false),
        Field::new("last_seen", timestamp(), false),
    ])
}

/// Path catalog; agg/type are readable strings, scale/depth are nullable for categorical fields.
pub fn paths_schema() -> SchemaRef {
    schema(vec![
        Field::new("path", DataType::Utf8, false),
        Field::new("field", DataType::UInt32, false),
        Field::new("agg", DataType::Utf8, true),
        Field::new("type", DataType::Utf8, false),
        Field::new("units", DataType::Utf8, true),
        Field::new("scale", DataType::UInt8, true),
        Field::new("depth", DataType::UInt8, true),
        Field::new("description", DataType::Utf8, true),
        Field::new("first_seen", timestamp(), false),
        Field::new("last_seen", timestamp(), false),
    ])
}

/// Shard catalog; vessel is a URN, hash a lowercase hexadecimal digest when sealed.
pub fn shards_schema() -> SchemaRef {
    schema(vec![
        Field::new("vessel", DataType::Utf8, false),
        Field::new("shard_no", DataType::UInt32, false),
        Field::new("ts_from", timestamp(), false),
        Field::new("ts_to", timestamp(), false),
        Field::new("sealed", DataType::Boolean, false),
        Field::new("bytes", DataType::UInt64, false),
        Field::new("hash", DataType::Utf8, true),
    ])
}
```

### [engine.rs](../../crates/ti-contracts/src/engine.rs)

```rust
use crate::{RecordBatch, Result};
use arrow_schema::SchemaRef;

/// Shared synchronous fixture facade. Runtime adapters may schedule it asynchronously.
/// Query batches must use one stable schema and physical units; no SQL mutations.
pub trait TiEngine: Send + Sync {
    /// Resolve a table's schema (telemetry, docs, raw, vessels, paths or shards).
    fn schema(&self, table: &str) -> Result<SchemaRef>;
    /// Execute read-only SQL; max_rows applies to the returned rows.
    fn query(&self, sql: &str, max_rows: usize) -> Result<QueryResult>;
    /// Return the query plan and pushdown diagnostics without executing it.
    fn explain(&self, sql: &str) -> Result<String>;
    /// Read operational status.
    fn status(&self) -> Result<EngineStatus>;
}

/// Result shared by MCP/HTTP/CLI fixtures; production streaming may adapt batches.
#[derive(Debug, Clone, PartialEq)]
pub struct QueryResult {
    /// Arrow output batches with the same schema.
    pub batches: Vec<RecordBatch>,
    /// True if the row cap omitted remaining results.
    pub truncated: bool,
    /// Query elapsed milliseconds.
    pub elapsed_ms: u64,
    /// One-line pushdown summary.
    pub pushdown: String,
}

/// Operational counters in the initial engine facade.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineStatus {
    /// Maximum current ingest lag in milliseconds.
    pub ingest_lag_ms: u64,
    /// Bytes in pending WAL files.
    pub wal_bytes: u64,
    /// Number of mutable shards.
    pub open_shards: u64,
    /// Number of sealed shards.
    pub sealed_shards: u64,
    /// Last completed sync per vessel URN, in Unix seconds; None if never.
    pub last_sync: Vec<(String, Option<i64>)>,
}

/// Document ingestion metadata shared by W5 and the docs table.
/// Query score is not stored: it is attached only to query results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    /// Stable resource/plugin-qualified ID; updates replace the same ID.
    pub id: String,
    /// Canonical vessel URN.
    pub vessel: String,
    /// notes, logbook or alerts.
    pub kind: String,
    /// Inclusive start Unix second.
    pub ts_start: i64,
    /// Exclusive end, or None for a point document.
    pub ts_end: Option<i64>,
    /// Display title.
    pub title: String,
    /// Searchable document body.
    pub body: String,
}

impl Document {
    /// Check the supported time domain and nonempty interval.
    pub fn validate(&self) -> Result<()> {
        if self.id.is_empty() {
            return Err(crate::Error::InvalidInput("docs.id".into()));
        }
        if !self.vessel.starts_with("vessels.urn:") {
            return Err(crate::Error::InvalidInput("docs.vessel".into()));
        }
        if !["notes", "logbook", "alerts"].contains(&self.kind.as_str()) {
            return Err(crate::Error::InvalidInput("docs.kind".into()));
        }
        if self.ts_start < crate::EPOCH {
            return Err(crate::Error::InvalidInput("docs.ts_start: pre-2020".into()));
        }
        if self.ts_end.is_some_and(|end| end <= self.ts_start) {
            return Err(crate::Error::InvalidInput("docs.ts_end".into()));
        }
        Ok(())
    }
}

/// W5 document handoff; upsert/delete invalidate affected text-cache shards.
pub trait DocumentIndex: crate::TextIndex {
    /// Persist/index a validated document by ID.
    fn upsert(&self, doc: &Document) -> Result<()>;
    /// Delete the vessel's document by ID; missing IDs are idempotent.
    fn delete(&self, vessel: &str, id: &str) -> Result<()>;
    /// Return document rows, with nullable score when q is None.
    fn documents(
        &self,
        vessel: Option<&str>,
        kind: Option<&str>,
        q: Option<&str>,
    ) -> Result<Vec<RecordBatch>>;
}
```

### [envelopes.rs](../../crates/ti-contracts/src/envelopes.rs)

```rust
use crate::{BucketIx, Error, Result};
use std::collections::BTreeSet;

/// Format version for WAL and shard envelopes; readers reject unknown versions.
pub const FORMAT_VERSION: u16 = 1;
/// WAL segment magic, independent of the record payload codec.
pub const WAL_MAGIC: [u8; 8] = *b"LUMETIW1";
/// Shard file magic; payload contains ordered roaring-portable rows.
pub const SHARD_MAGIC: [u8; 8] = *b"LUMETIS1";

/// Versioned WAL segment header. Binary representation is magic, version and
/// vessel-URN byte length (u32 LE), followed by URN UTF-8 bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalHeader {
    /// Canonical vessel URN.
    pub vessel_urn: String,
    /// Envelope version.
    pub version: u16,
}

/// Record frame metadata: payload length u32 LE, sequence u64 LE, CRC32 u32 LE.
/// CRC covers sequence bytes followed by payload bytes. Payload codec is fixed in spec 14.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalRecordHeader {
    /// Encoded payload bytes.
    pub payload_len: u32,
    /// Monotonic sequence within a vessel's WAL, starting at one.
    pub sequence: u64,
    /// IEEE CRC32 over sequence and payload.
    pub crc32: u32,
}

/// Portable shard-file header. Identity uses URN, never a foreign ordinal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShardFileHeader {
    /// Format version.
    pub version: u16,
    /// Canonical vessel URN.
    pub vessel_urn: String,
    /// 65,536-bucket shard number.
    pub shard: u32,
    /// Store-local field ID, resolved using the transferred catalog snapshot.
    pub field: u32,
    /// Fixed bucket width in seconds.
    pub width_seconds: u64,
    /// Number of portable roaring row payloads.
    pub row_count: u32,
}

/// Federation identity for one immutable shard version and its catalog snapshot.
/// hash identifies canonical field contents; catalog_hash prevents ambiguous row IDs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferIdentity {
    /// Canonical URN used to remap the vessel.
    pub vessel_urn: String,
    /// Shard number.
    pub shard: u32,
    /// Repair version, starting at one.
    pub version: u64,
    /// Bucket width.
    pub width_seconds: u64,
    /// Inclusive first bucket.
    pub from: BucketIx,
    /// Inclusive last bucket.
    pub to: BucketIx,
    /// Raw BLAKE3 digest over the canonical field stream.
    pub hash: [u8; 32],
    /// BLAKE3 digest of canonical catalog snapshot required to decode fields/rows.
    pub catalog_hash: [u8; 32],
}

fn urn_bytes(urn: &str) -> Result<Vec<u8>> {
    if !urn.starts_with("vessels.urn:") {
        return Err(Error::InvalidInput(
            "vessel_urn must be a canonical Signal K URN".into(),
        ));
    }
    let len = u32::try_from(urn.len()).map_err(|_| Error::Overflow("vessel_urn length"))?;
    let mut out = len.to_le_bytes().to_vec();
    out.extend_from_slice(urn.as_bytes());
    Ok(out)
}

impl WalHeader {
    /// Encode the stable header; unknown versions and aliases are rejected.
    pub fn encode(&self) -> Result<Vec<u8>> {
        if self.version != FORMAT_VERSION {
            return Err(Error::Unsupported("wal.version".into()));
        }
        let mut out = WAL_MAGIC.to_vec();
        out.extend_from_slice(&self.version.to_le_bytes());
        out.extend(urn_bytes(&self.vessel_urn)?);
        Ok(out)
    }
}

impl WalRecordHeader {
    /// Encode a frame header. Sequence zero is reserved for no-record checkpoints.
    pub fn encode(&self) -> Result<[u8; 16]> {
        if self.sequence == 0 || self.payload_len == 0 {
            return Err(Error::InvalidInput(
                "wal.record sequence/payload_len must be positive".into(),
            ));
        }
        let mut out = [0; 16];
        out[..4].copy_from_slice(&self.payload_len.to_le_bytes());
        out[4..12].copy_from_slice(&self.sequence.to_le_bytes());
        out[12..].copy_from_slice(&self.crc32.to_le_bytes());
        Ok(out)
    }
}

impl ShardFileHeader {
    /// Encode magic, version, URN, shard, field, width and row count in little endian.
    pub fn encode(&self) -> Result<Vec<u8>> {
        if self.version != FORMAT_VERSION {
            return Err(Error::Unsupported("shard.version".into()));
        }
        if self.width_seconds == 0 || self.shard > 65535 {
            return Err(Error::InvalidInput("shard.width_seconds/shard".into()));
        }
        let mut out = SHARD_MAGIC.to_vec();
        out.extend_from_slice(&self.version.to_le_bytes());
        out.extend(urn_bytes(&self.vessel_urn)?);
        out.extend_from_slice(&self.shard.to_le_bytes());
        out.extend_from_slice(&self.field.to_le_bytes());
        out.extend_from_slice(&self.width_seconds.to_le_bytes());
        out.extend_from_slice(&self.row_count.to_le_bytes());
        Ok(out)
    }
}

impl TransferIdentity {
    /// Check coverage and namespace before import.
    pub fn validate(&self) -> Result<()> {
        urn_bytes(&self.vessel_urn)?;
        if self.version == 0
            || self.width_seconds == 0
            || self.from > self.to
            || (self.from >> 16) != self.shard
            || (self.to >> 16) != self.shard
        {
            return Err(Error::InvalidInput(
                "transfer.version/width_seconds/coverage".into(),
            ));
        }
        Ok(())
    }
}

/// Exact bytes a hasher consumes: domain tag, then ascending field IDs with
/// u32 LE ID, u64 LE file length, and exact versioned file bytes.
/// This helper does not hash; W2/W8 stream the same ordering through BLAKE3.
pub fn canonical_shard_input(files: &[(u32, Vec<u8>)]) -> Result<Vec<u8>> {
    let mut ordered: Vec<_> = files.iter().collect();
    ordered.sort_by_key(|(id, _)| *id);
    let mut seen = BTreeSet::new();
    let mut out = b"LumeTI/shard/v1\0".to_vec();
    for (id, bytes) in ordered {
        if !seen.insert(*id) {
            return Err(Error::InvalidInput("shard.field: duplicate".into()));
        }
        out.extend_from_slice(&id.to_le_bytes());
        out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        out.extend_from_slice(bytes);
    }
    Ok(out)
}
```

### [config.rs](../../crates/ti-contracts/src/config.rs)

```rust
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;

/// Serializable ti.toml schema. Unknown keys are errors; omitted keys use defaults.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TiConfig {
    /// Fixed store bucket width; changing it requires rebuilding the store.
    pub width_seconds: u64,
    /// Local store root.
    pub store_root: String,
    /// Edge retention in years.
    pub retention_years: u32,
    /// Maximum registered fields per vessel.
    pub field_cap: u32,
    /// Signal K connection.
    pub signal_k: SignalKConfig,
    /// Allow/deny glob patterns, deny takes precedence.
    pub allow_paths: Vec<String>,
    /// Default deny patterns.
    pub deny_paths: Vec<String>,
    /// Named aggregate profiles.
    pub profiles: AggregateProfiles,
    /// Unit to decimal scale registry.
    pub unit_scales: BTreeMap<String, u8>,
    /// Exact-path scale overrides.
    pub path_scales: BTreeMap<String, u8>,
    /// Exact-path preferred sources, most preferred first.
    pub source_priorities: BTreeMap<String, Vec<String>>,
    /// Derived transition/edge/notification rules.
    pub derived: Vec<DerivedRule>,
    /// Listener configuration.
    pub bind: BindConfig,
    /// Optional bearer/NUTS/SCRAM configuration.
    pub auth: AuthConfig,
    /// Query resource limits.
    pub query: QueryLimits,
}

/// Device token comes from a one-time Signal K access request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SignalKConfig {
    /// WebSocket stream URL.
    pub url: String,
    /// Device token; never a Signal K user password.
    pub token: Option<String>,
    /// Server-provided access-request polling href, preserved verbatim.
    pub access_request_href: Option<String>,
    /// True when the access-request endpoint returned 404 with security disabled.
    pub no_auth_required: bool,
}

/// Profile names and aggregate columns enabled for numeric paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AggregateProfiles {
    /// Normal profile.
    pub default: Vec<String>,
    /// Paths with median sample interval >= W.
    pub slow: Vec<String>,
    /// Extra aggregates explicitly enabled by the operator.
    pub opt_in: Vec<String>,
}

/// Listener inside the deployment namespace; restrict host publication to LAN on boats.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BindConfig {
    /// IPv4/IPv6 address, 0.0.0.0 inside containers.
    pub address: String,
    /// HTTP/MCP port.
    pub http_port: u16,
    /// Optional PostgreSQL port.
    pub pg_port: Option<u16>,
}

/// Shore NUTS integration and optional boat credentials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct AuthConfig {
    /// Enable the existing shore NUTS auth adapter.
    pub nuts: bool,
    /// Optional HTTP bearer token.
    pub bearer_token: Option<String>,
    /// PostgreSQL SCRAM users.
    pub scram_users: Vec<ScramUser>,
}

/// PostgreSQL SCRAM verifier; no plaintext password is stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScramUser {
    /// Unique username.
    pub username: String,
    /// SCRAM-SHA-256 verifier string; cryptographic validation belongs to pgwire.
    pub verifier: String,
}

/// Boat resource/admission defaults; lower memory_bytes to 512 MiB on Pi 4.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QueryLimits {
    /// Per-query timeout.
    pub timeout_seconds: u64,
    /// Per-query memory pool bytes.
    pub memory_bytes: u64,
    /// Whole-unit memory limit, including ingest.
    pub unit_memory_bytes: u64,
    /// Parallel query partitions.
    pub target_partitions: usize,
    /// Heavy-query concurrency.
    pub heavy_queries: usize,
    /// Pause background work above this temperature.
    pub thermal_celsius: u16,
}

/// Derived event rule kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DerivedKind {
    /// Count transitions into a configured state.
    Transition,
    /// Count rising edges.
    RisingEdge,
    /// Notification state and raise count.
    Notification,
}

/// One operator rule applied at bucket close.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DerivedRule {
    /// Source path/glob.
    pub path: String,
    /// Destination field/path.
    pub output: String,
    /// Rule operation.
    pub kind: DerivedKind,
    /// Required for transition, e.g. started; otherwise absent.
    pub state: Option<String>,
}

impl Default for SignalKConfig {
    fn default() -> Self {
        Self {
            url: "ws://localhost:3000/signalk/v1/stream?subscribe=none".into(),
            token: None,
            access_request_href: None,
            no_auth_required: false,
        }
    }
}
impl Default for AggregateProfiles {
    fn default() -> Self {
        Self {
            default: vec!["mean".into(), "min".into(), "max".into()],
            slow: vec!["last".into()],
            opt_in: vec![],
        }
    }
}
impl Default for BindConfig {
    fn default() -> Self {
        Self {
            address: "0.0.0.0".into(),
            http_port: 8080,
            pg_port: Some(5432),
        }
    }
}
impl Default for QueryLimits {
    fn default() -> Self {
        Self {
            timeout_seconds: 30,
            memory_bytes: 1 << 30,
            unit_memory_bytes: 1536 << 20,
            target_partitions: 2,
            heavy_queries: 1,
            thermal_celsius: 75,
        }
    }
}
impl Default for TiConfig {
    fn default() -> Self {
        Self {
            width_seconds: 10,
            store_root: "./ti".into(),
            retention_years: 2,
            field_cap: 2000,
            signal_k: SignalKConfig::default(),
            allow_paths: vec![],
            deny_paths: vec!["*.ais.*".into(), "design.*".into()],
            profiles: AggregateProfiles::default(),
            unit_scales: [
                ("rad", 4),
                ("m/s", 3),
                ("K", 2),
                ("V", 3),
                ("A", 2),
                ("W", 1),
                ("Pa", 0),
                ("ratio", 4),
                ("m", 2),
                ("Hz", 1),
                ("s", 0),
                ("J", 0),
                ("C", 0),
                ("lat/lon", 7),
                ("deg", 7),
                ("unknown", 3),
            ]
            .into_iter()
            .map(|(k, v)| (k.into(), v))
            .collect(),
            path_scales: BTreeMap::new(),
            source_priorities: BTreeMap::new(),
            derived: vec![],
            bind: BindConfig::default(),
            auth: AuthConfig::default(),
            query: QueryLimits::default(),
        }
    }
}

fn invalid(key: &str, message: &str) -> Error {
    Error::InvalidInput(format!("{key}: {message}"))
}

impl TiConfig {
    /// Parse TOML, then validate with key-qualified diagnostics.
    pub fn from_toml(input: &str) -> Result<Self> {
        let mut config: Self =
            toml::from_str(input).map_err(|e| invalid("ti.toml", &e.to_string()))?;
        for (unit, scale) in Self::default().unit_scales {
            config.unit_scales.entry(unit).or_insert(scale);
        }
        config.validate()?;
        Ok(config)
    }

    /// Reject incompatible width when opening an existing store.
    pub fn validate_store_width(&self, existing_width: u64) -> Result<()> {
        if self.width_seconds != existing_width {
            return Err(invalid(
                "width_seconds",
                "changing store width requires a rebuild",
            ));
        }
        self.validate()
    }

    /// Validate all configured limits, scale tables and names.
    pub fn validate(&self) -> Result<()> {
        if self.width_seconds == 0 {
            return Err(invalid("width_seconds", "must be positive"));
        }
        if self.store_root.trim().is_empty() {
            return Err(invalid("store_root", "must not be empty"));
        }
        if self.retention_years == 0 {
            return Err(invalid("retention_years", "must be positive"));
        }
        if self.field_cap == 0 {
            return Err(invalid("field_cap", "must be positive"));
        }
        let authority = self
            .signal_k
            .url
            .strip_prefix("ws://")
            .or_else(|| self.signal_k.url.strip_prefix("wss://"))
            .and_then(|rest| rest.split(['/', '?', '#']).next());
        if authority.is_none_or(str::is_empty) || self.signal_k.url.chars().any(char::is_whitespace)
        {
            return Err(invalid(
                "signal_k.url",
                "requires ws:// or wss:// and a nonempty authority without whitespace",
            ));
        }
        if self.signal_k.token.as_ref().is_some_and(|x| x.is_empty()) {
            return Err(invalid("signal_k.token", "must not be empty"));
        }
        if self
            .signal_k
            .access_request_href
            .as_ref()
            .is_some_and(|x| x.trim().is_empty())
        {
            return Err(invalid("signal_k.access_request_href", "must not be empty"));
        }
        for (key, values) in [
            ("allow_paths", &self.allow_paths),
            ("deny_paths", &self.deny_paths),
        ] {
            if values.iter().any(|x| x.trim().is_empty()) {
                return Err(invalid(key, "empty pattern"));
            }
        }
        for (key, values) in [
            ("profiles.default", &self.profiles.default),
            ("profiles.slow", &self.profiles.slow),
            ("profiles.opt_in", &self.profiles.opt_in),
        ] {
            let mut seen = BTreeSet::new();
            for agg in values {
                if !["mean", "min", "max", "last", "count"].contains(&agg.as_str())
                    || !seen.insert(agg)
                {
                    return Err(invalid(key, "unknown or duplicate aggregate"));
                }
            }
            if key != "profiles.opt_in" && values.is_empty() {
                return Err(invalid(key, "must not be empty"));
            }
        }
        for (root, scales) in [
            ("unit_scales", &self.unit_scales),
            ("path_scales", &self.path_scales),
        ] {
            for (path, scale) in scales {
                if path.trim().is_empty() || *scale > 18 {
                    return Err(invalid(
                        &format!("{root}.{path}"),
                        "requires nonempty key and scale 0..=18",
                    ));
                }
            }
        }
        for (path, sources) in &self.source_priorities {
            let mut seen = BTreeSet::new();
            if path.is_empty()
                || sources.is_empty()
                || sources.iter().any(|s| s.is_empty() || !seen.insert(s))
            {
                return Err(invalid(
                    &format!("source_priorities.{path}"),
                    "requires unique nonempty sources",
                ));
            }
        }
        for (i, rule) in self.derived.iter().enumerate() {
            if rule.path.is_empty() || rule.output.is_empty() {
                return Err(invalid(
                    &format!("derived[{i}].path/output"),
                    "must not be empty",
                ));
            }
            if (rule.kind == DerivedKind::Transition
                && rule.state.as_ref().is_none_or(|s| s.is_empty()))
                || (rule.kind != DerivedKind::Transition && rule.state.is_some())
            {
                return Err(invalid(
                    &format!("derived[{i}].state"),
                    "only transitions require a nonempty state",
                ));
            }
        }
        if self.bind.address.parse::<IpAddr>().is_err() {
            return Err(invalid("bind.address", "must be an IPv4/IPv6 address"));
        }
        if self.bind.http_port == 0 {
            return Err(invalid("bind.http_port", "must be nonzero"));
        }
        if self.bind.pg_port == Some(0) || self.bind.pg_port == Some(self.bind.http_port) {
            return Err(invalid(
                "bind.pg_port",
                "must be nonzero and distinct from HTTP",
            ));
        }
        if self
            .auth
            .bearer_token
            .as_ref()
            .is_some_and(|x| x.is_empty())
        {
            return Err(invalid("auth.bearer_token", "must not be empty"));
        }
        let mut users = BTreeSet::new();
        for (i, user) in self.auth.scram_users.iter().enumerate() {
            if user.username.is_empty() || !users.insert(&user.username) {
                return Err(invalid(
                    &format!("auth.scram_users[{i}].username"),
                    "must be nonempty and unique",
                ));
            }
            if !user.verifier.starts_with("SCRAM-SHA-256$") {
                return Err(invalid(
                    &format!("auth.scram_users[{i}].verifier"),
                    "requires a SCRAM-SHA-256 verifier",
                ));
            }
        }
        for (key, value) in [
            ("query.timeout_seconds", self.query.timeout_seconds),
            ("query.memory_bytes", self.query.memory_bytes),
            ("query.unit_memory_bytes", self.query.unit_memory_bytes),
            (
                "query.target_partitions",
                self.query.target_partitions as u64,
            ),
            ("query.heavy_queries", self.query.heavy_queries as u64),
            ("query.thermal_celsius", self.query.thermal_celsius as u64),
        ] {
            if value == 0 {
                return Err(invalid(key, "must be positive"));
            }
        }
        if self.query.memory_bytes > self.query.unit_memory_bytes {
            return Err(invalid(
                "query.memory_bytes",
                "exceeds whole-unit memory cap",
            ));
        }
        Ok(())
    }
}
```

### [helpers.rs](../../crates/ti-contracts/src/helpers.rs)

```rust
use crate::{BucketIx, ColumnId, Error, Result, ShardKey, VesselOrd};

/// 2020-01-01T00:00:00Z, in Unix seconds.
pub const EPOCH: i64 = 1_577_836_800;

/// Compute a bucket from Unix seconds and a positive whole-second width.
/// Rejects timestamps before EPOCH and buckets outside the u32 column space.
pub fn bucket_of(timestamp: i64, width: u64) -> Result<BucketIx> {
    if width == 0 {
        return Err(Error::InvalidInput("bucket width must be positive".into()));
    }
    let elapsed = (timestamp as i128) - (EPOCH as i128);
    if elapsed < 0 {
        return Err(Error::InvalidInput("timestamp precedes EPOCH".into()));
    }
    u32::try_from(elapsed / (width as i128)).map_err(|_| Error::Overflow("bucket index"))
}

/// Encode a vessel and bucket without losing either 32-bit component.
pub const fn column_id(vessel: VesselOrd, bucket: BucketIx) -> ColumnId {
    ((vessel as u64) << 32) | (bucket as u64)
}

/// Address the vessel's group of 65,536 consecutive buckets.
pub const fn shard_key(vessel: VesselOrd, bucket: BucketIx) -> ShardKey {
    ShardKey {
        vessel,
        shard: bucket >> 16,
    }
}

/// Return a local column in 0..=65,535.
pub const fn local_col(bucket: BucketIx) -> u32 {
    bucket & 0xffff
}

fn factor(scale: u8) -> Result<f64> {
    if scale > 18 {
        return Err(Error::InvalidInput(
            "fixed-point scale must be in 0..=18".into(),
        ));
    }
    Ok(10_f64.powi(i32::from(scale)))
}

/// Round a finite physical value to a signed fixed-point integer.
/// Halfway ties round away from zero. Rejects unsupported scales and overflow.
pub fn to_fixed(value: f64, scale: u8) -> Result<i64> {
    let factor = factor(scale)?;
    if !value.is_finite() {
        return Err(Error::InvalidInput(
            "fixed-point value must be finite".into(),
        ));
    }
    let rounded = (value * factor).round();
    // i64::MAX as f64 rounds to 2^63, which is already out of range.
    if !rounded.is_finite()
        || !(-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&rounded)
    {
        return Err(Error::Overflow("fixed-point integer"));
    }
    Ok(rounded as i64)
}

/// Convert a fixed-point integer to physical units.
/// f64 may approximate integers whose magnitude exceeds 2^53.
pub fn from_fixed(value: i64, scale: u8) -> Result<f64> {
    Ok((value as f64) / factor(scale)?)
}

/// Maximum quantization error in physical units at this scale (half a unit).
/// This is not a floating-point representation error budget.
pub fn tolerance(scale: u8) -> Result<f64> {
    Ok(0.5 / factor(scale)?)
}
```

## Rules for every agent PR

- `cargo test` passes. Strict clippy and formatting apply to TI crates (`cargo clippy -p ti-contracts --all-targets -- -D warnings`, `cargo fmt -p ti-contracts --check` for W0). The integrator accepted the existing root formatter churn and unused-assignment warning as outside TI scope; do not reformat root sources in lane PRs.
- No `unsafe` outside `ti-store` mmap code, which needs a justifying comment.
- Every new predicate or aggregate implementation adds at least one golden query and one proptest. Shared contracts add focused boundary tests.
- Performance-sensitive changes attach a `ti-bench` before/after table.
- No new runtime dependency without a line in the decisions log. C bindings are allowed only for `croaring`, after the M4 evaluation.
- Lanes integrate on `main` daily behind the `ti` feature. The integrator runs the full corpus nightly and files regressions back to its owning lane.
