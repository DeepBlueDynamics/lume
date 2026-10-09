# 1. Purpose, goals, non-goals, done

Spec pp. 1–2.

## Purpose

Add a bitmap-indexed, SQL-queryable telemetry index to Lume ("Lume TI"). It sits
beside Signal K and signalk-parquet so one query can combine numeric thresholds,
discrete states, events, geography and full text across a fleet, returning in
milliseconds. It applies Pilosa/FeatureBase ideas (roaring bitmaps, bit-sliced
indexes, time-sharded column space) in Rust inside Lume. Apache DataFusion
supplies the SQL engine.

## The motivating query

```sql
SELECT vessel, date_bin(INTERVAL '10 minutes', ts) AS win,
       max("environment.wind.speedTrue@max")
FROM telemetry
WHERE "propulsion.port.state" = 'started'
  AND "electrical.bilge.pumpCycles" > 3
  AND "environment.wind.speedTrue@max" > 12.9   -- 25 kn in m/s
  AND match(notes, 'leak OR water')
  AND ts > now() - INTERVAL '90 days'
GROUP BY vessel, win;
```

## Goals

| # | Goal | Meaning |
|---|---|---|
| G1 | Full SQL over Signal K | Read-only SQL over bucketed telemetry and raw samples, one vessel or fleet: joins, CTEs, subqueries, window functions. Served over MCP, HTTP, CLI and Postgres wire (psql, Grafana, DBeaver unchanged). |
| G2 | Fast selective queries | ≥4-predicate queries run in bitmap space. Targets in [13-benchmarks](13-benchmarks.md). |
| G3 | Text and telemetry in one column space | `match()` over notes, logbook, notifications and Meridian VHF transcripts is just another bitmap filter. |
| G4 | Derived and rebuildable | Raw truth stays in signalk-parquet and the delta stream. The index can be dropped and rebuilt any time. |
| G5 | Agent-first | Schema introspection, NL path resolution via Lume hybrid search, `explain` that says what was pushed down. |
| G6 | Fleet-native | Each vessel indexes at the edge. Sealed shards replicate to shore over intermittent links; shore queries every vessel. |
| G7 | Plugs into the boat's Pi | One-click install on Hat Labs HALPI2 (HaLOS) and any OpenPlotter Pi. No compiler, no cloud, no change to OpenCPN or NMEA 2000. Pi 4 (4 GB) floor; CM5/Pi 5 target. |

## Non-goals for v1

- Replacing Signal K storage or the History API provider (History API adapter is a stretch goal).
- Raw-sample fidelity in the bitmap index — raw samples go through a federated `raw` table over Parquet.
- Writes through SQL (no INSERT/UPDATE/DELETE).
- Clustering or consensus. Single-process nodes; fleet scale via shard federation.
- Sub-second alerting. Rules and alarms stay in Signal K.
- Stored procedures, triggers, user-defined schemas. Pushdown changes only speed, never which SQL is accepted.

## Done means

- [ ] Golden query corpus (≥60 queries) matches the DuckDB oracle within fixed-point tolerance.
- [ ] Benchmark targets met on both reference machines.
- [ ] An agent connected only to Lume's MCP server answers the motivating query from a plain-English question.
- [ ] On PV-1's HALPI, Lume TI installs from the Signal K App Store in under 30 minutes; an energy query then runs from psql and from the plugin webapp.
