# A9 — PR #4 follow-up hardening

Work is limited to .lanes/w4, rebased onto the docs-only plan/lume-ti e004349 update.

## Changes

- Post-authentication pgwire idle reads expire after 10 minutes; stalled socket write/flush/shutdown operations expire after 30 seconds. PgOptions provides configurable positive durations. Query evaluation is outside these socket deadlines. A deterministic duplex test stalls a write; the SCRAM integration test cycles 33 idle clients and then queries again to check admission recovery.
- Completed sync sessions expire at their replay TTL. Published manifests still acknowledge replays without a staging session. The deterministic expiry test fills the admission cap, advances the expiry clock, verifies published replay, and admits another transfer.
- Chunk offset plus length and staging byte admission use checked arithmetic. u64::MAX plus a nonempty chunk and a chunk extending beyond the declared size return errors.
- Invalid websocket URL errors omit the URL and underlying parser detail, keeping userinfo/query credentials out of reconnect logs. Legacy raw-token CLI forms retain their behavior and warn toward ingest --token <token-file> and sync --token-file <path>. Warnings never echo the value.
- Each SQL session uses a bounded DataFusion working-memory pool: min(512 MiB, detectable RAM/4), with LUME_TI_QUERY_MEMORY_BYTES override. The pool is shared by that session's queries, separately from cache/catalog/input/UDF memory and process RSS. Unknown-platform RAM detection falls back to 512 MiB. The allocation test compares a 4 KiB pool against 64 MiB over 100,000 generated GROUP BY keys and checks reservation release and a subsequent query.
- D51 remains PROPOSED, awaiting user approval; no HTTP/MCP/SSE authentication is implemented here.

## Verification

Verified so far on rustc 1.96.0, CARGO_INCREMENTAL=0:
- Full root TI suite exits 0, with only the lead-approved container exclusion of the unchanged concurrent index-rename test. Root library: 82 passed, one ignored, one filtered; all integration suites pass.
- Pgwire integration suite: 9 passed; token-warning integration suite: 2 passed.
- Initial full ti-sync suite: 15 passed.
- Both CI formatting commands exit 0.

Full ti-sql, ti-ingest and ti-sync suites also exit 0, with fixture-dependent ignores unchanged. The 4 KiB GROUP BY fails cleanly, the 64 MiB version returns 100,000 rows, reservations return to zero, and SELECT 1 still succeeds.

Strict root + eight TI-crate clippy with --all-targets -- -D warnings exits 0. Its initial items_after_test_module failure was fixed by moving the pgwire deadline test module to the end of the file; the lead independently made the same relocation on the host. Root rustfmt and git diff --check pass afterward.

All 26 Q1–Q8 query row counts and answer fingerprints match bench/results/2026-10-08-0eff8df.json (cache_on). The candidate ran with LUME_TI_QUERY_MEMORY_BYTES=536870912 and a 256 MiB sealed cache against read-only store-full. Evidence is in .lanes/data/a9-hardening/correctness-fingerprints.json and .lanes/data/a9-hardening/pool512. The debug runner completed without resource errors. Container timings are not performance evidence; the lead's native p95 A/B is running separately.

The lead reports that the native full TI suite, including the unchanged index-rename test, and the three crate suites pass. That native result is attributed to the lead, not a local run.

## Disk handling

The initial root tests retained 4.8 GiB. A separate crate graph grew from 6.1 GiB to 11 GiB during linking, exceeding the 8 GiB cap; that build was immediately interrupted (exit 130). cargo clean removed 11.3 GiB. Remaining test runs use -j1 and CARGO_PROFILE_TEST_DEBUG=0/CARGO_PROFILE_DEV_DEBUG=0; the fresh query-runner build uses -j2 with one executable; these are local environment settings, with no shipped profile change. The interrupted run is not counted as passing. After successful checks, final cargo clean removed 5,593 files (2.1 GiB); .test-tmp was deleted.
