# Design: 1 s high-resolution store (`telemetry_hr`)

Status: **approved** by the user on 2026-10-06 (1 s, 3-month default retention,
configurable). Decision **D30**. Scheduled after M3. Not in the source spec; it answers
spec/11's open question "10 s, or 1 s with 1-min rollups?".

## Why

At W = 10 s, `@min` and `@max` already keep wind gusts and the shallowest depth, but
the track shape is lost: 10 s at 7 kn is about 36 m between positions. The `raw` table
only helps where signalk-parquet exists, and on HaLOS history lives in InfluxDB.
Signal K delivers navigation data at about 1 Hz, so a 1 s bucket keeps essentially
every sample for the paths where fidelity matters.

## Shape

Two stores per vessel node, each with its own bucket width, written by the same
ingest:

| Store / table | W | Paths | Aggregates | Default retention |
|---|---|---|---|---|
| `telemetry` (existing) | 10 s | all indexed paths | default profile (`@mean/@min/@max`) | 2 years on the Pi |
| `telemetry_hr` (new) | **1 s** | allow-list (below) | per-path (below) | **90 days**, configurable |

Default high-resolution allow-list and aggregates:

| Path | Aggregates | Why |
|---|---|---|
| `navigation.position` (lat/lon BSI + H3 cells) | `@last` | the track |
| `navigation.speedOverGround`, `.courseOverGroundTrue`, `.headingMagnetic`/`.headingTrue` | `@last` | the track |
| `environment.wind.speedTrue`, `.speedApparent`, `.angleTrueWater`, `.angleApparent` | `@mean`, `@max` | gusts and shifts |
| `environment.depth.belowTransducer` (and `belowKeel` if present) | `@min` | the shallowest reading |

A bucket at 1 s holds about one sample, so a single aggregate per path is enough;
`@mean` + `@max` on wind keeps the gust if the data rate is above 1 Hz.

## Configuration (`ti.toml`)

```toml
[stores.default]          # the existing 10 s store
width = "10s"
retention = "730d"        # Pi

[stores.hr]
width = "1s"
retention = "90d"         # Pi: about 3 months, configurable
shore_retention = "90d"   # shore node; set "forever" to keep everything ashore
paths = ["navigation.position", "navigation.speedOverGround", "..."]   # allow-list
[stores.hr.aggs]
"navigation.*" = ["last"]
"environment.wind.*" = ["mean", "max"]
"environment.depth.*" = ["min"]
```

- `retention` is enforced by the store's existing retention sweep (W2, "drop shards
  older than X"). It's now per store, not global.
- `width` is fixed at store creation (spec/05); changing it means rebuilding that store only.
- If `[stores.hr]` is omitted, there is no high-resolution store, and v1 behaves exactly as today.

## Size

About 15–20 fields at 1 s comes to roughly 0.5–2 GB per vessel-year, depending on the
time underway. With 90-day retention that's **about 0.15–0.5 GB on the Pi**. A shard is
2^16 one-second buckets, about 18.2 h, so 90 days is about 120 shards per vessel.

## Work, by lane (after M3)

- **Contracts PR (lead approval):** `ti.toml` gains `[stores.<name>]` tables (width,
  retention, shore_retention, path allow-list, aggregate map). The frozen contract types
  are unchanged: `BucketIx` is already relative to each store's W.
- **W3 ingest:** fan each normalized sample out to every store whose allow-list matches,
  with a bucketer and watermark per store (the 30 s lateness rule is unchanged).
- **W2 store:** retention per store; an independent store root, e.g. `ti/stores/hr/`.
- **W4 SQL:** register `telemetry_hr` with the same providers and pushdown. Joins with
  `telemetry` work on `vessel` plus `date_bin(...)`. `intervals()` and `within_nm` work
  on either table.
- **ti-bench and corpus:** a 1 s fixture for the allow-listed paths, and at least 6
  golden queries on `telemetry_hr` (a track replay, a gust max, the shallowest depth
  along a track, `within_nm` on a 1 s track, a cross-table join, and a retention-boundary
  query).
- **W8 bench:** add `telemetry_hr` size per vessel-quarter and Q1/Q7 timings to the report.

## Acceptance

- With `[stores.hr]` absent, the default build and all existing corpus results are unchanged.
- With it present, the `telemetry_hr` golden queries match the DuckDB oracle bucketed at 1 s.
- A retention test: data older than `retention` is gone from the high-resolution store
  and still present in the 10 s store.
- Pi budget: ingest stays within spec/03 limits (≤ 25 % of one core, ≤ 400 MB RSS)
  with both stores running.
