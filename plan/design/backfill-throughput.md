# Backfill throughput investigation

Compared pre-D30 `f6c47b4` with `c130bc1`, in the same container with the same harness. The historical 219,779 -> ~140k -> 174,769 host throughput decline is **not reproduced** on this subset. Industrial Pike confirmed by mail that the slower host runs overlapped 2–3 builds while the original baseline ran on a quieter host; contention is a plausible explanation, not a measured causal proof. A quiet host rerun remains necessary for absolute throughput.

## Method

One vessel (`367000000`), day partitions 60–66 inclusive: 175 files, 1,459,559 raw points and two sealed shards. Read the shared correctness data directly; each run creates a fresh output store. Default single store, width 10, opt-in last, no ti.toml or additional stores. Same metadata bootstrap, sorted file list, 50,000-record apply chunks and 256-file flush cadence. Catalog bootstrap and final sealing are outside the backfill timer. This measures the Signal K pipeline rather than generic Parquet mapping or document import.

Release builds use identical overrides: `CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 CARGO_PROFILE_RELEASE_LTO=false CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16`. These differ from the historical host fat-LTO/cu=1 settings, so compare within this experiment only. Timers add per-point overhead equally to both pipelines. `bucketer` includes normalize/classify/apply; its exclusive residual subtracts those nested stages. Decode measures batch iteration, with batch-to-raw extraction separately; reader construction and misc driver overhead remain outside named stages.

Initial three runs per version are retained in [raw results](../../bench/backfill-profile-results.json). Follow-up order was A/B/A/B with no concurrent local builds. Ratios below are current / pre-D30 elapsed durations (smaller is faster):

| Stage | Pair 1 | Pair 2 |
|---|---:|---:|
| hash | 0.980 | 1.028 |
| decode | 0.882 | 1.132 |
| extract | 0.972 | 1.069 |
| normalize | 1.295 | 1.236 |
| classify | 1.533 | 1.437 |
| apply | 0.985 | 0.993 |
| flush | 0.954 | 0.798 |
| bucketer | 0.998 | 1.013 |
| bucketer excluding normalize/classify/apply | 0.980 | 1.007 |
| total | 0.986 | 0.994 |

Apply and bucketing dominate; normalization and classification together are about 3% of wall time. A persistent multi-store classifier retains more path state than the earlier per-file classifier, which may explain classification overhead, but that attribution is inferred from source and not independently isolated. No broad store rewrite is justified by these measurements. The vessel-week cannot exclude effects that appear only over the full 95.9-million-value set.

## Contained change

`MultiStoreBucketer::backfill_raw_points` previously cloned the complete raw point (context/path/source/JSON) before a normalizer that only borrows its input. Added `normalize_point_ref` and use it in this loop; the existing owned API delegates to the same implementation. Filtering, flattening, ordering and aggregation are unchanged.

Two uncontaminated candidate runs recorded normalization at 0.1772 and 0.1713 s, versus 0.2342 and 0.2425 s for the balanced current runs (0.757× and 0.706×). These durations establish a repeatable stage-level reduction, **not an overall throughput improvement**: first candidate wall time was affected by I/O variation. A second candidate attempt overlapped compilation and is deliberately excluded from results. Final candidate total/current pair-2 was 0.996×, below useful significance.

All initial six, added four and retained two candidate runs have exactly the same shard seals:

| Shard | BLAKE3 seal |
|---|---|
| 0:296 | `b4dbe01fdcda9739dd105a0ff87dd276cbacc935add2a6a1a9653b316fb15b90` |
| 0:297 | `ea21267dde285dd989f6e7199e2eb296828349287cca1295ab0cd75716f22ff2` |

This proves byte-identical content-addressed seals on this dataset, not a full-year golden regression.

## Verification

`cargo test -p ti-ingest`: 43 passed, 3 intentionally ignored (two external-data gates and one CLI-binary gate). This includes backfill replay/idempotence, multi-store fanout, notifications and bounded-window tests. Baseline instrumentation patch passed `git apply --reverse --check` against the instrumented old worktree before removal. Default `cargo build` also passed, with an existing unused-assignment warning at src/main.rs:1851 (outside this change). Host rustfmt/strict clippy remain to be run by the lead.

## Reproduce

Apply [before patch](../../bench/backfill-before.patch) to a clean worktree at f6c47b4. The patch contains the same harness plus instrumentation of that snapshot's actual directory pipeline. Current instrumentation is gated by `backfill-profile`; disabled optimized builds compile timers away.

Build each snapshot sequentially using the release overrides above:

```sh
cargo build --release -p ti-ingest --example profile_backfill --features backfill-profile
```

Copy each executable before rebuilding the next snapshot; run A/B/A/B against the same raw vessel directory, specifying a different absent output directory each time:

```sh
profile_backfill RAW_VESSEL_DIRECTORY FRESH_STORE_DIRECTORY 7
```

Compare every returned hash, then delete generated stores. Do not overlap builds and timing runs. On the host, also repeat the production `backfill_store` full-set benchmark with fat LTO/cu=1 and no other builds. Keep shared Cargo target under 8 GB and clean after handoff.
