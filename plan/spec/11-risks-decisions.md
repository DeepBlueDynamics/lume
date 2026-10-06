# 11. Risks, open questions, decisions

Spec pp. 28–31.

The biggest risk is that bitmaps don't beat DuckDB by enough on single-boat data
to justify a second engine. The M6 benchmark gate exists to find that out early
and cheaply.

## Risks

| Risk | Signal | Mitigation |
|---|---|---|
| Bitmap speedup < 5× vs DuckDB on Q2, Q3, Q5, Q6 | M6 bench report | Keep `ti-sql` and route broad queries to `raw`. Ship only text, geo and `intervals()`, where bitmaps are unique |
| Pi memory pressure on year-long queries | RSS > 1 GB in Q4 | Shard-at-a-time streaming, mmap read path, evaluate `croaring` frozen views in M4 |
| Path explosion (AIS, per-device sources, plugin noise) | > 2,000 fields per vessel | Allow/deny lists in `ti.toml`, `slow` profile auto-detection, field cap with a `ti_status` warning |
| Fixed-point surprises (wrong units, missing meta) | Registry misses logged at ingest | Unit-to-scale table plus per-path override; `ti_status` lists paths without units |
| Clock skew and backfilled history | Updates far from receive time | 5-min skew rule, repair queue, versioned shards |
| DataFusion API churn | Breaking change on upgrade | Pin major version, upgrade quarterly behind the golden corpus |
| Bucket semantics mislead users ("max" of a 10 s mean) | Agent answers disagree with raw | `@agg` naming mandatory in output, `ti_sql` echoes it, `raw` available for exact checks |

## Open questions

- [x] Default bucket width: 10 s, or 1 s with 1-min rollups? (1 s is 10× the index size). **Resolved by D30:** keep 10 s for everything, plus a separate 1 s store for navigation, wind and depth with 90-day configurable retention.
- [ ] Per-source values as first-class columns in v1, or only `$source` sets?
- [ ] AIS contacts as a second table (`contacts`, columns = observer × bucket, rows = MMSI), or out of scope?
- [ ] Shore storage: local NVMe only, or sealed shards in object storage with a read cache?
- [ ] Should `ti_resolve` also index Signal K spec descriptions for paths a vessel has never reported?
- [ ] *(added in review)* Does anything besides sealed shards move to shore? p. 7 says only sealed shards; p. 20 ships open-shard WAL tails every 5 min. Reword p. 7 or drop WAL-tail sync.
- [ ] *(added in review)* PV-1 Done criterion: should the HALPI install be "from the Signal K App Store" (p. 2) or from the HaLOS Marine container store (p. 19)?
- [ ] Licensing check: borrow algorithm ideas only from FeatureBase (Apache-2.0), copy no code, keep Lume BSD-3-clean.

## Decisions log

