# W4 — `ti-sql` (weeks 2–6 for M3; M4 aggregate rewrite + intervals)

Depends on: W0 contracts (+ W1 via the `ShardSource` trait). Develops against an
in-memory `ShardSource` fixture first.
Spec: [07-query](../spec/07-query.md).

## Owns

DataFusion `TableProvider`, pushdown classifier, IR translation, materializer,
`intervals()`, `BitmapAggregateExec`, explain.

## Tasks

### Setup
- [ ] Pin one DataFusion major (decisions log). Matching arrow version shared with W0.
- [ ] In-memory `ShardSource` fixture loaded from the correctness set.

### Tables
- [ ] `telemetry` provider: `vessel`, `ts`, one column per field (`"path"`, `"path@agg"`, `"path$source"`), virtual `notes` / `logbook` / `alerts`; bare path aliases `@mean`.
- [ ] `docs` provider (`vessel, kind, ts_start, ts_end, title, body, score`), backed by W5.
- [ ] `raw` as a DataFusion `ListingTable` over signalk-parquet.
- [ ] `vessels`, `paths`, `shards` catalog tables (from W2 catalogs).
- [ ] Reject INSERT/UPDATE/DELETE/DDL with an error naming the table as derived.

### Pushdown
- [ ] `supports_filters_pushdown` per conjunct per the table: Exact (vessel, ts, set, bsi-vs-literal, IS NULL, match, AND/OR/NOT), Inexact (`in_bbox`, `within_nm`), Unsupported (everything else).
- [ ] Literal scaling and rounding identical to ingest (shared helper from W0).
- [ ] Translate pushed filters → `Predicate` IR; prune shards via vessel/ts bounds + manifest.

### Execution
- [ ] One DataFusion partition per shard; eval IR → `RoaringBitmap`.
- [ ] Materializer: iterate set bits in batches of 8,192; `set` via row-membership lookup; `bsi` reconstruct ÷ 10^scale; emit `RecordBatch`; projection-aware.
- [ ] Register `match`, `in_bbox`, `within_nm` UDFs (geo/text implementations from W5/W6).

### `intervals()` table function (M4)
- [ ] `intervals(predicate_sql, min_len, max_gap, vessel)` → `(vessel, start, end, buckets)`; read runs from the result bitmap (run containers when present); merge gaps ≤ `max_gap`; filter `min_len`.

### `BitmapAggregateExec` (M4)
- [ ] Physical optimizer rule replacing `Aggregate(Scan)` when all aggregates ∈ {`count(*)`, `count(col)`, `sum`/`min`/`max` of bsi} over pushed filters only, and `GROUP BY` ⊆ {`vessel`, `date_bin(ts)`}.
- [ ] Group windows → column-range masks.
- [ ] Benchmark vs materializing path on Q4 at shore scale (≥ 10×).

### Explain
- [ ] `EXPLAIN` / `ti_explain` Lume TI section: shards pruned vs scanned, per-conjunct pushdown class, bitmap cardinality after each step, materialized row count.

### Verify harness
- [ ] `lume ti verify --oracle duckdb --corpus tests/golden`: run TI + oracle, diff exact on keys/sets/counts, ±0.5×10^−scale on BSI.

## First deliverable
Golden corpus green against the in-memory `ShardSource` fixture.

## Gate (M3)
- [ ] All non-text, non-geo golden queries match the oracle — fixture first, then a real store
- [ ] `EXPLAIN` shows Exact pushdown for every expression marked Exact
- [ ] `raw` queries the same Parquet and matches DuckDB exactly

## Gate (M4, shared with W5/W6)
- [ ] Full golden corpus green incl. `match()`, `in_bbox`, `within_nm`, `intervals()`
- [ ] `BitmapAggregateExec` ≥ 10× faster than materializing on Q4 at shore scale
- [ ] `croaring` frozen-view evaluation written up in the decisions log (adopt / reject)
