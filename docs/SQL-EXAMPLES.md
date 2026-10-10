# SQL examples

Each block is one statement, what it answers, and the shape of the result. Numbers below are the shape, not a captured run. Replace `<index>` with an ordinary `lume index` directory and `<store>` with a TI store before running the verify line.

JSON results use the object in [SQL.md](SQL.md): `columns`, `rows`, `row_count`, `truncated`, `elapsed_ms`, `pushdown`, `units`, `hint`. A row is one object whose keys are the select list. Timestamps are strings. `score` is a JSON number, or null when the statement has no `match()`.

The binary must be built with `--features ti`.

## Documents

### Top hits, with score

The ten sections BM25 ranks highest for the words, best score first. Equal scores break on `id`.

```sql
SELECT file, title, line, score
FROM sections
WHERE match(body, 'anchor')
ORDER BY score DESC, id
LIMIT 10
```

Shape: up to 10 rows. `file` text or null, `title` text, `line` integer, `score` number. `truncated` is false at this limit.

<!-- verify: lume sql --db <index> --format json "SELECT file, title, line, score FROM sections WHERE match(body, 'anchor') ORDER BY score DESC, id LIMIT 10" -->

### How many sections per file

A facet: which files contain the words, and how many matching sections each has. `GROUP BY file` runs after `match()` has selected the rows (`src/sql.rs` pushes down only `match()`).

```sql
SELECT file, count(*) AS n
FROM sections
WHERE match(body, 'anchor')
GROUP BY file
ORDER BY n DESC, file
```

Shape: one row per file that matched. `file` text or null, `n` integer. Ordered by `n` descending.

<!-- verify: lume sql --db <index> --format json "SELECT file, count(*) AS n FROM sections WHERE match(body, 'anchor') GROUP BY file ORDER BY n DESC, file" -->

### How many sections match

```sql
SELECT count(*) AS n
FROM sections
WHERE match(body, 'anchor')
```

Shape: one row, `n` integer. This count is the number of BM25 hits, not a scan of the whole index (`tests/lume_sql.rs`).

<!-- verify: lume sql --db <index> --format json "SELECT count(*) AS n FROM sections WHERE match(body, 'anchor')" -->

### Entities and the edges between them

Which entities share an edge, and how related they are. `sections` has no entity column (`src/sql.rs`), so this does not join sections. It joins `entities` to `entity_edges` on the entity id. The tables exist only when the index has an entity graph. Without one, the query errors because `entities` is not registered.

```sql
SELECT e.entity, e.doc_count, x.b AS linked, x.relatedness
FROM entities e
JOIN entity_edges x ON e.entity = x.a
ORDER BY x.relatedness DESC, e.entity
LIMIT 20
```

Shape: up to 20 rows. `entity` and `linked` text, `doc_count` integer, `relatedness` number.

<!-- verify: lume sql --db <index> --format json "SELECT e.entity, e.doc_count, x.b AS linked, x.relatedness FROM entities e JOIN entity_edges x ON e.entity = x.a ORDER BY x.relatedness DESC, e.entity LIMIT 20" -->

## Telemetry

### Speed over ground, last 10 minutes

One row per bucket in the last 10 minutes. The bare name is the mean alias (`crates/ti-contracts/src/schemas.rs`). The SQL console preset uses that name (`plugins/signalk-lume-ti/public/index.html`). The time filter is an exact bitmap prune (`crates/ti-sql/tests/provider.rs`).

```sql
SELECT ts, "navigation.speedOverGround" AS sog
FROM telemetry
WHERE ts >= now() - INTERVAL '10 minutes'
ORDER BY ts
```

Shape: `ts` timestamp string, `sog` number or null, ordered by time. Empty `rows` if the boat sent nothing in that window, or if this store has no mean column under that path. The explicit peak is `"navigation.speedOverGround@max"`.

