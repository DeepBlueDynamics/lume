# Querying Lume with SQL

Lume has two SQL surfaces. They share one engine (Apache DataFusion) and they do not query the same tables.

Use `lume sql` for an ordinary search index, the directory `lume index` writes. That is books, manuals, notes, and code. Use `lume ti query` for a telemetry store: Signal K history, the boat's notes and logbook, Lume's own counters, and agent metrics. A manual can sit in the same session as the boat's history when you pass `--docs-index` to `lume ti query`.

Both commands need a `lume` binary built with `--features ti`. The published package is built that way. A default `cargo build` does not include them.

The version in this tree is still 0.12.3. Pull request #8 (stemming, and a coordination floor of 1.0 for new indexes) is merged here and is not in the published v0.12.3 binary. `match()` searches the index as it was built. An index built before that change keeps its old token mode until you reindex it with `lume index -f`.

Copy-paste statements are in [SQL-EXAMPLES.md](SQL-EXAMPLES.md).

## Search index: `lume sql`

```bash
lume sql --db <index> "SELECT file, title, score FROM sections WHERE match(body, 'anchor') ORDER BY score DESC LIMIT 10"
lume sql --db <index> --format json "<sql>"
lume sql repl --db <index>
```

`--format` is `table` (the default), `csv`, or `json`. The REPL reads one statement at a time, ending with `;`, and `.quit` exits.

