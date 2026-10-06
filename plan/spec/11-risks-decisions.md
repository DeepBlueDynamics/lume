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

- [ ] Default bucket width: 10 s, or 1 s with 1-min rollups? (1 s is 10× the index size)
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
| D14 | DuckDB CLI oracle rather than bundled Rust crate | Source: Zygomorphic Prawn survey relayed by Industrial Pike; avoid embedding the C++ engine |
| D15 | cargo-zigbuild for musl release builds | Source: Zygomorphic Prawn survey relayed by Industrial Pike; tooling choice, not a contracts runtime dependency |
| D16 | `ShardSink::apply` acknowledges after a buffered WAL append; WAL is fsynced at least every 1 s (group commit), and always before `flush` advances or truncates it and on shutdown | Lead decision on spec/14's stricter per-apply fsync proposal. Bounds power-loss to ≈1 s, well inside spec/03's 60 s allowance, without paying an fsync per apply on the Pi. W2 may benchmark per-apply fsync and propose a change |
| D17 | Contract types derive `Debug, Clone, PartialEq` (plus `Eq` where all fields allow); `Predicate`/`FieldValue` also need them for proptest | Lead decision: spec/10 showed derives only on `ShardKey`, but W1's proptests and every lane's fixtures need to construct, print and compare these values. Applied in W0 part 2 |
| D18 | serde 1.0 with derive in ti-contracts | W0 part 2: ti.toml schema and frozen WAL value serialization; root default build still gates TI dependencies behind ti |
| D19 | toml 0.9 in ti-contracts | W0 part 2: parse the typed ti.toml schema, reject unknown keys and report key-qualified validation errors; pure Rust |
| D20 | `raw` (for both the DuckDB oracle and TI) is a normalizing view over the real signalk-parquet layout: `context, ts TIMESTAMP, path, value DOUBLE, value_str VARCHAR, source`, with object keys flattened to `path.key` | Lead decision after [design/signalk-formats.md](../design/signalk-formats.md) found string timestamps, no `$source` column and per-file value types. Oracles stay layout-independent, and the generator writes the real layout so the view is tested against it ([repo-fit §10](../repo-fit.md)) |
| D21 | Ordinary set fields are single-valued per bucket: exactly one row bit per column, the last preferred-source value. Rows are pairwise disjoint and union to presence; the Arrow type is Utf8. `$source` stays multi-valued `List<Utf8>`, and W4 rewrites `=` to `array_has` | Lead ruling at the contracts freeze: with Exact pushdown, a filter must agree with the projected value, because DataFusion doesn't re-check it. Mid-bucket changes are covered by `@starts` / edge counts. Enforced by `validate_ordinary_set_rows` |
| D23 | proptest 1.x as a ti-core dev-dependency | W1: 10,000-case independent scalar/bitmap checks for signed BSI, predicate trees and D21 rewrites; coordinated with Prawn (D22 reserved for ti-bench), no new root runtime dependency |
| D26 | DataFusion =55.1.0, defaults disabled; sql/parquet/nested/datetime/math/string features; zstd-sys allowed by D27; async-trait 0.1, tokio 1 runtime/macros, futures 0.3, existing serde/serde_json for ti-sql | W4 read-only SQL, custom TableProvider/streaming executor, array_has rewrite, golden verification; root remains optional behind ti. DataFusion's additive transitive features enable zstd-sys; the lead approved this exception in D27. No vendoring or patches; bzip2/lzma C bindings remain excluded |

Reserved, and written by the owning lane at merge (agreed among the lanes on 2026-10-06):

- D22: Zygomorphic Prawn, `ti-bench` generator crate (arrow/parquet 59.x pure-Rust codecs, chrono; dev/optional)
- D24, D25: Romantic Pike, W2 `bincode` and `crc32fast`

The next free number is **D26**. Ask the lead before taking one. Every new runtime dependency needs a line here
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
