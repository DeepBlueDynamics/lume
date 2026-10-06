# W6 — `ti-geo` (weeks 6–8, gates M4)

Depends on: W0 contracts.
Spec: [05-data-model](../spec/05-data-model.md) (`geo` field), [07-query](../spec/07-query.md) (`in_bbox`, `within_nm`).

## Owns

H3 indexing (`h3o` crate), bbox and radius cover, refine.

## Tasks

- [ ] Add `h3o` (decisions log entry).
- [ ] `cells_for(lat, lon) -> [res5, res7, res9]` helper used by W3 normalize → `FieldValue::Cells`.
- [ ] `geo` field rows: one bitmap per occupied cell per shard.
- [ ] `bbox_cover(lat_min, lon_min, lat_max, lon_max) -> Vec<u64>` choosing resolution by area.
- [ ] `radius_cover(lat, lon, radius_nm) -> Vec<u64>`.
- [ ] Antimeridian and pole handling.
- [ ] Refine: `in_bbox` against lat/lon BSI; `within_nm` by haversine on materialized rows (Inexact pushdown — DataFusion re-applies).
- [ ] Proptest: cover never misses a point.
- [ ] Measure false-positive bucket rate at res 9.

## First deliverable
Cover never misses a point (proptest); ≤ 30 % false-positive buckets at res 9.

## Gate (M4)
- [ ] Full golden corpus green incl. `in_bbox`, `within_nm`
