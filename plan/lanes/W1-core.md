# W1 — `ti-core` (weeks 2–4, gates M1)

Depends on: W0 contracts only.
Spec: [05-data-model](../spec/05-data-model.md), [07-query](../spec/07-query.md) (execution).

## Owns

Column addressing; `set` / `bsi` / `presence` / `count` rows; BSI range compare;
sum, min, max; the `Predicate` IR evaluator.

## Tasks

### Row types
- [x] `PresenceRow` — one bitmap per field per shard.
- [x] `SetField` — value → row-id dictionary + one bitmap per row; `=`, `!=`, `IN`, `NOT IN` (negation = presence ANDNOT row).
- [x] `BsiField` — `exists`, `sign`, `bit[0..depth)`; sign-magnitude; depth growth adds rows only, never rewrites.
- [x] `CountField` — BSI with no sign row.
- [x] Clear-and-rewrite of a column across all rows (needed for late data and backfill idempotence).

### BSI algorithms
- [x] O'Neil/Quass range compare: `=`, `<`, `<=`, `>`, `>=`, `BETWEEN` in one pass over depth rows using AND/OR/ANDNOT only. Handle negatives via sign row.
- [x] Bit-sliced `sum` (popcount per slice × 2^i, signed), `min`, `max` over a filter bitmap.
- [x] Value reconstruction for a column set (used by W4 materializer), batched.

### Predicate evaluator
- [x] `eval(shard, &Predicate) -> RoaringBitmap` for `All`, `None`, `Present`, `SetEq`, `BsiCmp`, `TsRange` (range mask within the shard), `And`, `Or`, `Not`.
- [x] `Text` and `GeoCover` delegate to injected `TextIndex` / geo cover rows (W5/W6) — stubbed here.
- [x] Short-circuit on empty intermediate results.

### Naive model + proptests
- [x] `Vec<Option<i64>>` reference model per field.
- [x] 10,000-case proptests: BSI compare / sum / min / max incl. negatives and depth growth.
- [x] Random AND/OR/NOT trees of depth ≤ 4 vs the naive model.

## First deliverable
`Predicate::eval` passing proptests against the naive `Vec<Option<i64>>` model.

## Gate (M1, shared with W2)
- [x] BSI compare, sum, min, max agree with the naive model on 10,000 proptest cases
- [x] `Predicate::eval` agrees with the naive model for random trees of depth ≤ 4

## Notes
- Algorithm ideas may come from FeatureBase (Apache-2.0) but **no code** is copied (licensing open question).
- Existing `src/fast_retrieval.rs::MiniRoaring` is array/bitmap containers only (no run containers, no portable format) — see [repo-fit](../repo-fit.md) §2 before deciding to reuse it.

## W1 implementation report (2026-10-06)

Implementation/API, scope boundaries, checks, proptest counts/runtime and a reproducible
non-gating performance baseline: [ti-core README](../../crates/ti-core/README.md).
Rows and evaluator algorithms are original implementations with module documentation.
W2 owns durability; W4 owns Arrow materialization. Shared ti-contracts remains frozen.
