# SQL examples

Each block is one statement, what it answers, and the output of a real run. Document examples 1–4 are lume v0.12.3 on the repo `.lume-index` (a PDF), 2026-10-10, with the term `energy`. Telemetry and docs examples are a HaLOS Pi, v0.12.3, 2026-10-10. The Pi runs went through `POST /ti/query`, the same engine as `lume ti query`, and the same call the SQL console makes.

JSON results use the object in [SQL.md](SQL.md): `columns`, `rows`, `row_count`, `truncated`, `elapsed_ms`, `pushdown`, `units`, `hint`. `pushdown` tells you whether a filter was pushed down, for example `2 scans; conjunct classes: {"Exact"}`. A row is one object whose keys are the select list. Timestamps are strings. `score` is a JSON number, or null when the statement has no `match()`.

The binary must be built with `--features ti`. Paths in the captured rows were shortened with `…` in the notes below.

## Documents

Captured with `lume sql --db .lume-index --format json` on lume v0.12.3.

### Top hits, with score

The ten sections BM25 ranks highest for the words, best score first. Equal scores break on `id`.

```sql
SELECT file, title, line, score
FROM sections
WHERE match(body, 'energy')
ORDER BY score DESC, id
LIMIT 10
```

Expected output, lume v0.12.3, repo `.lume-index`, 2026-10-10. Columns `file`, `title`, `line`, `score`. Scores are BM25 floats. The top row was `…Sidis.pdf | Page 21 | 21 | 1.649`.

<!-- verify: lume sql --db <index> --format json "SELECT file, title, line, score FROM sections WHERE match(body, 'energy') ORDER BY score DESC, id LIMIT 10" -->

### How many sections per file

A facet: which files contain the words, and how many matching sections each has. `GROUP BY file` runs after `match()` has selected the rows (`src/sql.rs` pushes down only `match()`).

```sql
SELECT file, count(*) AS n
FROM sections
WHERE match(body, 'energy')
GROUP BY file
ORDER BY n DESC, file
```

Expected output, same run. Columns `file`, `n`. One row: `…Sidis.pdf | 62`.

<!-- verify: lume sql --db <index> --format json "SELECT file, count(*) AS n FROM sections WHERE match(body, 'energy') GROUP BY file ORDER BY n DESC, file" -->

### How many sections match

```sql
SELECT count(*) AS n
FROM sections
WHERE match(body, 'energy')
```

Expected output, same run. One row, `n` = 62. This count is the number of BM25 hits (`tests/lume_sql.rs`).

<!-- verify: lume sql --db <index> --format json "SELECT count(*) AS n FROM sections WHERE match(body, 'energy')" -->

### Entities and the edges between them

Which entities share an edge, and how related they are. `sections` has no entity column (`src/sql.rs`), so this does not join sections. It joins `entities` to `entity_edges` on the entity id. The repo `.lume-index` has `entity_graph.json` with empty `nodes` and `edges`, so the tables exist and the join matches nothing. If that file is missing, the tables are not registered and the query errors (`tests/lume_sql.rs`).

```sql
SELECT e.entity, e.doc_count, x.b AS linked, x.relatedness
FROM entities e
JOIN entity_edges x ON e.entity = x.a
ORDER BY x.relatedness DESC, e.entity
LIMIT 20
```

Expected output, same run. Columns `entity`, `doc_count`, `linked`, `relatedness`. 0 rows.

<!-- verify: lume sql --db <index> --format json "SELECT e.entity, e.doc_count, x.b AS linked, x.relatedness FROM entities e JOIN entity_edges x ON e.entity = x.a ORDER BY x.relatedness DESC, e.entity LIMIT 20" -->

## Telemetry

Captured on a HaLOS Pi, v0.12.3, 2026-10-10, with:

```bash
curl -s -H 'Accept: application/json' -H 'Content-Type: application/json' -X POST http://127.0.0.1:5863/ti/query -d '{"sql":"<sql>"}'
```

`lume ti query "<sql>" --store <store> --json` is the same statement. Each example has both.

### Speed over ground, last 10 minutes

One row per bucket in the last 10 minutes. The bare name is the mean alias (`crates/ti-contracts/src/schemas.rs`). The SQL console preset uses that name (`plugins/signalk-lume-ti/public/index.html`). The time filter is an exact bitmap prune (`crates/ti-sql/tests/provider.rs`).

```sql
SELECT ts, "navigation.speedOverGround" AS sog
FROM telemetry
WHERE ts >= now() - INTERVAL '10 minutes'
ORDER BY ts
```

```bash
curl -s -H 'Accept: application/json' -H 'Content-Type: application/json' -X POST http://127.0.0.1:5863/ti/query -d "{\"sql\":\"SELECT ts, \\\"navigation.speedOverGround\\\" AS sog FROM telemetry WHERE ts >= now() - INTERVAL '10 minutes' ORDER BY ts\"}"
```

