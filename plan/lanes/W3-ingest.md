# W3 — `ti-ingest` (weeks 2–5, gates M2)

Depends on: W0 contracts. Writes through `ShardSink` (mock until W2 lands).
Spec: [06-ingest](../spec/06-ingest.md), [05-data-model](../spec/05-data-model.md) (aggregates, derived fields, sources), [02-pilot-vessel](../spec/02-pilot-vessel.md).

## Owns

Signal K WebSocket client, delta decode, normalize, classify, bucketer,
watermark, unit/scale registry, Parquet backfill (and InfluxDB backfill for HaLOS).

## Tasks

### Sources
- [ ] WebSocket client: `ws://<host>:3000/signalk/v1/stream?subscribe=none`, then the two subscribe messages (`*` @ 1000 ms instant; `notifications.*` instant). Reconnect with backoff.
- [ ] Device-token auth read from `ti.toml` (token acquisition itself is W7 / plugin).
- [ ] Meta fetch on connect + hourly: `GET /signalk/v1/api/vessels/self` → registry (units, displayName, description, zones).
- [ ] `POST /ti/ingest` NDJSON path feeds the same decoder (route owned by W7).
- [ ] Delta recorder: capture a live session to a replayable log (needed for the M2 gate and PV-1 dataset).

### Backfill
- [ ] signalk-parquet reader (arrow-rs `parquet`): `received_timestamp` / `signalk_timestamp`, `path`, `value`, `value_*`. Skip files whose content hash is in the manifest.
- [ ] InfluxDB reader for HaLOS Marine: export by measurement + time range → same tuples. **PV-1 needs this** (history lives in InfluxDB).
- [ ] Backfill always uses clear-and-rewrite → idempotent.
- [ ] Runs at idle priority, pauses under load (signal from W7).

### Pipeline stages
- [ ] Decode → `(context, path, $source, timestamp, value)`; fall back to receive time if timestamp missing or skewed > 5 min.
- [ ] Normalize: flatten objects (`navigation.position` → `.latitude`, `.longitude` + H3 cells via W6 helper; `{value:{a,b}}` → `path.a`, `path.b`); apply deny list.
- [ ] Classify: numbers → `bsi`, strings/bools/enums → `set`; sticky first classification; type change logs and creates `path#2`.
- [ ] Unit/scale registry from the `ti.toml` table + meta; unknown numeric → scale 3 + registry-miss log.
- [ ] Bucketer: per-`(vessel, path)` accumulators (`count`, `sum`, `min`, `max`, `last`, sources seen, edge/transition counters).
- [ ] Aggregate profiles: default (`@mean/@min/@max`), `slow` (`@last`, auto-detected when median interval ≥ W), opt-in `@last`/`@count`.
- [ ] Derived fields from `ti.toml` rules: transitions (`@starts`), rising-edge counts, notifications (`set` over states + `count` of raises).
- [ ] Source handling: preferred-source value; every source in bucket → `path$source` set.
- [ ] Watermark close: max event time − 30 s lateness → emit `BucketRecord`s; late-but-unsealed → `rewrite: true`.
- [ ] Vessel ordinal assignment by context URN; `vessels.self` → its URN.
- [ ] Field cap + warning surfaced in `ti_status` (path-explosion risk).

## First deliverable
Replay a recorded delta log into `BucketRecord`s identical to the oracle bucketing.

## Gate (M2)
- [ ] 24 h recorded delta log → `BucketRecord`s equal to oracle bucketing
- [ ] Parquet backfill of the correctness set run twice → identical manifest hashes
- [ ] Pi 5 sustains 20,000 values/s for 1 h within ≤ 25 % of one core and ≤ 400 MB RSS

## Open
- No 24 h real delta log exists yet — generate one from `ti-bench gen` or record from any available Signal K demo server until PV-1 is live.
- Nanni engine gateway N2K format unconfirmed (PV-1 assumption list).
