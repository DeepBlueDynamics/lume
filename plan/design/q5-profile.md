# Q5-002 residual interval profile

Before cebf5ea, Q5-002 was the native cache-enabled outlier: warm p50 about 300 ms, while Q5-001
is 6.76 ms. Source inspection and a paired runtime experiment identify the cause:
IS DISTINCT FROM was not supported by the bitmap classifier, leaving a FilterExec
above telemetry. intervals() then materializes vessel/timestamp/state rows instead
of extracting compressed bitmap runs.

The profile-only investigation (446d578) changed no golden SQL, oracle,
classifier or store format. The subsequent exact classifier fix is described
below, followed by its lead-reported native acceptance.

## Paired diagnostic

Original predicate:

```sql
"propulsion.port.motorPower" > 500
AND "propulsion.main.state" IS DISTINCT FROM 'started'
```

Logically equivalent diagnostic predicate:

```sql
"propulsion.port.motorPower" > 500
AND ("propulsion.main.state" IS NULL
     OR "propulsion.main.state" <> 'started')
```

Both use intervals(min_len => '40s'). The NULL arm is necessary: a missing diesel
state is distinct from 'started'. Replacing the operator with <> alone would
change answers.

The profile records logical planning, physical planning, execution, materialized
rows, physical plans and complete canonical answers. Conversion/comparison of
answers is outside the timers. It uses a standard TaskContext for the prepared
read-only plan; there are no HTTP row caps or document predicates. Each side
starts with the decoded application cache cleared, then runs seven warm queries.
OS cache is uncontrolled. All cold/warm answers are checked exactly.

## Observed lane result

Rust 1.99 debug, store-full, same process, 256 MiB cache. Bind-mount cold timings
are unsuitable as native absolute numbers; compare sides in this environment.

| Metric | IS DISTINCT FROM | NULL-preserving bitmap equivalent |
|---|---:|---:|
| Warm total p50 | 770.15 ms | 54.03 ms |
| Warm total p95 | 782.61 ms | 56.71 ms |
| Warm logical planning median | 1.08 ms | 1.25 ms |
| Warm physical planning median | 4.58 ms | 3.36 ms |
| Warm execution median | 764.35 ms | 48.65 ms |
| Materialized telemetry rows | 1,545,371 | 0 |
| Result intervals | 134 | 134 |
| Cache evictions | 0 | 0 |
| Cold total | 70,205.86 ms | 66,543.09 ms |

Warm p50 improves 14.25 times. Separate phase medians need not sum to the total
median. The original physical plan explicitly reports materialization fallback
and Unsupported for the state predicate. The equivalent plan reports bitmap runs
with every filter Exact. Execution dominates the original cost; retained-cache
capacity does not explain it. At the profile-only stage, native release timing and the 150 ms p95 target
were pending; post-fix native acceptance is recorded below.

Artifact: .lanes/data/query-cache/q5-profile-lane.json (not committed).

## Tests and reproduction

The ordinary test creates started/stopped/missing-state buckets and low motor
power, including an interval across a shard boundary. It checks identical results,
fallback versus bitmap physical plans, zero bitmap materialization, and that
removing the NULL arm loses valid intervals. Observed: this test passed, and the
separate seven-run store-full profile passed with exact equality.

```sh
CARGO_INCREMENTAL=0 cargo test -p ti-sql --test q5_profile
CARGO_INCREMENTAL=0 TI_Q5_STORE=<read-only-store-full> \
TI_Q5_OUTPUT=<fresh-output.json> TI_Q5_RUNS=7 \
cargo test -p ti-sql --release --test q5_profile \
boat_store_q5_paired_profile -- --ignored --nocapture
```

On PowerShell set the same environment variables before cargo. A fresh output
filename is required. The default measurement uses seven runs, with an explicit
range of 1–100. At profile-only commit 446d578, specific-file rustfmt passed and strict clippy
was not run locally. The later classifier implementation's clippy passed below.

## Proposed optimization before ad627f0

Translate scalar column/literal IS DISTINCT FROM and IS NOT DISTINCT FROM into
the existing bitmap IR, preserving their two-valued NULL behavior under NOT as
well as AND/OR. Keep arrays, nonliteral comparisons and unsupported encodings
residual. Test missing fields, NULL literals, unknown dictionary values, numeric
columns and negated forms against DataFusion before changing the classifier.
The paired diagnostic shows that avoiding materialization is useful; the profile-only
commit 446d578 contained no production implementation or native target claim.

## Exact classifier implementation

