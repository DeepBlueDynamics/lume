# Design: `lume sql` over ordinary Lume search indexes

Status: **approved to implement** (user, 2026-10-06). Not in the source spec; this is an
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

- `sections.id` is the zero-based persisted section index returned as `section_index` by the search API. `file` is nullable; `line` is the indexed line number. Scores are null without a direct `match()` filter; multiple match filters intersect and use the first filter's score. Sorting equal scores can use `id` as a deterministic tie-break.
- `match(body, q)` runs BM25 through the in-process search API ([search-api.md](search-api.md), `SearchMode::LexicalOnly`), and is pushed down as Exact. Optionally `match(body, q, mode => 'hybrid')` when Shivvr is reachable.
- Everything else is standard DataFusion: joins, CTEs, window functions.
- Read-only; same INSERT/UPDATE/DELETE/DDL rejection as TI.
- Also exposed as an MCP tool `lume_sql` beside `lume_search`.

## Shared document and telemetry sessions

Lead-approved scope (2026-10-06): `lume ti query --docs-index <lume-index>`
registers `sections`, and optional graph tables, alongside telemetry in one
read-only session. A manual can be filtered with `match(s.body, q)` and joined
or cross-joined to telemetry. Ordinary sections have no implied timestamps or
vessel identity; the SQL author supplies the relationship.

Hybrid match modes remain outside this lexical-only slice.

## Dependencies

- In-process search API extraction ([search-api.md](search-api.md)). This is a hard prerequisite.
- DataFusion pin from W4. The feature sits behind `ti` (or a narrower `sql` feature, decided in Zygomorphic Prawn's dependency survey).

## Tasks

- [x] `SectionsTable` provider over `LoadedIndex.bm25` (projection-aware; `score` virtual).
- [x] `match()` UDF with pushdown: matched section ids → filter.
- [x] `entities` / `entity_edges` providers from `EntityGraph`.
- [x] `lume sql` CLI subcommand (`--format table|csv|json`).
- [x] `lume_sql` MCP tool (row/size caps like `ti_sql`).
- [x] Tests on `docs/monte_cristo`: hits from `match()` equal `lume search` hits for the same query and limit.

## Acceptance

- `SELECT id FROM sections WHERE match(body, q) ORDER BY score DESC LIMIT k` returns the same ids, in the same order, as `lume search q -l k -a 0` (lexical) on the same index.
- Default `lume` build size unchanged without the feature.

## Implementation and validation (2026-10-06)

`src/sql.rs` provides projection-aware `SectionsTable`, lexical Exact match
pushdown, optional entity tables, the CLI/REPL, and the in-process `lume_sql`
MCP adapter. No new dependency; DataFusion is re-exported by the optional
`ti-sql` crate. The default build does not compile this module or its surfaces.

`ti-sql::cli::run_with_index` accepts a root-injected registrar, preserving
LumeText injection for TI's own docs. `--docs-index` works for query, explain
and REPL, without changing stored Arrow schemas or index files.

Local integration tests: ten Monte Cristo match counts and scored hit sets,
projected counts, null scores without match, entity graph rows, read-only
statements, MCP 500-row/64-KiB caps, multiline REPL recovery, and a durable
manual/telemetry cross-join through `lume ti query --docs-index`: **3 passed**.
Full root feature suite: **73 passed, 0 failed, 2 fixture-dependent tests ignored**.
TI CLI parser unit tests: **2 passed**. Default `cargo build` without `ti`: **passed**.
All builds were sequential with `CARGO_INCREMENTAL=0` and debug info disabled
through environment overrides to stay below the 8-GB target cap.
Host rustfmt and strict clippy remain pending host validation.
