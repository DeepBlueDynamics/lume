# W0 — `ti-contracts` (week 1, gates M0)

Depends on: nothing. Everyone else depends on this.
Spec: [10-contracts](../spec/10-contracts.md), [05-data-model](../spec/05-data-model.md), [13-benchmarks](../spec/13-benchmarks.md).

## Owns

Shared types and traits, `ti.toml` schema, golden corpus, synthetic generator.

## Tasks

### Workspace bootstrap
- [ ] Convert the repo to a Cargo workspace with `crates/ti-*` members, keeping the existing `lume` crate building unchanged (see [repo-fit](../repo-fit.md) §1).
  Recommended layout (review): root `lume` package stays at `.` plus explicit `crates/ti-*` members, `resolver = "2"`, `default-members = ["."]`. TI dependencies on the root are optional and enabled by `ti`. Avoid a TI→root→TI cycle: if `ti-text` needs Lume search, move the search primitives into a lower-level member rather than depending on the root crate. This keeps `src/main.rs` and the release binary paths unchanged.
- [ ] Add the `ti` cargo feature flag on the `lume` binary; TI code compiles only behind it.
- [ ] CI: add `cargo fmt --check` and `cargo clippy -- -D warnings` for `crates/ti-*` (the existing crate's clippy stays informational until cleaned).

### Contract crate
- [ ] `VesselOrd`, `BucketIx`, `ColumnId`, `ShardKey`, `FieldKind`, `FieldSpec`, `Agg`, `BucketRecord`, `FieldValue`, `Predicate`, `CmpOp` exactly as spec'd, with doc comments.
- [ ] Traits `ShardSource`, `ShardSink`, `TextIndex`.
- [ ] **Define what the spec leaves undefined:** `AggOp`, `AggPartial`, `ShardManifestEntry`, the crate `Result`/error type. Record each in the decisions log (D8+).
- [ ] Column-space helpers: `EPOCH`, `bucket_of(t, W)`, `column_id(vessel, b)`, `shard_key(vessel, b)`, `local_col(b)`. Unit-tested at the edges (epoch, shard boundaries, u32 max).
- [ ] Fixed-point helpers: `to_fixed(v, scale)`, `from_fixed(i, scale)`, the oracle tolerance constant.
- [ ] Choose the roaring implementation the contracts expose (`RoaringBitmap`/`RoaringTreemap` from the `roaring` crate is implied) — decisions log entry required ([repo-fit](../repo-fit.md) §2).
- [ ] Arrow version pin for `RecordBatch` (must match the pinned DataFusion major) — decisions log entry.

### Semantics to freeze (gaps found in review, not in the PDF)
- [ ] **Null and negation semantics.** Define the universe for `All` and `Not` (existing buckets only?), SQL three-valued logic for `!=` / `NOT IN` / compound `NOT`, and multi-valued set fields. The IR has no unknown mask, and `!=` is presence ANDNOT row.
- [ ] **Catalog and dictionary interface.** Stable field IDs and set row IDs, plus how W3 registers a newly seen string value or path before emitting `SetValue(row)`. Also source-priority metadata and URN→ordinal remapping rules for shore import.
- [ ] **Arrow schemas.** `RecordBatch` schema, types, nullability and ordering for `telemetry`, `docs` and catalogs. Vessel and ts columns. How multi-valued `$source` is represented. The `docs`/`score` handoff W5→W4. A minimal `TiEngine` facade so W4, W5 and W7 can share fixtures.
- [ ] **Rewrite and durability.** Transaction boundary for multi-record rewrites (several `SetValue`/`Cells` per bucket), when to clear vs accumulate, partial-backfill merging, and what `apply`/`flush`/`seal` each guarantee durably. A single `rewrite: bool` doesn't cover these.
- [ ] **Numeric edge cases.** Fixed-point ties, NaN, ±inf and overflow; pre-epoch timestamps; zero-width ranges; whether text time ranges include their endpoints; `intervals()` end, gap and bucket-count semantics. Each needs a fixture.
- [ ] **Versioned envelopes.** WAL and shard file format versions, canonical hash ordering, and manifest/transfer identity (W2↔W8).

### `ti.toml`
- [ ] Schema + defaults: `W = 10s`, aggregate profiles (default / `slow` / opt-in), allow/deny lists (default deny `*.ais.*`, `design.*`), unit→scale table, per-path scale override, derived-field rules (transitions, edges, notifications), Signal K URL + token, store root, retention (2 y).
- [ ] Also: bind address, bearer token, pgwire SCRAM users, query limits (timeout, memory pool, `target_partitions`), source priorities, field cap.
- [ ] Loader with validation errors that name the bad key.

### Synthetic generator (`ti-bench gen`)
- [ ] Deterministic from a seed; writes signalk-parquet layout.
- [ ] Correctness set: 5 vessels × 90 days. Performance set: 50 × 365 × 120 paths (10 nav @ 1 Hz, rest @ 0.1 Hz).
- [ ] Models passages, anchoring, dock time; engine on/off; bilge cycles correlated with heel; wind fronts; notifications; notes with planted keywords.

### Golden corpus + oracle
- [ ] ≥ 60 golden queries covering Q1–Q8, each with a DuckDB oracle twin (`time_bucket(INTERVAL 'W', ts, TIMESTAMP '2020-01-01')` + same aggregate + fixed-point rounding).
- [ ] Stored expected outputs generated from the correctness set.
- [ ] Corpus format that `lume ti verify` (W4/W7) can consume: query, oracle query, class, expected result path, tolerance rules.

## Gate (M0)
- [ ] `ti-contracts` merged with types, traits and doc comments
- [ ] `ti.toml` schema with defaults
- [ ] `ti-bench gen` reproduces the correctness set byte-identically from a seed
- [ ] ≥ 60 golden queries with oracle twins and expected output

## Open
- Golden-query split across Q1–Q8 (how many of each)? Not specified.
- Where DuckDB runs in CI (dev-dependency `duckdb` crate vs CLI binary).
