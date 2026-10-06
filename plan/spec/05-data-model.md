# 5. Data model

Spec pp. 8–11.

Every column is one time bucket for one vessel. Every field is a set of
roaring-bitmap rows over those columns. A shard is one vessel × 65,536
consecutive buckets, so each row in a shard fits exactly one roaring container
(≤ 8 KB).

## Column space

- **Bucket width `W`:** per deployment, default 10 s. Fixed once a store is created; changing it means a rebuild.
- **Bucket index:** `b = floor((t − EPOCH) / W)`, `EPOCH = 2020-01-01T00:00:00Z`.
- **Column id:** `col: u64 = (vessel_ord as u64) << 32 | b`. 32-bit bucket index ≈ 1,360 years at 10 s.
- **Shard key:** `(vessel_ord, b >> 16)`. At 10 s a shard spans 7.58 days; ~48 shards per vessel-year.
- **Local column:** `b & 0xFFFF`, stored in a `RoaringBitmap` (u32) per row per shard.
- **Vessel ordinal:** dense u32 from the `vessels` catalog, keyed by Signal K context URN (e.g. `vessels.urn:mrn:imo:mmsi:367000000`). `vessels.self` resolves to its URN at ingest.

## Field types

| Type | Used for | Rows per shard | Predicates |
|---|---|---|---|
| `presence` | one per indexed path; bit set if any sample landed in the bucket | 1 | `IS NOT NULL`, `IS NULL` |
| `set` | strings, enums, booleans, `$source` labels, derived states | 1 per distinct value | `=`, `!=`, `IN`, `NOT IN` |
| `bsi` | numeric values, one field per aggregate | exists + sign + `depth` magnitude rows | `=`, `<`, `<=`, `>`, `>=`, `BETWEEN`; `sum`/`min`/`max`/`count` pushdown |
| `count` | event counts per bucket (pump cycles, alarms) | a `bsi` with no sign row | as `bsi` |
| `geo` | H3 cells of `navigation.position` at res 5, 7, 9 | 1 per occupied cell | `in_bbox()`, `within_nm()` (inexact, refined by lat/lon BSI) |
| `text` | Lume BM25 documents mapped onto buckets | resolved on demand from Lume postings | `match()` |

## BSI encoding

- **Fixed-point:** `i = round(v × 10^scale)`. Scale from the path's Signal K `meta.units` via the registry below; per-path override allowed.
- **Sign-magnitude:** rows `exists`, `sign`, then `bit[0..depth)`. Depth grows when a value exceeds current range; a wider depth only adds rows, never rewrites old ones.
- **Comparisons:** O'Neil/Quass bit-sliced range algorithm (as Pilosa). One pass over `depth` rows, AND/OR/ANDNOT only.
- **Tolerance:** oracle comparison allows ±0.5 × 10^−scale.

### Unit → scale registry

| Signal K unit | Scale | Resolution |
|---|---|---|
| `rad` | 4 | 0.0001 rad (≈ 0.006°) |
| `m/s` | 3 | 1 mm/s |
| `K` | 2 | 0.01 K |
| `V` | 3 | 1 mV |
| `A` | 2 | 10 mA |
| `W` | 1 | 0.1 W |
| `Pa` | 0 | 1 Pa |
| `ratio` | 4 | 0.01 % |
| `m` | 2 | 1 cm |
| `Hz` | 1 | 0.1 Hz |
| `s`, `J`, `C` | 0 | 1 unit |
| lat/lon (deg) | 7 | ≈ 1.1 cm |
| unknown numeric | 3 | logged as a registry miss |

## Bucket aggregates

Each numeric path keeps running `count`, `sum`, `min`, `max`, `last` per bucket.
The bucket closes into BSI fields named `path@agg`. The bare path is an alias for `@mean`.

- **Default profile:** `@mean`, `@min`, `@max`.
- **`slow` profile:** `@last` only — for paths with median sample interval ≥ `W` (e.g. tank levels).
- **Opt-in:** `@last` and `@count` on any path.

## Derived fields (rules in `ti.toml`, computed at bucket close)

- **Transitions:** e.g. `propulsion.*.state` changing to `started` → `count` field `…@starts`.
- **Edge counts:** a boolean path's rising edges per bucket (e.g. bilge pump cycles).
- **Notifications:** each `notifications.*` path → `set` field over state values (`normal`, `alert`, `warn`, `alarm`, `emergency`), plus a `count` of raises.

## Multiple sources

- Value comes from the preferred source (Signal K source priorities if configured, else first seen).
- Every source reporting in the bucket is recorded in a `path$source` set field, so `WHERE "navigation.speedOverGround$source" = 'n2k.115'` works.
- Per-source values are a v2 option.

## Text mapping

Notes (Resources API), logbook entries and notification messages are indexed as
Lume documents with `ts_start` and optional `ts_end`. `match(notes, q)` runs
Lume BM25 over docs of that kind; each hit becomes the bucket columns its time
range covers. The resulting bitmap is cached per `(q, shard)` with an LRU.

## Catalogs (JSON in store root, exposed as SQL tables)

- `vessels(ord, urn, name, mmsi, first_seen, last_seen)`
- `paths(path, field, agg, type, units, scale, depth, description, first_seen, last_seen)` — description from Signal K `meta` and the spec
- `shards(vessel, shard_no, ts_from, ts_to, sealed, bytes, hash)`

## Size budget

- **Worst case:** dense BSI field at depth 16 = 18 rows × 8 KB × 48 shards ≈ 6.9 MB per vessel-year at 10 s.
- **Typical boat:** 120 numeric paths × default profile ≈ 360 fields → ≤ 2.5 GB per vessel-year worst case; run containers and sparse paths bring expected size to 0.6–1.0 GB.
- **Pi retention:** default 2 years. Older shards live only on shore.