<!-- verify: lume ti query "SELECT ts, \"navigation.speedOverGround\" AS sog FROM telemetry WHERE ts >= now() - INTERVAL '10 minutes' ORDER BY ts" --store <store> --json -->

### One-minute buckets

The peak speed in each minute of that same window. `date_bin` plus `max` of a numeric column is the bitmap aggregate (`crates/ti-sql/src/aggregate.rs`).

```sql
SELECT date_bin(INTERVAL '1 minute', ts) AS minute,
       max("navigation.speedOverGround@max") AS max_sog
FROM telemetry
WHERE ts >= now() - INTERVAL '10 minutes'
GROUP BY minute
ORDER BY minute
```

Shape: one row per minute that had buckets. `minute` timestamp string, `max_sog` number or null, ordered by `minute`.

<!-- verify: lume ti query "SELECT date_bin(INTERVAL '1 minute', ts) AS minute, max(\"navigation.speedOverGround@max\") AS max_sog FROM telemetry WHERE ts >= now() - INTERVAL '10 minutes' GROUP BY minute ORDER BY minute" --store <store> --json -->

### Peak speed per vessel

```sql
SELECT vessel, max("navigation.speedOverGround@max") AS max_sog
FROM telemetry
WHERE ts >= now() - INTERVAL '10 minutes'
GROUP BY vessel
ORDER BY vessel
```

Shape: one row per vessel with buckets in the window. `vessel` text (a URN), `max_sog` number or null.

<!-- verify: lume ti query "SELECT vessel, max(\"navigation.speedOverGround@max\") AS max_sog FROM telemetry WHERE ts >= now() - INTERVAL '10 minutes' GROUP BY vessel ORDER BY vessel" --store <store> --json -->

### Which source wrote the speed

Buckets in the last 10 minutes whose speed source list contains `<source>`. `"path$source" = 'ref'` is rewritten to `array_has` (`crates/ti-sql/src/rewrite.rs`). Replace `<source>` with a source ref from this boat, such as the `$source` on a live Signal K delta.

```sql
SELECT count(*) AS n
FROM telemetry
WHERE array_has("navigation.speedOverGround$source", '<source>')
  AND ts >= now() - INTERVAL '10 minutes'
```

Shape: one row, `n` integer. Zero is a real answer when that source ref is absent. An unknown column means this store has no `$source` field for that path.

<!-- verify: lume ti query "SELECT count(*) AS n FROM telemetry WHERE array_has(\"navigation.speedOverGround\$source\", '<source>') AND ts >= now() - INTERVAL '10 minutes'" --store <store> --json -->

### Paths in this store

The catalog of columns, not the samples.

```sql
SELECT path, agg, type, units
FROM paths
ORDER BY path
```

Shape: one row per stored field. `path` text, `agg` text or null, `type` text (`presence`, `set`, `bsi`, `count`, or `geo`), `units` text or null. A large store can hit the 500-row cap; `truncated` is then true.

<!-- verify: lume ti query "SELECT path, agg, type, units FROM paths ORDER BY path" --store <store> --json -->

### How fresh the boat data is

The newest telemetry bucket.

```sql
SELECT max(ts) AS latest_ts
FROM telemetry
```

Shape: one row. `latest_ts` is a timestamp string, or null when `telemetry` has no rows.

<!-- verify: lume ti query "SELECT max(ts) AS latest_ts FROM telemetry" --store <store> --json -->

### How fresh Lume's own counters are

Same question for `telemetry_lume`. That table is registered only after the first self-telemetry write (`crates/ti-sql/src/engine.rs`). Before that, this errors because the table was not found. That error is expected on a store that has never recorded it.

```sql
SELECT max(ts) AS latest_ts
FROM telemetry_lume
```

Shape, once the table exists: one row, `latest_ts` timestamp string or null.

<!-- verify: lume ti query "SELECT max(ts) AS latest_ts FROM telemetry_lume" --store <store> --json -->

## Docs table

