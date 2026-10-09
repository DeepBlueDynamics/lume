# 8. Interfaces

Spec pp. 17–19. Owner: [W7](../lanes/W7-serve.md).

All surfaces are thin wrappers over one `TiEngine` handle. MCP is the primary
consumer, because nemesis8 agents are the main users.

## MCP tools (added to `lume serve`)

| Tool | Input | Returns |
|---|---|---|
| `ti_resolve` | `phrase`, optional `vessel`, `limit` (default 8) | Ranked candidate columns with path, agg, units, description, last value. Uses Lume hybrid search over the `paths` catalog plus Signal K spec descriptions |
| `ti_schema` | optional `prefix`, `type` | Tables, columns, types, units, scale, time coverage |
| `ti_sql` | `sql`, `max_rows` (default 500), `format` (`json`, `csv`, `markdown`) | Rows, truncation flag, elapsed ms, one-line pushdown summary |
| `ti_intervals` | `predicate`, `min_len`, `max_gap`, time range | Interval list (sugar over `intervals()`) |
| `ti_explain` | `sql` | Plan and pushdown report |
| `ti_status` | none | Ingest lag, WAL size, shards open/sealed, last sync per vessel |

Output caps: 500 rows and 64 KB; over that, the first rows plus a hint to
aggregate. Every result echoes the resolved units so the agent never confuses m/s and kn.

## HTTP (same port as `lume serve`)

- `POST /ti/sql` body `{sql, format}` → Arrow IPC stream (`application/vnd.apache.arrow.stream`), JSON or CSV by `Accept`.
- `GET /ti/schema`, `GET /ti/status`.
- `POST /ti/ingest` with NDJSON Signal K deltas, for push sources.
- `GET /ti/manifest`, `GET /ti/shards/{vessel}/{shard}/{version}.tar` for sync.
- Auth reuses NUTS auth on shore. On the boat it binds to the LAN with an optional bearer token.

## CLI (subcommands of `lume`)

```
lume ti init      --width 10s --root ./ti
lume ti ingest    --sk ws://openplotter.local:3000 --token $SK_TOKEN
lume ti backfill  --parquet ~/.signalk/data/parquet
lume ti sql       "SELECT ..."   [--format table|csv|json]
lume ti seal      [--vessel self]          # force-seal past shards
lume ti verify    --oracle duckdb --corpus tests/golden
lume ti sync      --to https://shore.example/ti | s3://bucket/ti
lume ti serve     --port 8080              # alias for lume serve with TI enabled
```

## Signal K plugin `signalk-lume-ti` (thin, Node)

- **Packaging.** Signal K App Store. Prebuilt `lume` binaries for linux-arm64 (Pi 4/5, 64-bit Pi OS / OpenPlotter) and linux-x64 as platform-specific optional npm deps. Statically linked against musl so the same file runs in a containerized Signal K. 32-bit armv7 is a stretch.
- **Auth.** Handles the Signal K access-request flow and stores the device token; the Rust process never needs a user password.
- **Supervision.** Runs `lume ti ingest` and `lume ti serve` as children, restarts on crash. Shows ingest lag, WAL size, disk use in Admin UI.
- **Webapp.** SQL console, saved queries with CSV download, schema browser. PV-1 energy queries ship as defaults.
- **Pin to chart.** On explicit user action only, writes query results to the Signal K Resources API (e.g. `intervals()` as notes or regions Freeboard-SK displays). Writes only resources, never data paths.
- **Stretch (M6+).** Registers as a Signal K History API provider answering `/signalk/v2/api/history/values` from Lume TI, so Freeboard-SK and KIP get fast history.

## Postgres wire protocol

`lume ti serve --pg 5432` exposes the same DataFusion session via the `pgwire` crate.
psql, Grafana's PostgreSQL source, DBeaver, pandas, psycopg work unchanged.

- Simple and extended query protocols, read-only sessions.
- SCRAM password auth from `ti.toml`. LAN-only bind by default.
- DataFusion `information_schema`, plus the minimal `pg_catalog` views Grafana and DBeaver need for introspection.
- Memory pool capped per query: 512 MB on Pi 4, 1 GB on Pi 5. Over the cap, error with a hint to narrow `ts`.
