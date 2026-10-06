# 7. Query layer

Spec pp. 13–16. Owner: [W4](../lanes/W4-sql.md); `match()` [W5](../lanes/W5-text.md); geo [W6](../lanes/W6-geo.md).

DataFusion parses, plans and executes SQL. Lume TI is a `TableProvider` that takes
pushed-down filters, evaluates them as bitmap operations per shard, and
materializes only surviving rows into Arrow. Pin one DataFusion major version per
release; upgrade only behind the golden corpus.

## SQL surface

| Table | Rows are | Key columns |
|---|---|---|
| `telemetry` | one per `(vessel, bucket)` with any data | `vessel`, `ts` (bucket start, UTC), one column per field: `"path"`, `"path@agg"`, `"path$source"`; virtual `notes`, `logbook`, `alerts` for `match()` |
| `docs` | one per indexed document | `vessel`, `kind`, `ts_start`, `ts_end`, `title`, `body`, `score` (only populated with `match()`) |
| `raw` | one per raw sample | DataFusion `ListingTable` over signalk-parquet: exact samples, no bitmap acceleration |
| `vessels`, `paths`, `shards` | catalog entries | as in [05-data-model](05-data-model.md) |

Column names are exact Signal K paths and must be double-quoted. Agents never
guess them: they call `ti_resolve` first.

## SQL coverage

Full read-only SQL — anything DataFusion accepts runs. Pushdown decides only
speed, never acceptance. Supported: joins of any kind across `telemetry`, `docs`,
`raw` and catalogs; CTEs incl. recursive; correlated and scalar subqueries;
window functions; `UNION`/`INTERSECT`/`EXCEPT`; `CASE`, `GROUP BY`/`HAVING`,
`ORDER BY`/`LIMIT`; DataFusion scalar/aggregate/time functions plus the TI
functions below. INSERT/UPDATE/DELETE/DDL are rejected with an error naming the
table as derived.

```sql
-- Hours where house SOC fell faster than 5 %/h
WITH h AS (
  SELECT date_bin(INTERVAL '1 hour', ts) AS hr,
         avg("electrical.batteries.house.stateOfCharge") AS soc
  FROM telemetry WHERE ts >= now() - INTERVAL '14 days' GROUP BY hr),
d AS (SELECT hr, soc, soc - lag(soc) OVER (ORDER BY hr) AS delta FROM h)
SELECT * FROM d WHERE delta < -0.05 ORDER BY hr;
```

The `ts` filter is pushed down; CTE, window and outer filter run in DataFusion.

## Functions

- `match(text_col, query) → bool` — Lume query syntax. `WHERE` only; always pushed down.
- `in_bbox(lat_min, lon_min, lat_max, lon_max) → bool` — H3 cover, refined against lat/lon BSI.
- `within_nm(lat, lon, radius_nm) → bool` — same, refined by haversine on materialized rows.
- `date_bin`, `date_trunc` and every other function are standard DataFusion.
- `intervals(predicate_sql, min_len => '0s', max_gap => '0s', vessel => NULL)` — table function returning `(vessel, start, end, buckets)` per contiguous run where the predicate holds. Reads runs straight from the result bitmap (run containers when present), then merges gaps ≤ `max_gap`.

```sql
-- When did we motor in more than 25 kn of wind this season?
SELECT * FROM intervals(
  '"propulsion.port.state" = ''started'' AND "environment.wind.speedTrue@max" > 12.9',
  min_len => '5m', max_gap => '1m')
WHERE start > '2026-05-01';
```

## Pushdown (`supports_filters_pushdown` per conjunct)

| Expression | Pushdown | Bitmap evaluation |
|---|---|---|
| `vessel =` / `IN` | Exact | prunes shards by vessel ordinal |
| `ts` comparisons, `BETWEEN` | Exact | prunes shards, then range mask inside boundary shards |
| `set_col = / != / IN / NOT IN` | Exact | row OR, or presence ANDNOT row |
| `bsi_col <op> literal`, `BETWEEN` | Exact | BSI range compare (literal scaled and rounded consistently) |
| `col IS [NOT] NULL` | Exact | presence row |
| `match(...)` | Exact | Lume postings mapped to buckets |
| `in_bbox`, `within_nm` | Inexact | H3 cover rows OR; DataFusion re-applies exact predicate |
| `AND`, `OR`, `NOT` of the above | Exact | bitmap AND, OR, ANDNOT |
| anything else (col-vs-col, arithmetic, UDFs) | Unsupported | DataFusion filters after materialization |

## Execution

1. **Plan.** Pushed filters → `Predicate` IR. Prune to candidate shards using vessel and ts bounds plus the manifest.
2. **Evaluate.** Each shard is a DataFusion partition. Evaluating the IR yields a `RoaringBitmap` of local columns.
3. **Materialize.** Iterate set bits in batches of 8,192. Per projected column: `set` → row-membership lookup per value; `bsi` → reconstruct integer from depth rows ÷ 10^scale. Emit a `RecordBatch`.
4. **Stream.** Batches go to DataFusion operators unchanged.

## Aggregate rewrites (M4)

A physical optimizer rule replaces `Aggregate(Scan)` with `BitmapAggregateExec`
when every aggregate is one of, over pushed filters only:

- `count(*)` — bitmap cardinality
- `count(col)` — cardinality of filter AND presence
- `sum`, `min`, `max` of a `bsi` column — bit-sliced algorithms

`GROUP BY` must be absent or limited to `vessel` and `date_bin(ts)`; group
windows become column-range masks. Target ≥ 10× faster than materialize-then-
aggregate on year-long windows.

## Explain

`EXPLAIN` and `ti_explain` print the DataFusion plan plus a Lume TI section:
shards pruned vs scanned, each conjunct's pushdown class, bitmap cardinality
after each step, and materialized row count. Agents use this to rewrite slow queries.
