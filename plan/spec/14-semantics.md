# 14. Proposed shared semantics (W0 part 1)

Status: proposal for integrator review, not a change to the frozen spec 10 signatures.
Sources: spec 05 (column space/encoding), 06 (rewrite/WAL), 07 (SQL), 10 (contracts).
Implementation in part 1 is limited to types and checked helpers; evaluator, store,
ingest and interval behavior below must be implemented and tested by their lanes.

## Nulls and negation (gap 1)

- `All` is the existing telemetry bucket universe in the shard, not all 65,536 positions. Rationale: SQL telemetry rows exist only for buckets with any data.
- `None` is empty; `And([])` is All and `Or([])` is None. Rationale: boolean identities make generated IR deterministic.
- Internally evaluate SQL expressions as true/false/unknown masks; `eval` returns only the true mask. `Not` exchanges true and false and preserves unknown. Rationale: complementing the true bitmap alone would turn null comparisons into matches.
- Missing field values make comparisons unknown, including `!=`, `NOT IN` and compound negation; `Present` and its negation are two-valued IS NOT NULL/IS NULL tests. Rationale: preserve SQL three-valued logic and the specified presence-ANDNOT rule.
- Nullable IN lists cannot be expressed by today's row-ID-only IR; leave such filters in DataFusion unless an exact translation is proved. Rationale: IN containing NULL changes negation behavior.
- For multi-valued sets, positive membership means any matching row; negative membership means present and no matching row. Rationale: matches the spec's row-OR/presence-ANDNOT rule; SQL list representation remains gap 3.
- Inexact geo filters remain inexact through compound boolean expressions, and NOT must not complement a conservative geo cover to obtain an exact result. Rationale: a cover's false positives can become false negatives under negation.

## Rewrite and durability (gap 4)

- One `apply` slice is a transaction boundary. Group records by (vessel, bucket, field); all records in a group must agree on rewrite. Rationale: a per-record clear would discard preceding set values.
- A rewrite group clears the field's entire column once, then installs every group value; duplicate set rows/cells are idempotent. Reject incompatible value encodings or multiple distinct numeric values before mutation. Rationale: late-data repair replaces a complete field aggregate.
- Validate the complete slice before appending or publishing; query readers see either the old or new state. A failed apply may have uncertain durable outcome after an I/O failure and requires recovery before retry. Rationale: errors must not expose half a multi-field rewrite or claim an unprovable rollback.
- `apply` acknowledges only after the framed WAL is durably synced, then publishes in-memory state. `flush` durably persists rows/catalog dependencies before advancing/truncating the WAL. Rationale: crash replay cannot reference unknown row IDs or lose acknowledged records.
- `seal` flushes, durably writes the immutable version, then atomically publishes and syncs the manifest; it returns only after durable publication. Rationale: replication must never discover a partial version.
- Partial backfill must reconstruct the complete bucket/field from authoritative samples, including previously ingested samples, before rewrite; it must not overwrite a full aggregate with only the new fragment. Rationale: rewrite idempotence alone does not guarantee merge correctness.
- This durability proposal is stronger than spec 03's possible 60-second power-loss window and may affect throughput; W2/W3 must benchmark it before freeze. Rationale: acknowledge the cost rather than silently weaken apply's guarantee.
- Clearing to an absent value has no representation in the current FieldValue enum; defer deletion-only rewrites to a contracts PR. Rationale: do not overload Present or invent a sentinel numeric value.

## Numeric and time edges (gap 5)

- Helpers use Unix seconds (`i64` timestamp), EPOCH = 1577836800, and positive whole-second `u64` width. Reject pre-epoch time and bucket indices beyond u32. Rationale: matches the unsigned address space while avoiding subtraction overflow.
- Buckets cover [start, start + W); TsRange endpoints are inclusive bucket indices, and reversed ranges are empty. Rationale: distinguish sample-time bucketing from the explicitly inclusive IR range.
- Fixed-point uses f64 input, decimal scale 0..=18 and ties away from zero; reject NaN, infinity and results outside [-2^63, 2^63). Negative zero encodes zero. Rationale: give ingest, literals and the DuckDB oracle one checked rule.
- Scale >18 is rejected even though FieldKind stores u8; this is an explicit proposal to constrain configuration, not a type change. Rationale: decimal factors above signed-integer precision are not useful for this v1 encoding.
- `from_fixed` returns f64 and may lose integer precision above 2^53; tolerance(scale) = 0.5 / 10^scale covers quantization only. Rationale: callers must not assume lossless integer-to-float conversion or a constant tolerance across units.
- SQL literal thresholds use the same to_fixed helper as ingest. Rationale: the spec requests consistent scaling and rounding.
- A document with ts_end covers [ts_start, ts_end); missing ts_end is a point assigned to its start bucket. Empty/reversed intervals are rejected; a point must use missing ts_end. Rationale: avoid matching the next bucket when an interval ends exactly at its boundary.
- intervals() returns half-open [start, end), merges runs only within a vessel (including across shard boundaries), and measures max_gap by missing-bucket duration. Rationale: consecutive shards must not split a continuous passage.
- Apply min_len after gap merging to the wall-clock span; buckets counts only matching buckets and excludes bridged gaps. Rationale: preserve evidence count while reporting elapsed passage duration.

### Required fixtures for downstream lanes

- Null comparison, IS NULL, nested NOT/AND/OR, IN with NULL, present multi-valued sets, sparse All universe, and NOT of inexact geo.
- Multiple set/cell rewrite records, mixed rewrite flags, invalid later record, crash at each WAL/flush/seal boundary, empty rewrite, partial backfill and replay twice.
- Implemented helper tests: epoch/pre-epoch, zero width, u32 max, shard boundary reconstruction, negative ties, negative zero, nonfinite input, i64 overflow, scale bounds and per-scale tolerance.
- Text ending on a bucket boundary, point documents, reversed ranges; interval runs crossing a shard, gap exactly at max_gap, min_len after merge and count excluding gaps.

## Newly defined contract types

- AggOp: CountAll, Count, Sum, Min, Max. Rationale: query operations differ from ingest Agg profiles.
- AggPartial: Count(u64), Sum { sum: i128, count: u64 }, Min(Option<i64>), Max(Option<i64>). Rationale: preserve empty/null state and enough integer width for per-shard sums; consumers must validate operation/variant agreement and merge with checked arithmetic.
- ShardManifestEntry: key, version (u64 starting at 1), inclusive from/to buckets, bytes (u64), raw 32-byte BLAKE3 hash. Rationale: minimum identity and coverage for sealing; canonical envelope is gap 6.
- Error/Result: invalid input, overflow, corruption, missing item, unsupported capability, I/O and Arrow errors. Rationale: share diagnostics without adding a runtime error dependency.
- ShardKey gains Debug only; all spec 10 fields, variants, aliases and method signatures remain unchanged. Rationale: support diagnostics on the newly defined manifest type.

## TODO for part 2

- Gap 2: catalog/dictionary registration, stable IDs, source priorities, URN remapping.
- Gap 3: Arrow schemas/nullability/order, docs/score handoff and TiEngine fixture facade.
- Gap 6: versioned WAL/shard envelopes, canonical hash inputs and transfer identity.