Expected output, HaLOS Pi, v0.12.3, 2026-10-10. 55 rows. Columns `ts`, `sog`. One row was `{"ts":"2026-10-10T02:32:40Z","sog":3.334}`. The bare column `navigation.speedOverGround` is accepted.

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

```bash
curl -s -H 'Accept: application/json' -H 'Content-Type: application/json' -X POST http://127.0.0.1:5863/ti/query -d "{\"sql\":\"SELECT date_bin(INTERVAL '1 minute', ts) AS minute, max(\\\"navigation.speedOverGround@max\\\") AS max_sog FROM telemetry WHERE ts >= now() - INTERVAL '10 minutes' GROUP BY minute ORDER BY minute\"}"
```

Expected output, same Pi run. 10 rows. Columns `minute`, `max_sog`. One row was `{"minute":"2026-10-10T02:32:00Z","max_sog":3.49}`.

<!-- verify: lume ti query "SELECT date_bin(INTERVAL '1 minute', ts) AS minute, max(\"navigation.speedOverGround@max\") AS max_sog FROM telemetry WHERE ts >= now() - INTERVAL '10 minutes' GROUP BY minute ORDER BY minute" --store <store> --json -->

### Peak speed per vessel

```sql
SELECT vessel, max("navigation.speedOverGround@max") AS max_sog
FROM telemetry
WHERE ts >= now() - INTERVAL '10 minutes'
GROUP BY vessel
ORDER BY vessel
```

```bash
curl -s -H 'Accept: application/json' -H 'Content-Type: application/json' -X POST http://127.0.0.1:5863/ti/query -d "{\"sql\":\"SELECT vessel, max(\\\"navigation.speedOverGround@max\\\") AS max_sog FROM telemetry WHERE ts >= now() - INTERVAL '10 minutes' GROUP BY vessel ORDER BY vessel\"}"
```

Expected output, same Pi run. 1 row. Columns `vessel`, `max_sog`. The row was `{"vessel":"vessels.urn:mrn:signalk:uuid:0eb191d0-…","max_sog":3.75}`.

<!-- verify: lume ti query "SELECT vessel, max(\"navigation.speedOverGround@max\") AS max_sog FROM telemetry WHERE ts >= now() - INTERVAL '10 minutes' GROUP BY vessel ORDER BY vessel" --store <store> --json -->

### Which source wrote the speed

`"<path>$source"` is a list of source refs, not a single string. Read one value before you filter:

```sql
SELECT "navigation.speedOverGround$source"
FROM telemetry
LIMIT 1
```

```bash
curl -s -H 'Accept: application/json' -H 'Content-Type: application/json' -X POST http://127.0.0.1:5863/ti/query -d "{\"sql\":\"SELECT \\\"navigation.speedOverGround\$source\\\" FROM telemetry LIMIT 1\"}"
```

On the Pi a value was `["n2k-sample-data.160"]`. `$source` paths are also rows in `paths`.

`array_has` tests membership in that list. `"path$source" = 'ref'` is rewritten to the same call (`crates/ti-sql/src/rewrite.rs`).

```sql
SELECT count(*) AS n
FROM telemetry
WHERE array_has("navigation.speedOverGround$source", 'n2k-sample-data.160')
  AND ts >= now() - INTERVAL '10 minutes'
```

```bash
curl -s -H 'Accept: application/json' -H 'Content-Type: application/json' -X POST http://127.0.0.1:5863/ti/query -d "{\"sql\":\"SELECT count(*) AS n FROM telemetry WHERE array_has(\\\"navigation.speedOverGround\$source\\\", 'n2k-sample-data.160') AND ts >= now() - INTERVAL '10 minutes'\"}"
```

Expected output, same Pi run. One row, `n` = 54. `pushdown` was `2 scans; conjunct classes: {"Exact"}`.

<!-- verify: lume ti query "SELECT \"navigation.speedOverGround\$source\" FROM telemetry LIMIT 1" --store <store> --json -->

<!-- verify: lume ti query "SELECT count(*) AS n FROM telemetry WHERE array_has(\"navigation.speedOverGround\$source\", 'n2k-sample-data.160') AND ts >= now() - INTERVAL '10 minutes'" --store <store> --json -->

### Paths in this store

The catalog of columns, not the samples.

```sql
SELECT path, agg, type, units
FROM paths
ORDER BY path
```

```bash
curl -s -H 'Accept: application/json' -H 'Content-Type: application/json' -X POST http://127.0.0.1:5863/ti/query -d '{"sql":"SELECT path, agg, type, units FROM paths ORDER BY path"}'
```

Expected output, same Pi run. Columns `path`, `agg`, `type`, `units`. One row was `{"path":"electrical.batteries.1.batteryType","agg":null,"type":"set","units":null}`. `$source` paths are in this list too. A large store can hit the 500-row cap.

<!-- verify: lume ti query "SELECT path, agg, type, units FROM paths ORDER BY path" --store <store> --json -->

### How fresh the boat data is

The newest telemetry bucket.

```sql
SELECT max(ts) AS latest_ts
FROM telemetry
```

