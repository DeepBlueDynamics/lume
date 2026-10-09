# 3. Single-box budget

Spec p. 6.

On PV-1, Signal K, OpenCPN and Lume TI are believed to share one HALPI.
Navigation always wins: Lume TI is a low-priority guest. A heavy query can slow
itself but never OpenCPN or Signal K.

| Process | Priority | Memory (planning est., 8 GB CM5) | CPU |
|---|---|---|---|
| OpenCPN | highest, untouched | 0.5–1 GB | bursts on chart redraw |
| Signal K server | high, untouched | 0.2–0.4 GB | light |
| InfluxDB + Grafana (HaLOS Marine only) | normal | 0.3–0.6 GB | light |
| Lume TI ingest | low (`CPUWeight=50`, nice 10) | ≤ 400 MB RSS | ≤ 25 % of one core |
| Lume TI queries | lowest | 1 GB pool (512 MB on 4 GB boards) | 2 of 4 cores |
| Lume TI seal, backfill, sync | idle (`ionice -c3`) | within ingest budget | paused under load |

## Rules enforced in `ti-serve` and the container/systemd unit

- **Hard cap.** `MemoryMax=1.5G` on the whole Lume TI unit. A query that needs more fails with a hint to narrow `ts`; it never swaps the box.
- **Admission.** DataFusion `target_partitions = 2` on the boat. One heavy query at a time, others queue; default timeout 30 s.
- **Thermal.** HALPI2 is passively cooled; host daemon reports SoC temp. Above 75 °C, background jobs pause and query threads drop to 1.
- **Shutdown.** On the HALPI shutdown signal (power-button double-click or host daemon), flush the WAL first. A power cut costs at most 60 s of buckets, replayable from InfluxDB or Parquet.
- **Contention test.** Gates M6 (see [13-benchmarks](13-benchmarks.md)).

Memory figures are estimates, to be replaced with PV-1 measurements in M6.

Owners: [W7](../lanes/W7-serve.md) (admission, caps, thermal, shutdown, unit files),
[W2](../lanes/W2-store.md) (WAL flush on shutdown), [W8](../lanes/W8-sync-bench.md) (contention test).
