# W5 — `ti-text` (weeks 6–8, gates M4)

Depends on: W0 contracts, **Lume search API** (existing crate).
Spec: [05-data-model](../spec/05-data-model.md) (text mapping), [06-ingest](../spec/06-ingest.md) (documents source), [07-query](../spec/07-query.md) (`match()`).

## Owns

Document ingestion from notes, logbook and notifications; `match()` → bucket
bitmaps; LRU cache. Implements `TextIndex`.

## Tasks

- [ ] Expose a **library-level** Lume BM25 search API usable from `ti-text` (today MCP tools shell out to the CLI — see [repo-fit](../repo-fit.md) §3).
- [ ] Poll `GET /signalk/v2/api/resources/notes` every 60 s; ingest logbook plugin entries when installed; notification messages from the live stream (hand-off from W3).
- [ ] Meridian VHF transcripts as documents (format TBD — no spec yet).
- [ ] Each doc → Lume document with `ts_start`, optional `ts_end`, `kind`, `vessel`; stored under `ti/docs/`.
- [ ] `match_buckets(vessel, kind, q, from, to)` → `RoaringTreemap` of global ColumnIds covering each hit's time range.
- [ ] LRU cache per `(q, shard)`.
- [ ] `docs` table rows (with `score` populated only under `match()`), handed to W4.
- [ ] Lume query-syntax compatibility (`'leak OR water'`).

## First deliverable
`match()` subset of the golden corpus green.

## Gate (M4)
- [ ] Full golden corpus green, text queries included

## Open
- Does `match()` use pure BM25 or Lume's hybrid (semantic) path? Spec says "Lume BM25" — semantic needs Shivvr running, which matters on the boat.
- Meridian transcript format/source is unspecified.
