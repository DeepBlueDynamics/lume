# Typed Metadata, Filters, and Facets

Lume supports typed metadata attached to indexed documents and sections, bitmap-accelerated query filtering before BM25 scoring, native count-ordered facets, and first-class SQL integration over DataFusion.

Metadata can be declared directly in Markdown files via YAML frontmatter or externally across directories using JSON Lines manifests (`lume.meta.jsonl`).

---

## 1. Defining Metadata

Lume ingests metadata from two primary sources during `lume index`:

### YAML Frontmatter

Markdown files can open with a YAML frontmatter block demarcated by `---`:

```markdown
---
title: "Deep Sea Navigation"
category: navigation
tags: [marine, offshore, gps]
year: 2024
draft: false
---
# Chapter 1: Waypoints
Content begins here...
```

- Frontmatter lines are extracted and blanked out during parsing so 1-based line numbers in subsequent sections match the original file line numbers exactly.
- Supports scalars (strings, numbers, booleans) and lists (inline `[a, b]` or multi-line `- item`).

### Metadata Manifests (`lume.meta.jsonl`)

External metadata can be associated with files without modifying document contents by placing `lume.meta.jsonl` files in the directory tree:

```json
{"path": "manuals/engine.md", "category": "engineering", "year": 2022, "tags": ["diesel", "propulsion"]}
{"path": "manuals/safety.md", "category": "safety", "year": 2025, "tags": ["emergency", "liferaft"]}
```

- Each line is a JSON object with a required `"path"` relative to the manifest directory (or target root).
- Manifest rows override frontmatter properties on key collisions.
- Deeper manifests (located in subdirectories) take precedence over shallower manifests.

### Schema Overrides (`lume.schema.json`)

By default, types are inferred automatically from ingested values (and widened if necessary, e.g. mixing integers and floats widens to `float`). To enforce strict types, add an optional `lume.schema.json` file in the root target directory:

```json
{
  "category": "keyword",
  "tags": "keyword_list",
  "year": "integer",
  "rating": "float",
  "published_at": "date"
}
```

---

## 2. Supported Types and Internal Storage

Metadata is stored in columnar structures backed by [MiniRoaring](../src/fast_retrieval.rs) bitmaps for high-performance set operations:

| Type | Description | Internal Representation | Capabilities |
|---|---|---|---|
| `keyword` | Single-valued string | String dictionary + 1 roaring bitmap per unique term | Case-insensitive equality (`category:biology`, `-category:draft`) |
| `keyword_list` | Multi-valued string array | String dictionary + offsets/ords + 1 roaring bitmap per unique term | Case-insensitive membership (`tags:gps`, `-tags:deprecated`) |
| `integer` | 64-bit signed integer | Dense/sparse `i64` array + present-run intervals | Equality and comparisons (`year:>=2020`, `year:<2025`, `year:2020..2024`) |
| `float` | 64-bit floating point | Dense/sparse `f64` array + present-run intervals | Equality and comparisons (`rating:>4.5`) |
| `date` | ISO-8601 date / timestamp | Epoch seconds / timestamp array | Date comparisons (`pub:>=2024-01-01`) |

---

## 3. Query Filters

Filters can be passed to `lume search` via the `-F` / `--filter` option (repeatable or space-separated):

```bash
lume search "emergency procedure" -F "category:safety" -F "year:>=2020"
```

### Syntax Reference

- **Exact Equality**: `field:value` (case-insensitive keyword match). Strings with spaces can be quoted: `author:"Jane Doe"`.
- **Negated Equality**: `-field:value` excludes sections matching the given value (e.g. `-status:archived`).
- **Numeric Comparisons**:
  - Greater than / equal: `year:>=2020`, `price:>100`
  - Less than / equal: `year:<=2024`, `price:<50`
  - Bounded range: `year:2020..2024` (inclusive)
- **Negated Comparisons**: `-year:<2000` (excludes sections with year less than 2000).

### Execution Architecture: Filter-Before-Scoring

Query filters are evaluated against MiniRoaring bitmaps **before** BM25 scoring:
1. Bitmap operations compute the exact matching candidate section set across all filter clauses.
2. In `search_top_k_filtered`, candidate sections outside the filtered bitmap are never scored or traversed.
3. This provides **limit invariance**: top-K rankings are identical regardless of whether limit is 1 or 1000, and filtered search runs significantly faster than post-filtering.

---

## 4. Native Facets

Facets aggregate value counts across the candidate set matching the query and filters. Request facets using `--facet <field>`:

```bash
lume search "turbine" -F "year:>=2020" --facet category --facet tags --json
```

### Output Format

```json
{
  "hits": [ ... ],
  "facets": {
    "category": {
      "buckets": [
        { "val": "engineering", "count": 14 },
        { "val": "maintenance", "count": 5 }
      ],
      "missing": 1
    },
    "tags": {
      "buckets": [
        { "val": "diesel", "count": 12 },
        { "val": "electric", "count": 8 },
        { "val": "auxiliary", "count": 3 }
      ],
      "missing": 0
    }
  }
}
```

### Ordering Contract

Facet buckets follow a strict deterministic sorting order:
1. **Count descending**: Highest frequency values appear first.
2. **Value ascending**: Alphabetical order is used as a tie-breaker when counts are equal.
3. **`missing`**: Tracks the number of candidate sections where the field is `null` or unassigned.

---

## 5. SQL Integration (`lume sql`)

When built with `--features ti`, metadata fields are exposed directly as first-class columns on the `sections` table in `lume sql` and `lume ti query --docs-index`:

- Single-valued keywords appear as `Utf8` columns.
- Multi-valued keyword lists appear as `List(Utf8)` columns.
- Integers and floats appear as `Int64` and `Float64`.

### Filter Pushdown

Filter predicates on metadata columns (such as `WHERE year >= 2020` or `WHERE category = 'navigation'`) push down directly into the table scan as `TableProviderFilterPushDown::Exact`. Sections that do not match the filter are excluded at the storage layer without executing a DataFusion `FilterExec` step.

### Facet Aggregations in SQL

Native facet behavior can be replicated in SQL using standard `GROUP BY`:

**Single-valued Column (`category`):**
```sql
SELECT category, count(*) AS n
FROM sections
WHERE match(body, 'turbine') AND category IS NOT NULL
GROUP BY category
ORDER BY n DESC, category ASC;
```

**Multi-valued Column (`tags`):**
```sql
SELECT tag, count(*) AS n
FROM (
    SELECT unnest(tags) AS tag
    FROM sections
    WHERE match(body, 'turbine')
)
GROUP BY tag
ORDER BY n DESC, tag ASC;
```

---

## 6. Index Format Version 3 Rules

Lume uses an explicit versioning policy to guarantee backwards compatibility and atomic disk consistency:

- **Format Version 1**: Legacy unstemmed index without metadata.
- **Format Version 2**: Default stemmed index without metadata. An index built over plain documents with no frontmatter and no manifests remains at version 2.
- **Format Version 3**: Dedicated format version for indexes containing typed metadata.

### Loader & Writer Invariants

1. **Format Version 3 requires `meta.json`**:
   - When `format_version == 3` in `state.json`, `LoadedIndex::open` checks that `meta.json` exists and its `num_sections` matches `bm25.json`.
   - If `meta.json` is missing or corrupted, the loader refuses with an actionable error prompting to reindex with `lume index -f`.
2. **Atomic Write Ordering**:
   - `flush_searchable_indexes` writes `meta.json` atomically (via `.tmp` file and rename) *before* `state.json` is written.
   - If an index has no metadata fields, any existing `meta.json` is removed and `state.json` is written with `format_version: 2`.
3. **Backwards Compatibility**:
   - Existing v2 and v1 indexes continue to load cleanly with `meta: None`.

---

## 7. Examples

The following examples demonstrate common usage patterns. Each example is annotated with its verification status.

### Example 1: Basic Indexing with Frontmatter
*Status: Verified (tested in `src/meta.rs:test_frontmatter_subset_and_line_numbers`)*

```bash
lume index docs/ -f
```
Extracts YAML frontmatter from Markdown files, derives column types, and persists `meta.json` along with `state.json` (`format_version: 3`).

### Example 2: Filtering Search by Exact Keyword
*Status: Verified (tested in `src/search.rs:test_search_top_k_filtered`)*

```bash
lume search "safety inspection" -F "category:marine"
```
Limits BM25 candidate scoring exclusively to documents with `category == "marine"`.

### Example 3: Range Filter on Integer Field
*Status: Verified (tested in `tests/lume_sql.rs:metadata_columns_pushdown_and_facets_equivalence`)*

```bash
lume search "regulations" -F "year:>=2022"
```
Evaluates the integer range filter against section bitmaps prior to BM25 candidate ranking.

### Example 4: Multi-Field Compound Filter
*Status: Verified (tested in `src/meta.rs` unit tests)*

```bash
lume search "propulsion" -F "category:engineering" -F "-status:draft" -F "year:2020..2025"
```
Combines keyword inclusion, keyword exclusion, and a bounded integer range.

### Example 5: Native Facet Aggregation
*Status: Verified (tested in `tests/lume_sql.rs:metadata_columns_pushdown_and_facets_equivalence`)*

```bash
lume search "cancer" --facet tags --json
```
Returns hits and a count-descending frequency distribution across tags:
```json
{
  "facets": {
    "tags": {
      "buckets": [
        { "val": "dna", "count": 2 },
        { "val": "clinical", "count": 1 }
      ],
      "missing": 0
    }
  }
}
```

### Example 6: SQL Filter Pushdown on Sections
*Status: Verified (tested in `tests/lume_sql.rs:metadata_columns_pushdown_and_facets_equivalence`)*

```sql
SELECT id, title, year FROM sections WHERE year >= 2020 ORDER BY id;
```
DataFusion pushes `year >= 2020` into `SectionsTable`, avoiding physical table-level filter execution.

### Example 7: SQL Facet Equivalence for Keyword Lists
*Status: Verified (tested in `tests/lume_sql.rs:metadata_columns_pushdown_and_facets_equivalence`)*

```sql
SELECT tag, count(*) AS n
FROM (SELECT unnest(tags) AS tag FROM sections WHERE match(body, 'cancer'))
GROUP BY tag
ORDER BY n DESC, tag ASC;
```
Produces identical buckets and counts to native `--facet tags`.

### Example 8: Directory Manifest Override
*Status: Verified (tested in `src/meta.rs:test_manifest_parsing_and_resolution`)*

```json
{"path": "reports/q3.md", "department": "operations", "reviewed": true}
```
In `reports/lume.meta.jsonl`, associates `department` and `reviewed` metadata with `reports/q3.md` without modifying file content.

### Example 9: Plain Index Compatibility Check
*Status: Verified (tested in `tests/lume_sql.rs:test_plain_index_writes_format_version_2_and_loads`)*

```bash
lume index docs/monte_cristo --db .plain-index
```
A corpus without metadata writes `state.json` with `format_version: 2` and no `meta.json`. `LoadedIndex::open` succeeds with `meta.is_none()`.
