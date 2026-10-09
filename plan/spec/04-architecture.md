# 4. System architecture

Spec p. 7.

Four layers in one Rust binary: ingest, store, query, interfaces. Runs on every
vessel; sealed shards are the only thing that moves to shore.

```
 Signal K server            signalk-parquet              Documents
 (WS deltas, meta,          (raw samples, source         (notes, logbook,
  notes, notifications)      of truth)                    manuals in Lume BM25)
        │ live                    │ backfill                   │
        ▼                         ▼                            │
 ┌─ ti-ingest (per vessel) ───────────────────────────────┐    │
 │ Decode & normalize ──► Bucketer ──► WAL & shard writer │    │
 │ (flatten, H3 cells,    (count,sum,   (CRC-framed WAL;  │    │
 │  classify type)         min,max,last; flush every 60 s)│    │
 │                         30 s lateness watermark)       │    │
 └──────┬───────────────────────────────────────┬─────────┘    │
        │ path registry                         │ BucketRecords│
        ▼                                       ▼              │
 ┌─ ti-store ─────────────────────────────────────────────┐    │
 │ Catalogs           Sealed shards  ◄─seal─  Open shard  │    │
 │ (vessels, paths,   (immutable,             (roaring    │    │
 │  shards; units,     BLAKE3-hashed;          rows in    │    │
 │  fixed-pt scale)    unit shipped to shore)  memory)    │    │
 └──────┬──────────────────┬─────────────────────┬────────┘    │
        │ schema           │                     │             │
        ▼                  ▼                     ▼             ▼
 ┌─ ti-sql on DataFusion ─────────────────────────────────────────┐
 │ TableProvider ──► [ BITMAP EVALUATOR ] ◄── ti-text / ti-geo    │
 │ (classify filters,  per-shard AND/OR/BSI;   match() as bucket  │
 │  emit Predicate IR) materializes Arrow rows bitmaps; H3 cover  │
 └──────────────▲──────────────────▲────────────────────▲─────────┘
                │                  │                    │
         MCP tools             HTTP                CLI & Signal K plugin
   ti_sql, ti_resolve,   /ti/sql as Arrow,      lume ti sql, verify, sync
        ti_explain          JSON, CSV
```

The bitmap evaluator is the only place `Predicate` IR meets roaring rows, which
is why it is the contract every lane builds against ([10-contracts](10-contracts.md)).
Raw samples bypass the index entirely through the federated `raw` table over Parquet.

Layer → lane map:

| Layer | Lanes |
|---|---|
| ti-ingest | [W3](../lanes/W3-ingest.md) |
| ti-store | [W2](../lanes/W2-store.md), with row types from [W1](../lanes/W1-core.md) |
| ti-sql | [W4](../lanes/W4-sql.md), [W5](../lanes/W5-text.md), [W6](../lanes/W6-geo.md) |
| Interfaces | [W7](../lanes/W7-serve.md) |
| Fleet / shore | [W8](../lanes/W8-sync-bench.md) |