```bash
curl -s -H 'Accept: application/json' -H 'Content-Type: application/json' -X POST http://127.0.0.1:5863/ti/query -d '{"sql":"SELECT max(ts) AS latest_ts FROM telemetry"}'
```

Expected output, same Pi run. One row, `latest_ts` = `2026-10-10T02:41:40Z`.

<!-- verify: lume ti query "SELECT max(ts) AS latest_ts FROM telemetry" --store <store> --json -->

### How fresh Lume's own counters are

Same question for `telemetry_lume`. On this Pi the table was already there.

```sql
SELECT max(ts) AS latest_ts
FROM telemetry_lume
```

```bash
curl -s -H 'Accept: application/json' -H 'Content-Type: application/json' -X POST http://127.0.0.1:5863/ti/query -d '{"sql":"SELECT max(ts) AS latest_ts FROM telemetry_lume"}'
```

Expected output, same Pi run. One row, `latest_ts` = `2026-10-10T02:42:10Z`. Until the first self-telemetry write the table is not registered (`crates/ti-sql/src/engine.rs`).

<!-- verify: lume ti query "SELECT max(ts) AS latest_ts FROM telemetry_lume" --store <store> --json -->

## Docs table

### Notes and logbook in a time window

Documents whose body matches, limited to notes and logbook, with `ts_start` in January 2026. `match(body, ...)` is the bitmap. `kind` and `ts_start` are applied after those rows are loaded (`crates/ti-sql/src/docs.rs`).

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

```bash
curl -s -H 'Accept: application/json' -H 'Content-Type: application/json' -X POST http://127.0.0.1:5863/ti/query -d '{"sql":"SELECT kind, title, ts_start, ts_end, score FROM docs WHERE match(body, '\''anchor'\'') AND kind IN ('\''notes'\'', '\''logbook'\'') AND ts_start >= TIMESTAMP '\''2026-01-01 00:00:00'\'' AND ts_start < TIMESTAMP '\''2026-02-01 00:00:00'\'' ORDER BY score DESC LIMIT 20"}'
```

Expected output, same Pi run. The statement runs. Columns `kind`, `title`, `ts_start`, `ts_end`, `score`. 0 rows in that window. `kind` values stored by ingest are `notes`, `logbook`, and `alerts`.

<!-- verify: lume ti query "SELECT kind, title, ts_start, ts_end, score FROM docs WHERE match(body, 'anchor') AND kind IN ('notes', 'logbook') AND ts_start >= TIMESTAMP '2026-01-01 00:00:00' AND ts_start < TIMESTAMP '2026-02-01 00:00:00' ORDER BY score DESC LIMIT 20" --store <store> --json -->

## Agent telemetry

### Tokens per agent per hour

How many tokens each agent added in each hour. The column is the one the agents dashboard uses (`bench/grafana/lume-agents-dashboard.json`). This Pi has no `telemetry_agents` store.

```sql
SELECT date_bin(INTERVAL '1 hour', ts) AS hour,
       vessel AS agent,
       max("claude_code.token.usage@last") - min("claude_code.token.usage@last") AS tokens
FROM telemetry_agents
GROUP BY hour, vessel
ORDER BY hour, agent
```

```bash
curl -s -H 'Accept: application/json' -H 'Content-Type: application/json' -X POST http://127.0.0.1:5863/ti/query -d "{\"sql\":\"SELECT date_bin(INTERVAL '1 hour', ts) AS hour, vessel AS agent, max(\\\"claude_code.token.usage@last\\\") - min(\\\"claude_code.token.usage@last\\\") AS tokens FROM telemetry_agents GROUP BY hour, vessel ORDER BY hour, agent\"}"
```

Expected output, same Pi run. The query errors:

```text
table 'datafusion.public.telemetry_agents' not found; available tables: telemetry, docs, paths, vessels, shards, telemetry_lume
```

Once OTLP metrics have created the table, the shape is one row per agent per hour: `hour` timestamp string, `agent` text, `tokens` number. `date_bin` with `max` and `min` of a numeric column is the bitmap aggregate (`crates/ti-sql/src/aggregate.rs`). The subtraction sits on top of those two values.

<!-- verify: lume ti query "SELECT date_bin(INTERVAL '1 hour', ts) AS hour, vessel AS agent, max(\"claude_code.token.usage@last\") - min(\"claude_code.token.usage@last\") AS tokens FROM telemetry_agents GROUP BY hour, vessel ORDER BY hour, agent" --store <store> --json -->

## Grafana

The Tokens Over Time panel in `bench/grafana/lume-agents-dashboard.json` is the statement above with Grafana macros. Grafana rewrites `$__timeGroupAlias` and `$__timeFilter` before it sends the text. Lume does not understand those macros. Expanded, it is the same query as the agent example, so it is not repeated here. On this Pi it fails with the same missing-table error. Point Grafana at pgwire (`lume serve --ti-store <store> --pg 5864`) only after `telemetry_agents` exists.

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
