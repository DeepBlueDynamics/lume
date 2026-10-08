# A15: q6-004 document range join

The Pi benchmark reported 308.53 ms p95 against the 200 ms edge target.
That is lead-reported; the host was about 65 ms. Native release and Pi acceptance
remain separate from container debug timing.

The initial Rust 1.96 debug profile over dist/pi-bench/store found that the
existing CollectLeft hash join already builds from the small matching docs input.
It nevertheless materializes 257,588 telemetry rows before the vessel/time range
residual selects 193 joined rows. Warm execution was 605–643 ms, with planning
about 17–24 ms. The original seven-run total p50 was 631.18 ms and p95 667.03 ms.
Artifact: .lanes/data/query-cache/q6-004-before.json.

## Pruning

A physical optimizer recognizes inner joins with a direct telemetry input,
an equality on vessel/entity, and conjunctive timestamp lower and upper bounds
against a materialized memory input. It derives an inclusive, per-vessel union
of bucket ranges, merges overlaps, prunes impossible shards, and intersects the
range bitmap before telemetry value reconstruction. The original HashJoinExec
and filter remain unchanged: duplicate document matches, scores, strict bounds,
and all residual conditions still run normally.

This does not use document text-cover bitmaps: those are half-open, whereas
q6-004 explicitly includes ts_end. Off-grid starts round up to the first bucket;
ends round down. Strict comparisons conservatively retain their endpoints.

The memory-input row cap is 4,096, checked before building ranges. Any NULL range
disables pruning entirely. Unrecognized joins, outer joins, OR bounds, one-sided
bounds, computed telemetry inputs, and non-memory inputs remain ordinary plans.
Memory projections preserve column indices. Residual docs filters and fetch
limits are ignored for bounds, producing a safe superset without evaluating
arbitrary predicates during planning.

The diagnostic SqlSession constructor can disable only document range pruning;
bitmap aggregates and all other optimizer rules remain enabled in both A/B sides.

## Reproduction

First copy the fixture into the lane's `.test-tmp/pi-bench-store`; opening a legacy
store can migrate its document persistence even for a query. The initial profile
did migrate the shared fixture; the lead confirmed that migration was intended
and authorized leaving it in place. Subsequent profiling uses only the owned copy.

From the lane, on the native host with Rust 1.96, set these environment variables
(PowerShell: use $env:NAME). Use a fresh output filename:

```sh
CARGO_INCREMENTAL=0
TI_Q6_STORE=<repo>/.lanes/w4/.test-tmp/pi-bench-store
TI_Q6_OUTPUT=<repo>/.lanes/data/query-cache/q6-native-paired.json
TI_Q6_RUNS=7
# Optional: all 26 benchmark queries, including q6-004
TI_Q6_ALL=1
cargo +1.96.0 test --locked --release --features ti --test ti_q6_profile \
  q6_document_range_join_profile -- --ignored --nocapture
```

In sh, export the variables before running cargo. Set TMPDIR and
CARGO_TARGET_TMPDIR to the lane scratch directory as usual. Both sides share the
same immutable source and lexical document index. Each query/mode starts with
the decoded bitmap cache cleared, followed by seven warm runs. OS cache is
uncontrolled. Canonical complete rows must match both sides and every warm run.
The JSON includes phase times, physical plans, row counts, materialization and
cache counters, and complete answers; raw JSON stays in .lanes/data.

## Validation

Targeted pushed/residual integration tests pass, covering inclusive and strict
endpoints, reversed operands/input order, overlapping docs, two vessels, an
off-grid start and the 65,536-bucket shard boundary. NULL ends, 4,097-row inputs,
OR/outer/arithmetic/one-sided shapes preserve ordinary execution and rows.

## Same-process lane A/B

Rust 1.96 debug, Pi fixture, 256 MiB cache, seven warm iterations:

| q6-004 metric | Pruning disabled | Pruning enabled |
|---|---:|---:|
| Warm total p50 | 640.48 ms | 35.24 ms |
| Warm total p95 | 660.40 ms | 43.13 ms |
| Cold total | 14,287.28 ms | 14,282.26 ms |
| Materialized telemetry rows | 257,588 | 193 |
| Joined rows | 193 | 193 |

Warm p50 improves 18.17 times. Every complete row matches across both variants
and all warm runs, and the earlier unchanged-code profile's 193 rows match too.
The original inclusive hash-join filter remains in both plans. Cold decoding
still visits the same 13 shards and is unchanged; this is a warm join improvement.
Artifact: .lanes/data/query-cache/q6-004-paired.json (raw reply not committed).

The owned-copy 26-query debug check passed complete row equality for every query
and all seven warm iterations (296.80 s). q6-004 materialization is 257,588 → 193;
its warm p50 is 640.40 → 34.15 ms and p95 720.60 → 43.76 ms.
Raw output: `.lanes/w4/.test-tmp/q6-26-owned.json`.

| Query | Disabled p50 ms | Enabled p50 ms | >1.15× and >2 ms |
|---|---:|---:|---|
| q1-001 | 4.62 | 5.16 | — |
| q1-002 | 5.44 | 4.26 | — |
| q1-004 | 5.35 | 5.17 | — |
| q2-002 | 8.05 | 7.05 | — |
| q2-003 | 10.55 | 10.01 | — |
| q2-005 | 15.71 | 15.42 | — |
| q3-001 | 9.39 | 8.44 | — |
| q3-002 | 11.14 | 11.05 | — |
| q3-004 | 8.22 | 6.10 | — |
| q4-001 | 18.34 | 18.66 | — |
| q4-002 | 29.95 | 26.24 | — |
| q4-005 | 22.36 | 22.74 | — |
| q5-001 | 13.97 | 13.49 | — |
| q5-002 | 14.81 | 15.07 | — |
| q6-001 | 27.11 | 33.41 | FLAG |
| q6-002 | 28.86 | 33.20 | FLAG |
| q6-003 | 33.80 | 33.48 | — |
| q6-004 | 640.40 | 34.15 | — |
| q6-005 | 29.22 | 30.84 | — |
| q7-001 | 18.11 | 18.68 | — |
| q7-002 | 15.37 | 16.36 | — |
| q8-001 | 110.33 | 107.13 | — |
| q8-002 | 77.47 | 77.17 | — |
| q8-006 | 80.43 | 80.34 | — |
| q7-006 | 25.83 | 21.02 | — |
| q6-006 | 30.49 | 29.71 | — |

Q6-001 and Q6-002 have identical physical plan text before/after and do not use
the pruning. Their timing flags require repeat/native checks; they are not
silently accepted as noise. A 21-warm-run repeat on the owned fixture found
Q6-001 27.86 → 29.43 ms (+1.57 ms) and Q6-002 31.60 → 29.96 ms; neither
triggers the rule, and all rows match. Artifact: `.test-tmp/q6-repeat-owned.json`.
Full ti-sql tests, scoped fmt, and the locked root TI test command with the
approved atomic-reader skip passed. Whole-package and all eight TI crates'
strict clippy (`--all-targets -- -D warnings`) passed with Rust 1.96. Both CI
formatting checks passed. The default `cargo +1.96.0 build --locked` passed.
Target usage remained below 8 GiB (5.9 GiB at the final build). Native release p95,
the CI regression rule (greater than 1.15 times AND more than 2 ms absolute),
and the Pi target are pending lead measurements. No native PASS is inferred
from these debug timings.
