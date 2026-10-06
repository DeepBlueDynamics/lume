//! Shared types and boundaries for Lume TI lanes.
//!
//! Proposed behavioral rules are in plan/spec/14-semantics.md.
//! This crate contains contracts and checked addressing/conversion helpers only.

#![forbid(unsafe_code)]

pub use arrow_array::RecordBatch;
pub use arrow_schema;
pub use roaring::{RoaringBitmap, RoaringTreemap};

mod helpers;
pub use helpers::*;

/// Dense store-local vessel ordinal, resolved from a Signal K URN.
pub type VesselOrd = u32;
/// Time bucket index relative to EPOCH, with a fixed deployment width.
pub type BucketIx = u32; // floor((t - EPOCH) / W)
/// Global column identity: vessel in the high 32 bits, bucket in the low bits.
pub type ColumnId = u64; // (vessel as u64) << 32 | bucket

/// One vessel and a 65,536-bucket window.
#[derive(Clone, Copy, Debug, Hash, Eq, PartialEq, Ord, PartialOrd)]
pub struct ShardKey {
    /// Store-local vessel ordinal.
    pub vessel: VesselOrd,
    /// Bucket index shifted right by 16.
    pub shard: u32,
} // shard = bucket >> 16

/// Storage encoding for a field.
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
pub enum FieldValue {
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
/// Ordered ingest boundary; apply/flush/seal semantics are proposed in spec 14.
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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
#[derive(Clone, Debug, Eq, PartialEq)]
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
/// Hash is the raw 32-byte BLAKE3 digest; envelope rules remain a part-2 proposal.
#[derive(Clone, Debug, Eq, PartialEq)]
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