### Notes and logbook in a time window

Documents whose body matches, limited to notes and logbook, with `ts_start` in January 2026. Change the timestamps to a window this store actually covers. `match(body, ...)` is the bitmap. `kind` and `ts_start` are applied after those rows are loaded (`crates/ti-sql/src/docs.rs`).

```sql
SELECT kind, title, ts_start, ts_end, score
FROM docs
WHERE match(body, 'anchor')
  AND kind IN ('notes', 'logbook')
  AND ts_start >= TIMESTAMP '2026-01-01 00:00:00'
  AND ts_start < TIMESTAMP '2026-02-01 00:00:00'
ORDER BY score DESC
LIMIT 20
```

Shape: up to 20 rows. `kind` and `title` text, `ts_start` timestamp string, `ts_end` timestamp string or null, `score` number. Empty `rows` when nothing in that window matches. `kind` values stored by ingest are `notes`, `logbook`, and `alerts`.

<!-- verify: lume ti query "SELECT kind, title, ts_start, ts_end, score FROM docs WHERE match(body, 'anchor') AND kind IN ('notes', 'logbook') AND ts_start >= TIMESTAMP '2026-01-01 00:00:00' AND ts_start < TIMESTAMP '2026-02-01 00:00:00' ORDER BY score DESC LIMIT 20" --store <store> --json -->

## Agent telemetry

### Tokens per agent per hour

How many tokens each agent added in each hour. The column is the one the agents dashboard uses (`bench/grafana/lume-agents-dashboard.json`). `telemetry_agents` exists only after OTLP metrics have been stored. Before that, this errors because the table was not found. If the token path was never ingested, the column is unknown and this errors too.

```sql
SELECT date_bin(INTERVAL '1 hour', ts) AS hour,
       vessel AS agent,
       max("claude_code.token.usage@last") - min("claude_code.token.usage@last") AS tokens
FROM telemetry_agents
GROUP BY hour, vessel
ORDER BY hour, agent
```

Shape: one row per agent per hour. `hour` timestamp string, `agent` text, `tokens` number. `date_bin` with `max` and `min` of a numeric column is the bitmap aggregate (`crates/ti-sql/src/aggregate.rs`). The subtraction sits on top of those two values. `lume ti explain` prints whether that aggregate was chosen or fell back to reading rows.

<!-- verify: lume ti query "SELECT date_bin(INTERVAL '1 hour', ts) AS hour, vessel AS agent, max(\"claude_code.token.usage@last\") - min(\"claude_code.token.usage@last\") AS tokens FROM telemetry_agents GROUP BY hour, vessel ORDER BY hour, agent" --store <store> --json -->

## Grafana

### Tokens panel over pgwire

This is the Tokens Over Time panel in `bench/grafana/lume-agents-dashboard.json`. Grafana rewrites `$__timeGroupAlias` and `$__timeFilter` before it sends the statement. Lume does not understand those macros. The verify line is the same statement with `date_bin` and no time macro, which is what you run in `psql` or `lume ti query`. Point Grafana at the pgwire port (`lume serve --ti-store <store> --pg 5864`, database and user as provisioned).

```sql
SELECT
  $__timeGroupAlias(ts, 1h),
  vessel AS metric,
  max("claude_code.token.usage@last") - min("claude_code.token.usage@last") AS tokens
FROM telemetry_agents
WHERE $__timeFilter(ts)
GROUP BY 1, 2
ORDER BY 1
```

Shape of the runnable form: `hour` timestamp string, `metric` text, `tokens` number, one row per agent per hour. Same absence rules as the agent example above.

<!-- verify: lume ti query "SELECT date_bin(INTERVAL '1 hour', ts) AS hour, vessel AS metric, max(\"claude_code.token.usage@last\") - min(\"claude_code.token.usage@last\") AS tokens FROM telemetry_agents GROUP BY hour, vessel ORDER BY hour" --store <store> --json -->
