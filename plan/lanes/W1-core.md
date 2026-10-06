# W1 — `ti-core` (weeks 2–4, gates M1)

Depends on: W0 contracts only.
Spec: [05-data-model](../spec/05-data-model.md), [07-query](../spec/07-query.md) (execution).

## Owns

Column addressing; `set` / `bsi` / `presence` / `count` rows; BSI range compare;
sum, min, max; the `Predicate` IR evaluator.

## Tasks

### Row types
- [ ] `PresenceRow` — one bitmap per field per shard.
- [ ] `SetField` — value → row-id dictionary + one bitmap per row; `=`, `!=`, `IN`, `NOT IN` (negation = presence ANDNOT row).
- [ ] `BsiField` — `exists`, `sign`, `bit[0..depth)`; sign-magnitude; depth growth adds rows only, never rewrites.
- [ ] `CountField` — BSI with no sign row.
- [ ] Clear-and-rewrite of a column across all rows (needed for late data and backfill idempotence).

### BSI algorithms
- [ ] O'Neil/Quass range compare: `=`, `<`, `<=`, `>`, `>=`, `BETWEEN` in one pass over depth rows using AND/OR/ANDNOT only. Handle negatives via sign row.
- [ ] Bit-sliced `sum` (popcount per slice × 2^i, signed), `min`, `max` over a filter bitmap.
- [ ] Value reconstruction for a column set (used by W4 materializer), batched.

### Predicate evaluator
- [ ] `eval(shard, &Predicate) -> RoaringBitmap` for `All`, `None`, `Present`, `SetEq`, `BsiCmp`, `TsRange` (range mask within the shard), `And`, `Or`, `Not`.
- [ ] `Text` and `GeoCover` delegate to injected `TextIndex` / geo cover rows (W5/W6) — stubbed here.
- [ ] Short-circuit on empty intermediate results.

### Naive model + proptests
- [ ] `Vec<Option<i64>>` reference model per field.
- [ ] 10,000-case proptests: BSI compare / sum / min / max incl. negatives and depth growth.
- [ ] Random AND/OR/NOT trees of depth ≤ 4 vs the naive model.

## First deliverable
`Predicate::eval` passing proptests against the naive `Vec<Option<i64>>` model.

## Gate (M1, shared with W2)
- [ ] BSI compare, sum, min, max agree with the naive model on 10,000 proptest cases
- [ ] `Predicate::eval` agrees with the naive model for random trees of depth ≤ 4

## Notes
- Algorithm ideas may come from FeatureBase (Apache-2.0) but **no code** is copied (licensing open question).
- Existing `src/fast_retrieval.rs::MiniRoaring` is array/bitmap containers only (no run containers, no portable format) — see [repo-fit](../repo-fit.md) §2 before deciding to reuse it.