The follow-up translates indexed scalar numeric/count/dictionary comparisons
using the existing bitmap IR. For non-NULL literals, DISTINCT is IS NULL OR <>,
and NOT DISTINCT is IS NOT NULL AND =. The explicit presence guard supplies a
FALSE mask for absent values; without it equality would remain UNKNOWN beneath
NOT. NULL literals become IS NOT NULL or IS NULL respectively. Reversed
literal/column operands are supported. Arrays, column-to-column comparisons and
unsupported encodings remain residual. No seal encoding or golden SQL changes.

The regression fixture spans two vessels and the 65,536-bucket shard boundary,
with NULLs, whole-shard missing fields, unknown dictionary values, counts and
Float64 reconstruction collisions. It compares pushed rows against an ordinary
Arrow MemTable evaluated by DataFusion, and directly verifies that every distinct
predicate and its negation have empty UNKNOWN masks. Nested NOT/AND/OR and
reversed operands are included. The interval fixture compares both bitmap forms
against a forced materialization fallback and preserves the NULL arm check.

Observed targeted tests: boundaries 2/2, distinct 2/2, provider 6/6 and the
ordinary Q5 interval regression 1/1; the boat profile is ignored by default.
The distinct test covers 132 pushed/residual SQL comparisons, plus direct
truth-mask checks. Full cargo test -p ti-sql passed: 39 tests, with the three
fixture/performance tests ignored as declared. Crate-scoped cargo fmt --check
passed. Strict cargo clippy -p ti-sql --all-targets -- -D warnings also passed
on Rust 1.99.

The lead accepted the native corpus/timing gates at cebf5ea; details follow below.

### Post-change store-full profile

Rust 1.99 debug, same read-only store-full and 256 MiB cache, seven warm runs:

| Query | Warm p50 | Warm p95 | Materialized rows | Intervals |
|---|---:|---:|---:|---:|
| Original DISTINCT predicate, now pushed | 55.19 ms | 72.15 ms | 0 | 134 |
| Explicit NULL-preserving bitmap equivalent | 54.74 ms | 68.87 ms | 0 | 134 |

The original predicate improves from 770.15 to 55.19 ms p50 (13.96 times) in
this environment. Every cold/warm answer matched the paired equivalent. A
separate complete JSON comparison against the pre-change profile also found
all 134 intervals identical. Both physical plans use bitmap runs. Cold totals
were 64,082.79 and 63,936.71 ms; these bind-mount numbers are not native targets.
Artifact: .lanes/data/query-cache/q5-profile-after.json (not committed).

The measured integration binary was built by the targeted cargo test command,
then run directly after the full suite finished, with no compiler running:

```sh
TI_Q5_STORE=<read-only-store-full> TI_Q5_OUTPUT=<fresh-output.json> TI_Q5_RUNS=7 \
target/debug/deps/q5_profile-<cargo-hash> \
boat_store_q5_paired_profile --ignored --nocapture
```

The release cargo command above remains the native host reproduction. The
61/0/1 corpus result, native Q5-002 p50 target and other-class regression gate
have not been run locally for this change.

## Native acceptance at cebf5ea

Lead-reported on 2026-10-07, Rust 1.96 release on the Windows host:

| Paired Q5-002 query | Warm p50 | Warm p95 | Intervals | Materialized rows |
|---|---:|---:|---:|---:|
| DISTINCT, exact bitmap pushdown | 8.45 ms | 8.72 ms | 134 | 0 |
| Explicit NULL-preserving bitmap equivalent | 9.29 ms | — | 134 | 0 |

This is below the 150 ms target, from about 300 ms before the classifier fix.
The unchanged golden corpus passes 61/0/1. All 20 answer fingerprints match.
The class A/B had two reruns, with no concurrent build in the second:

| Class | First p50 change | Second p50 change |
|---|---:|---:|
| Q1 | +3% | +6% |
| Q2 | −6% | −15% |
| Q3 | −17% | −3% |
| Q4 | −12% | −9% |
| Q5 | −97% | — |
| Q7 | +3% | +1% |
| Q8 | +9% | +1% |

Q5's −97% change was reported once, without a separate per-rerun value.
The lead attributed Q8's first +9% (then +1%) to noise and Q1's +6%
(0.54→0.57 ms) to sub-millisecond jitter, and explicitly accepted the gates.
All classes now meet their p95 targets. These native results were run by the
lead, not re-run in the lane. The Pi currently runs cache build 6d7f5c1 with
64 MiB [query].sealed_cache_bytes and lead-reported 99 MB RSS after restart;
it is not yet the distinct-pushdown acceptance build.
