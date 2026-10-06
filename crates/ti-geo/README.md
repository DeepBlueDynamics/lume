# ti-geo

Pure-Rust H3 cells at resolutions 5, 7 and 9, conservative bbox/radius covers, and exact coordinate refinement. The root `ti` feature includes this crate. D31 pins h3o 0.11.0 and geo 0.33.1; geo defaults and native PROJ bindings are disabled.

`cells_for(lat, lon)` validates finite WGS84 coordinates and returns the three cells. `bbox_cover` uses H3 Covers tiling and chooses res9 for area <=25 km², res7 for <=2500 km², otherwise res5. Rectangle pieces are at most 90° wide; wrapped longitude bounds split at the dateline. Polar caps cover all longitudes. A tiny 1e-7° expansion handles degenerate boxes and coordinate quantization. Covers are sorted and deduplicated. `bbox_cover_at_resolution` allows explicit measurement at an ingest resolution.

`radius_cover` tiles a conservative spherical bounding box. Refinement uses haversine with R=3440.065 nautical miles, matching the corpus oracle. Endpoints and radius comparisons are inclusive; radius must be finite and nonnegative. This bounding-box cover deliberately includes spatial false positives.

SQL binds latitude@last/longitude@last through typed analysis, pushes OR(H3 cover, BSI bounding envelope) as Inexact, and keeps exact residual refinement. The independent BSI envelope preserves separately reported coordinates, preferred-source combinations, quantization, and legacy shards without real H3 cells. Negated geo never complements an inexact cover.

Verified on the initial W6 implementation:
- Two unit tests and four cover tests passed, including 300 random bbox and 300 random spherical-radius cases.
- Pole, antimeridian, exact boundary and zero-area cases passed.
- Res9 uniform local-grid measurement: 90,601 synthetic one-point buckets, 22,650 exact hits, 27,085 candidates, 4,435 false positives; 16.3744% of candidate buckets (target <=30%). This is a local bbox fixture, not a global bound or the Q7 corpus result.
- `cargo tree -p ti-geo -d` contains no duplicate geo or geo-types. The normal/build dependency tree contains no C compiler/native geometry dependency.

Run `cargo test -p ti-geo -- --nocapture` for properties and measured output. SQL integration tests live in ti-sql/tests/geo.rs; live ingest wiring tests live in ti-ingest/tests/geo.rs. Full corpus Q7 verification requires the generated expected JSONs and a populated TI store; it is a separate acceptance gate.
