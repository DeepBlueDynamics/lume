# Bucket-gap investigation

## Reproduction before the fix

Verified on the pre-count-paths base `4bad710`, using the generated delta stream in
`crates/ti-ingest/tests/bucket_gap.rs`. Three battery paths update every three
seconds (nominal capacity 25,712,640,000, charge efficiency 0.95, temperature
coefficient 0.01), while depth and SOG update every second.

`CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 CARGO_PROFILE_DEV_STRIP=symbols
CARGO_TARGET_TMPDIR=$lane/.test-tmp TMPDIR=$lane/.test-tmp cargo test --locked
-p ti-ingest --test bucket_gap` exited 101 before any ingest fix:

- Clean maintenance before/after deltas, including receive/event time straddling
  boundaries, flush every five seconds, WAL ticks and a late delta: passed.
- A one-shot sink apply failure while maintenance closes bucket 0: failed.
  Capacity presence after retry was `[1, 2, 3]`, expected `[0, 1, 2, 3]`.

The bucketer removed the complete window before synthesis and sink apply. If
apply failed, the service logged or ignored the error and the window was already
lost. The injected failure proves this loss path; it does not prove which error,
if any, occurred on the Pi. The lead will re-check the live three-path bucket
counts after deployment. Numeric source preferences were not involved in the
clean pre-count-paths reproduction.

## Repair and retry boundary

Windows are keyed by (store, vessel, bucket), not by source or PGN. Record
synthesis and sink apply borrow the complete window; removal and close
notification happen only after acknowledgement. Failures retain the window,
including late repairs. Retries reinstall its complete snapshot with rewrite=true,
so partial publication or a lost acknowledgement cannot double-count. New samples
continue accumulating while the window waits. Maintenance retries independently
of dirty-row flushing, with monotonic exponential delays of 1, 2, 4, 8, 16 and
30 seconds (30-second maximum). Shutdown gives retries five seconds, then returns
an error rather than claiming a clean flush. Pending in-memory windows do not
add a new crash-durability guarantee beyond the existing WAL.

Acknowledged close notifications remain queued across later-window failures and
are dispatched after sink flush; late rewrites do not retrigger rules. Message
processing finishes the remaining values after a close failure instead of losing
the rest of a multi-value delta.

The production service previously discarded periodic advance_watermark errors
with let _ =. Thus an apply/emit failure on that path could leave no log at all.
The bucketer now prints the first cause once per store/vessel/bucket, counts every
failed attempt, and suppresses repeated logging for retries of that window.
Periodic WAL tick and store-flush failures also print their causes.

The battery-only Pi pattern is possible with vessel-wide windows: after a failed
close discarded all paths, delayed depth/SOG samples could recreate that bucket
while the slower battery PGN had no more samples for it. The synthetic delayed
fast-path test verifies that the repair preserves all five paths in this sequence.
This is a code-supported explanation, not evidence that this timing or a specific
sink error occurred on the Pi. The live Q6 re-check remains the deployment oracle.

## Persistent failure bounds

Each store caps open retained windows at 64 and conservative reservations at
64 MiB, whichever is reached first. Every sample reserves 1024 bytes plus
8 times path/source byte lengths and 4 times string-value length, including
repeated contributions. This intentionally overestimates retained accumulator
memory rather than claiming an RSS measurement. A rejected reservation mutates
no window; an acknowledged window releases its reservations.

At the cap the store logs INGEST BLOCKED once, sets ingest_blocked=true and
increments samples_rejected_blocked for each rejected normalized contribution.
Other matching stores still process the input. Retained windows are retried and
drained in vessel/bucket order before the blocked store resumes; its recovery
transition is logged. Rejected new contributions are explicitly accounted for,
not buffered without limit. The persistent-failure fixture checks all 64 retained
windows and their three battery values after recovery, plus resumed acceptance.

## Status counters

The six failure/drop fields plus ingest_blocked and samples_rejected_blocked are
written to ingest_status.json and read afresh by /ti/status.
They are service-lifetime counters; older or absent status files expose NULL.
Multi-store totals count store-local contributions, so fanout may count one input
sample more than once.

- samples_dropped_late: samples outside the bucket time domain, or late event
  counts beyond the 128-window repair cache. Ordinary accepted late rewrites do
  not increment it.
- samples_dropped_nonfinite: nonfinite numeric samples or position coordinates.
- samples_skipped_magnitude: finite samples that cannot fit fixed-point BSI.
  Listed count_paths still count them; value aggregates exclude them. Historical
  per-path skipped_magnitudes records remain available separately.
- samples_rejected_source: finite count_paths contributions excluded from the
  preferred-source bare count, including previously selected contributions
  displaced when a better source arrives. Existing all-source value aggregates
  and ordinary set preference semantics remain unchanged.
- apply_failures: failed record-synthesis or sink-apply attempts.
- apply_retries: actual attempts to republish a previously failed window, including
  failed retries; checks before its backoff deadline do not count.

## Verification

On the count_paths merge base 7bf038d, cargo test --locked -p ti-ingest passed
64 tests with the existing three fixture-dependent tests ignored. Final root
cargo test --locked --features ti passed 100 tests, zero failures and the two
existing fixture-dependent ignores; no fleet test exclusion was used. The default
cargo build --locked also passed. The eleven bucket-gap
tests cover boundary maintenance, retained failure, lost acknowledgement/count
replacement, delayed fast paths, counter serialization, bounded backoff, one
cause log per window, persistent-failure caps/recovery and store isolation. Root HTTP status verification and final regression are
recorded in the handoff mail. Host strict clippy/rustfmt and the Pi Q6 check are
not claimed as locally run.