| # | Decision | Why |
|---|---|---|
| D1 | Index is derived and rebuildable; Parquet and deltas stay source of truth | No migration risk; the oracle comes for free |
| D2 | Build in Lume (Rust, roaring) rather than revive FeatureBase (Go, archived Feb 2024) | One binary, BM25 postings already roaring-native, Pi-friendly |
| D3 | DataFusion for SQL | Mature planner, Arrow-native, `TableProvider` pushdown API, can also query Parquet (`raw`) |
| D4 | Shard = vessel × 2^16 buckets | One roaring container per row per shard; vessel-local seal and sync |
| D5 | Sealed, content-hashed shard is the unit of replication | Idempotent sync over flaky links; shore needs no merge logic |
| D6 | DuckDB oracle defines correctness | Deterministic acceptance for agent lanes |
| D7 | `Predicate` IR is the only interface between SQL and bitmaps | Lets W1 and W4 build in parallel from day one |
| D8 | DataFusion =55.1.0 (future ti-sql); arrow 59 major via lockfile, arrow-array/arrow-schema requirements 59.2 | Source: Zygomorphic Prawn dependency survey and Industrial Pike correction; local W0 lockfile resolves 59.3.0. No DataFusion dependency in contracts; future use disables default features and selects features explicitly |
| D9 | roaring 0.11.5 for TI | Source: Zygomorphic Prawn survey reports portable format, run containers and RoaringTreemap; existing MiniRoaring remains unchanged |
| D10 | Root ti-contracts dependency optional behind ti; root stays default workspace member | Source: W0 review and Industrial Pike assignment; normal root build retains its four direct dependencies |
| D11 | Future Parquet uses pure-Rust codecs only, no zstd | Source: Zygomorphic Prawn survey relayed by Industrial Pike; honor the C-binding restriction |
| D12 | Future blake3 enables pure | Source: Zygomorphic Prawn survey relayed by Industrial Pike; avoid cc for musl builds |
| D13 | Smoke-test pgwire SCRAM on aarch64-musl early | Source: Zygomorphic Prawn survey relayed by Industrial Pike reports ring dependency; validate portability before shipping |
| D14 | DuckDB as an out-of-process oracle (CLI or the `duckdb` Python package, 1.5.6), never a bundled Rust crate | Source: Zygomorphic Prawn survey relayed by Industrial Pike; avoid embedding the C++ engine. Amended 2026-10-06: the oracle and `tests/golden/gen_expected.py` use the Python package (installed on the host with `pip --user`, user-approved); still dev/test only, not a product dependency |
| D15 | cargo-zigbuild for musl release builds | Source: Zygomorphic Prawn survey relayed by Industrial Pike; tooling choice, not a contracts runtime dependency |
| D16 | `ShardSink::apply` acknowledges after a buffered WAL append; WAL is fsynced at least every 1 s (group commit), and always before `flush` advances or truncates it and on shutdown | Lead decision on spec/14's stricter per-apply fsync proposal. Bounds power-loss to ≈1 s, well inside spec/03's 60 s allowance, without paying an fsync per apply on the Pi. W2 may benchmark per-apply fsync and propose a change |
| D17 | Contract types derive `Debug, Clone, PartialEq` (plus `Eq` where all fields allow); `Predicate`/`FieldValue` also need them for proptest | Lead decision: spec/10 showed derives only on `ShardKey`, but W1's proptests and every lane's fixtures need to construct, print and compare these values. Applied in W0 part 2 |
| D18 | serde 1.0 with derive in ti-contracts | W0 part 2: ti.toml schema and frozen WAL value serialization; root default build still gates TI dependencies behind ti |
| D19 | toml 0.9 in ti-contracts | W0 part 2: parse the typed ti.toml schema, reject unknown keys and report key-qualified validation errors; pure Rust |
| D20 | `raw` (for both the DuckDB oracle and TI) is a normalizing view over the real signalk-parquet layout: `context, ts TIMESTAMP, path, value DOUBLE, value_str VARCHAR, source`, with object keys flattened to `path.key` | Lead decision after [design/signalk-formats.md](../design/signalk-formats.md) found string timestamps, no `$source` column and per-file value types. Oracles stay layout-independent, and the generator writes the real layout so the view is tested against it ([repo-fit §10](../repo-fit.md)) |
| D21 | Ordinary set fields are single-valued per bucket: exactly one row bit per column, the last preferred-source value. Rows are pairwise disjoint and union to presence; the Arrow type is Utf8. `$source` stays multi-valued `List<Utf8>`, and W4 rewrites `=` to `array_has` | Lead ruling at the contracts freeze: with Exact pushdown, a filter must agree with the projected value, because DataFusion doesn't re-check it. Mid-bucket changes are covered by `@starts` / edge counts. Enforced by `validate_ordinary_set_rows` |
| D22 | `ti-bench` workspace member: arrow 59.2 + parquet 59.2 (default-features off; flate2-zlib-rs, lz4_flex, snap, arrow) + chrono 0.4 (default-features off, clock) | Synthetic signalk-parquet generator for the correctness and performance sets (Zygomorphic Prawn, merged by lead in c65e515). Pure-Rust codecs only (D11/D27); not a dependency of the `lume` binary |
| D23 | proptest 1.x as a ti-core dev-dependency | W1: 10,000-case independent scalar/bitmap checks for signed BSI, predicate trees and D21 rewrites; coordinated with Prawn (D22 reserved for ti-bench), no new root runtime dependency |
| D24 | bincode 1.3 in ti-store | W2: length-prefixed little-endian fixed-integer serialization for WAL record payloads (spec 14 §79); pinned 1.3.3 in Cargo.lock. WAL payload encoding is an on-disk format versioned by the frozen WAL header (v1); any change of encoder or major version requires a header version bump plus a migration note |
| D25 | crc32fast 1.4+ in ti-store | W2: IEEE CRC32 frame checksum for WAL records over sequence LE and payload (spec 14 §78); pure-Rust fast table-based CRC32, already in shared lockfile |
| D26 | DataFusion =55.1.0, defaults disabled; sql/parquet/nested/datetime/math/string features; zstd-sys allowed by D27; async-trait 0.1, tokio 1 runtime/macros, futures 0.3, existing serde/serde_json for ti-sql | W4 read-only SQL, custom TableProvider/streaming executor, array_has rewrite, golden verification; root remains optional behind ti. DataFusion's additive transitive features enable zstd-sys; the lead approved this exception in D27. No vendoring or patches; bzip2/lzma C bindings remain excluded |

