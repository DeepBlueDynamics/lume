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

## Intended repair

Retain a window until record synthesis and sink acknowledgement succeed, and
retry the retained window on subsequent maintenance. Test both watermark close
and explicit flush, including an already acknowledged prefix of closes and
notification publication. Surface late, nonfinite, skipped-magnitude and rejected
source sample counters in ingest_status.json and /ti/status. Counter semantics
and final verification results will be recorded with the fix.
