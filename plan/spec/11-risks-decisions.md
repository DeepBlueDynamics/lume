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

New decisions go below as D8+. Every new runtime dependency needs a line here
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
