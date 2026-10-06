# W6 — `ti-geo` (weeks 6–8, gates M4)

Depends on: W0 contracts.
Spec: [05-data-model](../spec/05-data-model.md) (`geo` field), [07-query](../spec/07-query.md) (`in_bbox`, `within_nm`).

## Owns

H3 indexing (`h3o` crate), bbox and radius cover, refine.

## Tasks

- [x] Add `h3o` (decisions log entry).
- [x] `cells_for(lat, lon) -> [res5, res7, res9]` helper used by W3 normalize → `FieldValue::Cells`.
- [x] `geo` field rows: one bitmap per occupied cell per shard.
- [x] `bbox_cover(lat_min, lon_min, lat_max, lon_max) -> Vec<u64>` choosing resolution by area.
- [x] `radius_cover(lat, lon, radius_nm) -> Vec<u64>`.
- [x] Antimeridian and pole handling.
- [x] Refine: `in_bbox` against lat/lon BSI; `within_nm` by haversine on materialized rows (Inexact pushdown — DataFusion re-applies).
- [x] Proptest: cover never misses a point.
- [x] Measure false-positive bucket rate at res 9.

## First deliverable
Implemented: 600 random bbox/radius cases, pole/dateline/degenerate boundary fixtures; the res9 local grid measured 4,435 false-positive buckets among 27,085 candidates (16.3744%). See [ti-geo README](../../crates/ti-geo/README.md). This is a local measurement, not a global false-positive bound.

## Gate (M4)
- [ ] Full golden corpus green incl. `in_bbox`, `within_nm`
