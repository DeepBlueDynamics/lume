# Design: `lume sql` over ordinary Lume search indexes

Status: **approved to plan** (user, 2026-10-06). Not in the source spec; this is an
addition. Build after W4 brings DataFusion in (after M3).

## Why

Lume TI's SQL only reaches TI's own `ti/docs/` index of notes, logbook entries and
notifications. Indexes built with `lume index <dir>` are invisible to SQL. Once
DataFusion is in the build, exposing them is one read-only `TableProvider` over the
existing `Bm25Index`. It needs no bitmap engine.

## Surface

```
lume sql --db .lume-index "SELECT file, count(*) FROM sections WHERE match(body, 'Dantès') GROUP BY file"
```

| Table | Rows are | Columns |
|---|---|---|
| `sections` | one per indexed chunk | `id`, `file`, `title`, `line`, `body`, `score` (set only under `match()`) |
| `entities` | one per SKG entity (if `entity_graph.json` exists) | `entity`, `doc_count` |
| `entity_edges` | one per SKG edge | `a`, `b`, `jaccard`, `relatedness` |

- `match(body, q)` runs BM25 through the in-process search API ([search-api.md](search-api.md), `SearchMode::LexicalOnly`), and is pushed down as Exact. Optionally `match(body, q, mode => 'hybrid')` when Shivvr is reachable.
- Everything else is standard DataFusion: joins, CTEs, window functions.
- Read-only; same INSERT/UPDATE/DELETE/DDL rejection as TI.
- Also exposed as an MCP tool `lume_sql` beside `lume_search`.

## Not in scope

Joins with telemetry. Plain indexes have no time ranges, so a join has no key to use.
A `sections.ts` column could be added later for corpora that carry dates.

## Dependencies

- In-process search API extraction ([search-api.md](search-api.md)). This is a hard prerequisite.
- DataFusion pin from W4. The feature sits behind `ti` (or a narrower `sql` feature, decided in Zygomorphic Prawn's dependency survey).

## Tasks

- [ ] `SectionsTable` provider over `LoadedIndex.bm25` (projection-aware; `score` virtual).
- [ ] `match()` UDF with pushdown: matched section ids → filter.
- [ ] `entities` / `entity_edges` providers from `EntityGraph`.
- [ ] `lume sql` CLI subcommand (`--format table|csv|json`).
- [ ] `lume_sql` MCP tool (row/size caps like `ti_sql`).
- [ ] Tests on `docs/monte_cristo`: hits from `match()` equal `lume search` hits for the same query and limit.

## Acceptance

- `SELECT id FROM sections WHERE match(body, q) ORDER BY score DESC LIMIT k` returns the same ids, in the same order, as `lume search q -l k -a 0` (lexical) on the same index.
- Default `lume` build size unchanged without the feature.
