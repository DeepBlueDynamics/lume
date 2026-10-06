# W8 — `ti-sync` + `ti-bench` (weeks 9–12, gates M6)

Depends on: W0 contracts, W2 store.
Spec: [09-install-fleet](../spec/09-install-fleet.md) (fleet topology), [13-benchmarks](../spec/13-benchmarks.md).

## Owns

Manifest diff, resumable shard shipping, WAL-tail ship; benchmark harness and reports.

## Tasks — `ti-sync`
- [ ] Manifest diff local vs shore → missing shard versions.
- [ ] Resumable chunked upload of `{vessel}/{shard}/{version}.tar` (HTTP to `/ti/shards/...` or `s3://`).
- [ ] Open-shard WAL tail segments every 5 min when online (shore lag ≤ 5 min).
- [ ] Shore import: mount all vessels' shards; re-map vessel ordinals by URN.
- [ ] Respect link state + data budget (PV-1: Starlink or Viasat, TBD).
- [ ] Idle priority; pause under load.
- [ ] Two-node test harness with a lossy link (20 % chunk drop, 30-min outage).

## Tasks — `ti-bench`
- [ ] Harness over Q1–Q8 with p50/p95/p99, cold + warm cache.
- [ ] DuckDB baseline runs on the same data.
- [ ] Index bytes per vessel-year vs Parquet bytes; ingest values/s, CPU %, RSS on Pi; sync bytes/day/vessel.
- [ ] Results → `bench/results/<date>-<git sha>.json` + markdown summary; CI fails on > 15 % p95 regression.
- [ ] Single-box contention test on HALPI2: OpenCPN pan/zoom + full-rate ingest + Q4/Q8 back-to-back 10 min; measure SK delta latency, dropped deltas, OpenCPN p95 frame time.
- [ ] Replace [03-single-box-budget](../spec/03-single-box-budget.md) memory estimates with PV-1 measurements.

## First deliverable
Two-node sync test with a link that drops 20 % of chunks.

## Gate (M6, with integrator)
- [ ] Two-node sync converges with 20 % chunk loss and a 30-minute outage
- [ ] Shore answers fleet queries across 50 synthetic vessels
- [ ] Benchmark report against every target; go / no-go in the decisions log

## Open
- Shore storage: local NVMe vs object storage + read cache (open question).
- How to measure OpenCPN frame time on the Pi.
- CI hardware for the "> 15 % p95 regression" check — Pi benchmarks can't run on GitHub-hosted runners.