Reserved, and written by the owning lane at merge (agreed among the lanes on 2026-10-06):


| # | Decision | Why |
|---|---|---|
| D27 | **Amends D11.** Accept `zstd-sys` (C, built via `cc`), which DataFusion 55.1.0 forces in through `arrow-ipc`'s zstd feature even with `default-features = false` and only `sql` enabled. No other C codec crates are allowed: bzip2, lzma and liblzma must stay absent, checked with `cargo tree --features ti -i <crate>`. TI must not *enable* any further C codec itself. Musl builds compile it through cargo-zigbuild (D15). Add an early aarch64-musl `cargo zigbuild -p ti-sql` smoke test, like D13 | Lead ruling on W4's finding, verified independently by the lead in a scratch project. The alternative, vendoring 4 patched DataFusion crates, would mean re-patching on every quarterly DataFusion upgrade (spec/11 risk "DataFusion API churn"). **Spec deviation:** spec/10's PR rule says C bindings are allowed only for `croaring`. This decision makes a recorded exception for `zstd-sys` and asks the spec owner to confirm |
| D28 | `tungstenite` 0.24 in `ti-ingest` (blocking, plain `ws://` without TLS) | W3: Signal K WebSocket client and subscription management; keeps ingest off tokio and preserves low-priority thread budget. `wss://` requires rustls/ring and is out of scope for v1 (W7/W8 follow-up if remote Signal K needed) |
| D29 | `parquet` 59.2 in `ti-ingest` (`default-features = false`, `arrow`, `snap`, `flate2`, `zstd`) | W3: signalk-parquet raw tier backfill reader; pure-Rust codecs (`snap`, `flate2`), with `zstd` enabled reusing `zstd-sys` already accepted under D27 (no new C crates). C crates `bzip2-sys` and `lzma-sys` stay strictly forbidden |

| # | Decision | Why |
|---|---|---|
| D30 | A second **1 s high-resolution store** (`telemetry_hr`) beside the 10 s store, holding an allow-list of navigation (position, SOG, COG, heading: `@last`), wind (`@mean`, `@max`) and depth (`@min`). Retention is per store and configurable, defaulting to **90 days** on the Pi and on shore (`shore_retention`) | User decision, 2026-10-06. 10 s loses track shape (about 36 m between fixes at 7 kn), while gusts and shallowest depth are already kept by `@max`/`@min`. A 1 s bucket holds about one sample at Signal K's ~1 Hz, so it's full fidelity for those paths at about 0.15–0.5 GB per vessel over 90 days. No frozen-type change; needs a `ti.toml` contracts PR. Design: [design/hi-res-store.md](../design/hi-res-store.md). Scheduled after M3 |

| # | Decision | Why |
|---|---|---|
| D31 | `ti-geo` uses exact `h3o` 0.11.0 (BSD-3-Clause, explicit `std`/`geo` features) and `geo` 0.33.1 (MIT OR Apache-2.0, default features disabled), one shared geo/geo-types version | W6: pure-Rust H3 indexing at resolutions 5/7/9 and conservative `ContainmentMode::Covers` tiling. The h3o geometry API accepts geo polygons; the lead approved the direct matching geo dependency. No optional PROJ, triangulation, threading or native geometry bindings are enabled. Reuses D23 proptest for cover completeness checks |

The next free number is **D32**. Ask the lead before taking one. Every new runtime dependency needs a line here
(PR rule, [10-contracts](10-contracts.md)).

## Sources (from spec)

- Lume README — hybrid search, roaring-bitmap SKG, MCP server
- FeatureBase repository — archived Feb 21 2024, Apache-2.0
- signalk-parquet README — Parquet archive, DuckDB querying, History API provider
- Signal K History API — `/signalk/v2/api/history`, from / to / duration
- Signal K Resources API, provider methods — filtering left to provider plugins
- HALPI2 documentation — CM5, NVMe, CAN-FD NMEA 2000, RS-485 NMEA 0183
- HALPI2 operating system images — HaLOS, Marine image with Signal K, Grafana, InfluxDB, AvNav
- halos-marine-containers — HaLOS Marine container store definitions
