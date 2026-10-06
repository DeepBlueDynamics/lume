# W2 — `ti-store` (weeks 2–4, gates M1)

Depends on: W0 contracts. Row types come from W1 behind the contract.
Spec: [06-ingest](../spec/06-ingest.md) (stages 6–7, late data, on-disk layout), [05-data-model](../spec/05-data-model.md) (catalogs).

## Owns

WAL, shard files, manifest, seal, versioning, repair, mmap/read cache. Implements
`ShardSink` (write side) and `ShardSource` (read side) over a real store.

## Tasks

### On-disk layout
- [ ] `ti/` root: `ti.toml`, `catalog/vessels.json`, `catalog/paths.json`, `manifest.json`, `wal/<vessel_ord>.wal`, `shards/<vessel_ord>/<shard_no>/v<version>/<field_id>.rbm`, `docs/`.
- [ ] Catalog read/write for `vessels`, `paths`, `shards` (atomic replace via temp + rename).

### WAL
- [ ] Per-vessel append-only log: length-prefixed bincode records + CRC32.
- [ ] Replay on startup; stop cleanly at the first torn/CRC-bad record.
- [ ] Truncate to the flush point after a successful flush.
- [ ] Flush-on-shutdown hook (HALPI shutdown signal — wired by W7).

### Open shard
- [ ] `ShardSink::apply` → in-memory roaring rows for the current window, dirty tracking.
- [ ] Flush dirty rows every 60 s or 50k records.
- [ ] Re-open of a closed-but-unsealed bucket: clear-and-rewrite for that field.

### Seal + versioning
- [ ] Seal when `now − shard_end > 1 h` (and on `lume ti seal`).
- [ ] Immutable per-field `.rbm` in roaring portable format; BLAKE3 over field files; manifest update.
- [ ] Deterministic output: byte-identical files and hash from identical input (fixed field order, no timestamps in files).
- [ ] `repair` queue: late data after seal rewrites the shard into `v<n+1>` with a new hash.

### Read path
- [ ] `ShardSource::shards` prune by vessel + bucket range from the manifest.
- [ ] mmap read of sealed `.rbm` files; read cache. Only `unsafe` allowed in the workspace lives here, with a justifying comment.
- [ ] Shard-at-a-time streaming to keep Pi RSS bounded (risk: memory pressure).
- [ ] Retention: drop shards older than 2 y locally (shore keeps them).

## First deliverable
Crash-recovery test: kill -9 during flush, restart, no lost or duplicated record.

## Gate (M1, shared with W1)
- [ ] 1,000 kill -9 runs during flush, zero lost or duplicated records after replay
- [ ] Seal produces byte-identical files and hashes from identical input

## Open
- Crash test on Windows dev boxes vs Linux CI only (kill -9 semantics). Probably Linux-only job.