`<index>` is the directory passed to `lume index --db`. The same directory is the MCP tool `lume_sql` (`sql` is required; `db` defaults to the server's index; `max_rows` is 1 to 500).

### Tables

From `src/sql.rs`.

| Table | When it exists | Columns |
|---|---|---|
| `sections` | Always | `id` (uint64), `file` (text, nullable), `title` (text), `line` (uint64), `body` (text), `score` (float, nullable) |
| `entities` | Only if the index has an entity graph | `entity` (text), `doc_count` (uint64) |
| `entity_edges` | Same | `a`, `b` (text), `jaccard`, `relatedness` (float) |

`id` is the section's position in the index, the same number search returns as `section_index`. `line` is the source line number. `score` is filled only when the statement filters with `match()`. Without `match()`, `score` is null. Several `match()` filters keep the score from the first one.

`sections` has no entity column. There is nothing to join `sections` to `entities` on. `entities` joins to `entity_edges` on `entity = a` (or `b`). The tables are registered only when the index has an entity graph (`src/sql.rs`). The repo `.lume-index` has that file with empty `nodes` and `edges`, so the join runs and returns no rows. If the file is missing, selecting from `entities` errors (`tests/lume_sql.rs`).

### `match()`

`match(body, 'terms')` is BM25 over the section body. It has to be a top-level `AND` filter: `WHERE match(body, 'a') AND match(body, 'b')` is the intersection of the two searches (`tests/lume_sql.rs`). The column must be `body` and the query must be a string literal (`src/sql.rs`, `match_query`).

Anything else is not pushed down. A filter DataFusion can apply after the scan, such as `file = 'manual.md'`, still works. A filter that makes DataFusion evaluate `match()` itself fails. That includes `OR` (`WHERE match(body, 'engine') OR id = 1` errors in `tests/lume_sql.rs`) and using `match()` in the select list.

`AND NOT match(body, '...')` is not in this tree. It is coming in the next release on branch `search/not`, which is not merged. On v0.12.3, and on this tree, that form errors. The function refuses to run as a row test and returns: `match(body, 'q') must filter the docs or sections table directly (or use match(notes|logbook|alerts, 'q') on telemetry)` (`crates/ti-sql/src/docs.rs`).

### Limits

- Read-only. `INSERT`, `UPDATE`, `DELETE`, and DDL error. One statement only.
- Results stop at 500 rows or 64 KiB, whichever comes first. The JSON then has `truncated` true and `hint` set to `Aggregate results or narrow the time range.` The object also has `columns`, `rows`, `row_count`, `elapsed_ms`, `pushdown`, and `units` (`crates/ti-sql/src/engine.rs`). `pushdown` says how many scans ran and which filters were pushed down. A fully pushed-down filter looks like `2 scans; conjunct classes: {"Exact"}`. `Inexact` and `Unsupported` show up in that same set when a conjunct was not exact.
- `lume ti query --docs-index <index>` registers these same tables beside telemetry. Sections have no vessel and no timestamp. A join to `telemetry` is whatever relationship you write (`plan/design/lume-sql.md`).

## Telemetry: `lume ti query`

```bash
lume ti query "SELECT vessel, count(*) AS n FROM telemetry GROUP BY vessel" --store <store>
lume ti query "<sql>" --store <store> --json
lume ti repl --store <store>
lume ti explain "<sql>" --store <store>
```

`--json` prints the same JSON object as `lume sql --format json`. `explain` prints which filters became bitmaps. The REPL commands are `.tables`, `.schema`, `.explain`, `.examples`, `.help`, and `.quit` (`crates/ti-sql/src/cli.rs`).

The same statement is available three other ways:

- HTTP, from `lume serve --ti-store <store>` (loopback port 5863 unless you change it): `POST /ti/query` with a JSON body `{"sql":"..."}`. Send `Accept: application/json` and `Content-Type: application/json` to get JSON. Without that Accept header the body is Arrow IPC, with `X-TI-Row-Count` and `X-TI-Truncated` (`src/ti_http.rs`).
- pgwire, for Grafana and `psql`: `lume serve --ti-store <store> --pg 5864`, or the plugin's PostgreSQL switch. Read-only.
- MCP tools on that server: `ti_query`, `ti_schema`, `ti_explain`, `ti_status`, `ti_resolve`.

The plugin's SQL console uses that HTTP call:

```bash
curl -s -H 'Accept: application/json' -H 'Content-Type: application/json' -X POST http://127.0.0.1:5863/ti/query -d '{"sql":"SELECT max(ts) AS latest_ts FROM telemetry"}'
```

Dotted column names are quoted: `"navigation.speedOverGround@max"`.

### Tables

A normal open registers these (`crates/ti-sql/src/session.rs`, schemas in `crates/ti-contracts/src/schemas.rs`):

| Table | Rows | Columns |
|---|---|---|
| `telemetry` | One per vessel and time bucket | `vessel`, `ts`, then one column per stored path, then `notes`, `logbook`, `alerts`, then `entity` |
| `docs` | One per note, logbook entry, alert, or imported document | `id`, `vessel`, `kind`, `ts_start`, `ts_end`, `title`, `body`, `score`, `entity` |
| `vessels` | One per vessel | `ord`, `urn`, `name`, `mmsi`, `first_seen`, `last_seen` |
| `paths` | One per stored field | `path`, `field`, `agg`, `type`, `units`, `scale`, `depth`, `description`, `first_seen`, `last_seen` |
| `shards` | One per shard | `vessel`, `shard_no`, `ts_from`, `ts_to`, `sealed`, `bytes`, `hash` |

`ts`, `ts_start`, `ts_end`, `first_seen`, and `last_seen` are UTC timestamps. `ts_end` and `score` are nullable. `score` on `docs` is null except under `match()` (`crates/ti-sql/src/docs.rs`).

Signal K paths become columns named `path@mean`, `path@min`, `path@max`, `path@last`, and so on. A mean field also appears under the bare path (`navigation.speedOverGround` is the mean). The SQL console's Recent SOG preset uses that bare name (`plugins/signalk-lume-ti/public/index.html`). `path$source` is the list of source refs for that path.

`entity` on `telemetry` is the vessel URN again (`crates/ti-sql/src/provider.rs`). `entity` on `docs` is the same copy of `vessel` (`crates/ti-sql/src/docs.rs`). Agent rows use that column as the agent id.

`kind` on `docs` is `notes`, `logbook`, or `alerts`. OTLP logs are stored as `logbook`.

Two more tables appear only after they have data (`crates/ti-sql/src/engine.rs`):

- `telemetry_lume`, from `<store>/stores/lume`, after the first self-telemetry write. It is Lume's own counters, not the boat. Until that directory has a `catalog`, the table is not registered and a query says it was not found.
- `telemetry_agents`, from `<store>/stores/agents`, after the first OTLP metrics. Token totals use paths such as `claude_code.token.usage@last` (`bench/grafana/lume-agents-dashboard.json`). On a HaLOS Pi that had not ingested agent metrics, v0.12.3 answered: `table 'datafusion.public.telemetry_agents' not found; available tables: telemetry, docs, paths, vessels, shards, telemetry_lume`.

Extra configured stores become `telemetry_<name>` (`crates/ti-sql/src/store.rs`, `table_name_for_store`). The high-resolution navigation store in the README is `telemetry_hr`.

### What is pushed down

Pushed down means the filter or aggregate runs on the bitmaps. The engine does not read every bucket. The JSON `pushdown` field is the check: `2 scans; conjunct classes: {"Exact"}` means every conjunct of that scan was exact (`crates/ti-sql/src/engine.rs` formats the class names with Debug, so the name is quoted). A HaLOS Pi run of the source-list count in [SQL-EXAMPLES.md](SQL-EXAMPLES.md) returned that field.

These are exact (`crates/ti-sql/src/classifier.rs`, `aggregate.rs`):

- `ts` compared with a timestamp, including `ts >= now() - INTERVAL '10 minutes'`. That form is exact (`crates/ti-sql/tests/provider.rs`).
- `vessel` or `entity` equal to a URN string.
- A stored numeric or state column compared with a literal (`>`, `<`, `=`, `BETWEEN`, `IN`).
- `match(notes, 'q')`, `match(logbook, 'q')`, or `match(alerts, 'q')` on `telemetry`. The query is a string literal. This is the text bitmap for buckets those documents cover.
- `match(body, 'q')` on `docs`, as a top-level `AND` filter. Several of them intersect.
- `"some.path$source" = 'ref'`. That equality is rewritten to `array_has` before planning (`crates/ti-sql/src/rewrite.rs`). `array_has("some.path$source", 'ref')` is the same pushdown. It has to be the source column versus a string.
- `GROUP BY vessel` and/or `entity`, and one `date_bin` of `ts`, with `count`, `min`, `max`, or `sum` of an indexed numeric column. The interval has to be a fixed duration (`INTERVAL '1 minute'`, `'1 hour'`, `'1 day'`). Calendar months are not. A null origin is not. Two different `date_bin` sizes are not. Anything else still runs, by reading rows, and `explain` calls that a fallback.

`in_bbox(south, west, north, east)` and `within_nm(lat, lon, nautical_miles)` take those arguments only. The engine supplies `navigation.position.latitude@last` and `longitude@last` (`crates/ti-sql/src/geo.rs`). They are inexact: an H3 cover picks candidate buckets, then latitude and longitude are checked. They error if those position columns are not in the store.

### What is not pushed down

- `LIKE`, arithmetic in the filter, and comparing two columns. The classifier's reason is `expression is outside the frozen bitmap IR`, or `comparison is not column versus literal`.
- An `AND` or `OR` that contains one of those. The whole compound stays unsupported.
- `match()` that is not a direct filter: in the select list, under `OR`, or `AND NOT match()` on `sections` (see above).
- On `docs` and `sections`, every filter except `match()` runs after the matching rows are loaded. A time window on `docs.ts_start` is correct and is not a bitmap time prune.
- `GROUP BY` other than vessel, entity, and one `date_bin`. That includes `GROUP BY file` on `sections`, which is ordinary DataFusion after `match()` has chosen the rows.

### Other limits

- Read-only, one statement, 500 rows, 64 KiB. Same JSON hint as `lume sql`.
- `telemetry_lume` and `telemetry_agents` are absent until the first write, as above.
- Column names depend on what was ingested. `ti_schema`, or `SELECT path, agg, units FROM paths`, is the list for this store. A name that is not there is an unknown column, not a silent null.
