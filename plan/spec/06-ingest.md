# 6. Ingest pipeline

Spec pp. 11–13. Owner: [W3](../lanes/W3-ingest.md) (stages 1–5), [W2](../lanes/W2-store.md) (stages 6–7).

Live deltas and Parquet backfill feed the same bucketer. Buckets close on a
watermark and land in shards through a WAL; a shard seals once it is entirely
in the past.

## Sources

1. **Live.** Rust WebSocket client to `ws://<host>:3000/signalk/v1/stream?subscribe=none`, then sends
   - `{"context":"vessels.self","subscribe":[{"path":"*","period":1000,"policy":"instant"}]}`
   - `{"context":"vessels.self","subscribe":[{"path":"notifications.*","policy":"instant"}]}`

   Auth: Signal K device token from an access request, stored in `ti.toml`.
2. **Backfill.** Read signalk-parquet files with arrow-rs `parquet`. Map `received_timestamp` / `signalk_timestamp`, `path`, `value`, `value_*` columns. Skip files whose content hash is already in the manifest. On HaLOS Marine, history is in bundled InfluxDB: a second backfill reader exports by measurement and time range and maps to the same tuples.
3. **Meta.** On connect and hourly, `GET /signalk/v1/api/vessels/self` → `meta` (units, displayName, description, zones) into the path registry.
4. **Documents.** Poll `GET /signalk/v2/api/resources/notes` every 60 s; also logbook plugin entries when installed, and notification messages from the live stream. Each becomes a Lume document with a time range.

## Stages

1. **Decode.** Delta → `(context, path, $source, timestamp, value)` tuples, using the update's `timestamp`, falling back to receive time if missing or skewed > 5 min.
2. **Normalize.** Flatten objects: `navigation.position` → `.latitude`, `.longitude` plus H3 cells; `{value: {a, b}}` → `path.a`, `path.b`. Drop deny-listed paths (default: `*.ais.*` raw sentences and `design.*`).
3. **Classify.** Numbers → `bsi`; strings, booleans, enums → `set`. Objects flattened and recursed. First classification is sticky; a later type change logs and creates a `path#2` field.
4. **Bucket.** Per-`(vessel, path)` accumulators keyed by bucket index: `count`, `sum`, `min`, `max`, `last`, sources seen, edge/transition counters.
5. **Close.** Bucket closes when watermark (max event time − 30 s lateness) passes its end. Emits `BucketRecord`s.
6. **Write.** Append to per-vessel WAL (length-prefixed bincode + CRC32), apply to in-memory shard rows. Flush dirty rows every 60 s or 50k records, then truncate WAL to the flush point.
7. **Seal.** Once `now − shard_end > 1 h`. Writes immutable per-field `.rbm` files (roaring portable format), BLAKE3 hash over the field files, updates the manifest.

## Late and re-ingested data

- Data after bucket close but before seal re-opens that bucket for that field — clear-and-rewrite (BSI rewrite clears every row's bit for the column first).
- Late data after seal → `repair` queue. Repair rewrites the shard into a new version with a new hash; shore picks it up by hash.
- Backfill always uses clear-and-rewrite, so it is idempotent.

## Throughput targets

- **Pi 5:** ≥ 20,000 values/s sustained live ingest, ≤ 25 % of one core, ≤ 400 MB RSS.
- **Shore host, backfill:** ≥ 1,000,000 values/s.

## On-disk layout

```
ti/
  ti.toml
  catalog/vessels.json  catalog/paths.json
  manifest.json                        # shards, versions, hashes
  wal/<vessel_ord>.wal
  shards/<vessel_ord>/<shard_no>/v<version>/<field_id>.rbm
  docs/                                # Lume index for notes, logs, notifications
```
