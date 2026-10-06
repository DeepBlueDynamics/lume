# 10. Frozen contracts and PR rules

Spec pp. 20–24. Owner: [W0](../lanes/W0-contracts.md).

Nine crates, nine agent lanes. W0 freezes the shared contracts in week 1; after
that every lane builds against traits and fixtures and never waits on another
lane's implementation. All crates live in the Lume workspace under `crates/ti-*`.

## Contracts (frozen in week 1)

Changing anything below requires a contracts PR approved by the integrator. Each
lane mocks the others behind these.

```rust
pub type VesselOrd = u32;
pub type BucketIx  = u32;               // floor((t - EPOCH) / W)
pub type ColumnId  = u64;               // (vessel as u64) << 32 | bucket

#[derive(Clone, Copy, Hash, Eq, PartialEq, Ord, PartialOrd)]
pub struct ShardKey { pub vessel: VesselOrd, pub shard: u32 } // shard = bucket >> 16

pub enum FieldKind { Presence, Set, Bsi { scale: u8 }, Count, Geo { res: u8 } }

pub struct FieldSpec {
    pub id: u32, pub path: String, pub agg: Option<Agg>, pub kind: FieldKind,
    pub units: Option<String>,
}
pub enum Agg { Mean, Min, Max, Last, Count, Starts, Edges }

/// Output of the bucketer; input to the store.
pub struct BucketRecord {
    pub vessel: VesselOrd, pub bucket: BucketIx, pub field: u32,
    pub value: FieldValue, pub rewrite: bool,
}
pub enum FieldValue { Present, SetValue(u32 /* row id */), Int(i64), Cells(Vec<u64>) }

/// The only language between SQL and bitmaps.
pub enum Predicate {
    All, None,
    Present(u32),
    SetEq { field: u32, rows: Vec<u32>, negate: bool },
    BsiCmp { field: u32, op: CmpOp, lo: i64, hi: Option<i64> }, // hi for BETWEEN
    TsRange { from: BucketIx, to: BucketIx },                   // inclusive
    Text { kind: String, query: String },
    GeoCover { field: u32, cells: Vec<u64> },
    And(Vec<Predicate>), Or(Vec<Predicate>), Not(Box<Predicate>),
}
pub enum CmpOp { Eq, Ne, Lt, Le, Gt, Ge, Between }

pub trait ShardSource: Send + Sync {
    fn shards(&self, vessels: Option<&[VesselOrd]>, from: BucketIx, to: BucketIx) -> Vec<ShardKey>;
    fn eval(&self, shard: ShardKey, p: &Predicate) -> Result<RoaringBitmap>; // local cols
    fn read(&self, shard: ShardKey, cols: &RoaringBitmap, fields: &[u32]) -> Result<RecordBatch>;
    fn agg(&self, shard: ShardKey, cols: &RoaringBitmap, field: u32, a: AggOp) -> Result<AggPartial>;
}
pub trait ShardSink: Send {
    fn apply(&mut self, recs: &[BucketRecord]) -> Result<()>;
    fn flush(&mut self) -> Result<()>;
    fn seal(&mut self, key: ShardKey) -> Result<ShardManifestEntry>;
}
pub trait TextIndex: Send + Sync {
    fn match_buckets(&self, vessel: VesselOrd, kind: &str, q: &str, from: BucketIx, to: BucketIx)
        -> Result<RoaringTreemap>; // global ColumnIds
}
```

Note: `AggOp`, `AggPartial`, `ShardManifestEntry` and the `Result` error type are
referenced but not defined in the spec — W0 must define them (see
[W0](../lanes/W0-contracts.md)).

## Rules for every agent PR

- `cargo test`, `cargo clippy -- -D warnings`, and `cargo fmt --check` all pass. No `unsafe` outside `ti-store` mmap code, and that code needs a justifying comment.
- Every new predicate or aggregate path adds at least one golden query and one proptest.
- Performance-sensitive changes attach a `ti-bench` before/after table to the PR.
- No new runtime dependency without a line in the decisions log. C bindings allowed only for `croaring`, and only after the M4 evaluation.
- Lanes integrate on `main` daily behind a feature flag (`ti`). The integrator agent runs the full corpus nightly and files regressions back to the owning lane.
