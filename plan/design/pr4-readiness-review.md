# PR #4 readiness review (A8)

Review base: `plan/lume-ti` at `61d26d3`. Work branch: `ti/pr4-review`.
Scope: 189 changed Rust source/test files under `src/`, `crates/`, and `tests/`, with focused review of HTTP, pgwire, OTLP and sync entry points. This is source review plus targeted regression tests, not a penetration test or an upstream dependency audit.

## Ranked findings

| Priority | Location | Cause and consequence | Disposition |
| --- | --- | --- | --- |
| P1 | src/agent.rs:641, standalone OTLP routing | Standalone bearer protection covered OTLP routes, but the same port also exposed unauthenticated TI queries and MCP. | Fixed in 7f581c4: only POST /v1/metrics and /v1/logs are reachable; all other paths return 404, with or without a configured token. README and SETUP §14 now describe the separation. |
| P1 | crates/ti-sync/src/tar.rs:83, parse_tar | A tiny checksummed archive could claim a multi-GiB entry and allocate it before validating remaining bytes or shard integrity. The parser also accepted unsafe paths and link entry types. | Fixed in 31dc7f3: rejects sizes above remaining archive bytes or the 64 MiB upload cap before allocation; accepts only regular files with confined relative names; rejects traversal, absolute paths, platform prefixes, ustar prefixes and links. Targeted malicious-header/path tests added. |
| P2 | src/agent.rs:593, handle_connection | Missing header terminator could produce an out-of-bounds body slice. Lossy UTF-8 header decoding could also invalidate byte offsets. | Fixed in dc16052: requires complete, valid UTF-8 headers and returns 400 for incomplete or invalid headers. Raw byte offsets are retained. |
| P2 | src/agent.rs:691, MCP body reader | Unbounded Content-Length allocation, accepted truncated bodies, and no HTTP socket read/write timeouts. | Fixed in dc16052: caps MCP bodies at 8 MiB with 413, reads bounded chunks, rejects duplicate/invalid Content-Length and unsupported Transfer-Encoding, rejects truncated bodies, and sets 30 s read/write timeouts including overload responses. These are per-operation timeouts, not an absolute request deadline. |
| P2 | src/agent.rs:900, serve_configured | Ordinary non-loopback HTTP exposes unauthenticated TI and MCP, including indexing tools in plain serve. Plain serve defaults to 0.0.0.0. | Fixed warning/documentation in e1450b3; preserves the approved trusted-LAN behavior. Authentication remains proposed D51, below. Standalone OTLP is exempt because its only routes require its configured bearer on non-loopback binds. |
| P2 | src/ti_pg.rs:process_connection (1036 at review base) | TLS handshake and startup/auth have 10 s deadlines, but subsequent socket reads and protocol writes have no deadlines. An authenticated stalled client can retain a connection slot; concurrent clients are capped at 32. | Findings-only per lead. Add post-auth read/write deadlines in a separate scope. Upstream frame-size behavior was not verified; do not assume the output cap bounds input frames. |
| P2 | crates/ti-sql/src/session.rs:new_with_bitmap_aggregates and engine.rs:query_with_parameters | SessionContext uses the default runtime without a configured evaluation memory budget. Output row/byte caps apply after execution produces a batch. pgwire likewise checks emitted rows after obtaining batches. | Findings-only per lead. Source-order inference: large expressions or intermediate results can allocate beyond the response cap. No deliberate OOM test was run. Add bounded evaluation memory and query deadlines separately. |
| P2 | crates/ti-sync/src/shore.rs:prune_expired (91 at review base) | Completed staging sessions are retained regardless of TTL; the session count cap is 64. | Source-derived finding: enough distinct completed transfers can exhaust admission even after TTL. Not reproduced in this scope. Expire completed sessions after the replay window, preserving published-manifest idempotency. |
| P3 | src/main.rs:ingest/sync token arguments | Both commands accept raw tokens in argv as well as token files. | Existing compatibility behavior; recommend token-file usage to avoid process-list exposure. No token was printed or committed during review. |
| P3 | crates/ti-ingest/src/websocket.rs:60 | Invalid URL errors include the supplied URL. | A URL containing credentials or sensitive query parameters could expose them in an error; this is an inference from the formatting path, not an observed credential leak. Redact URLs separately. |

Two additional P2 fixes were approved during review. `src/bm25.rs:742` used a UTF-8-unsafe byte-100 preview on the verbose rejection path. A regression reproduced the panic using a 🚤 character spanning bytes 99–103 and a zero-score candidate; aa43875 replaces it with a character-safe preview and the test passes. All range slices in bm25.rs/search.rs were inspected: the remaining Markdown string offset counts ASCII '#' characters, while snippet slicing indexes a bounded vector of lines.

`src/agent.rs:878` now owns an admission slot through a Drop guard so an unwinding handler panic releases it. The injected-panic regression passed: the handler thread unwinds and the active count returns to zero. Fixed in aab48f5.

## Proposed D51: HTTP/MCP authentication

This is a proposal, not an approved decision or implemented authentication scheme. Keep a distinct HTTP/MCP bearer loaded from a private token file, enforce it before dispatch/body processing on every TI, MCP and SSE route, and require it for non-loopback listeners. Separate read/query permissions from indexing/admin tools. Do not reuse a SCRAM verifier as a bearer. Use TLS or an authenticated TLS proxy for shore access. Keep the standalone OTLP listener ingestion-only with its own bearer. Propose a loopback default for plain MCP too. Changing default binds or trusted-LAN compatibility requires an explicit decision.

## Additional checks

No TODO/FIXME/XXX markers were found in the 189 reviewed Rust files. Nine ignored test annotations across eight files have explicit reasons: large corpus data, generated fixtures, a root TI executable, or native/host-only performance/oracle inputs. The referenced environment variables and fixture requirements still exist; no stale exclusion was identified.

Request body limits remain endpoint-specific: MCP 8 MiB, OTLP 8 MiB, sync chunks 16 MiB, ordinary TI requests 64 KiB. HTTP/MCP response and SQL admission caps remain unchanged. Sync bearer comparison is constant-time; OTLP bearer is checked before accepting data; non-loopback pgwire requires SCRAM configuration. The tar unpacker installs numeric field IDs rather than using archive names as filesystem destinations, but the parser now rejects unsafe entries rather than silently accepting unknown metadata.

## Verification

The unchanged atomic-index concurrency test is not reliable in this Docker Desktop Windows-bind-mounted lane: it failed locally with 24 and 97 read errors. Mount rename behavior is a hypothesis for the difference, not a proven cause. The lead reports 20/20 native Windows passes at 61d26d3 and passing ubuntu/ext4 CI since A5. Per his explicit instruction, the container remainder excludes this test without changing or weakening it; the full native suite remains his merge gate. Native results are reported by the lead, not independently run in this lane.

Checks run on rustc 1.96.0 with CARGO_INCREMENTAL=0 and scratch inside the lane:

- Standalone OTLP: `cargo test --locked --features ti --test ti_otlp` — exit 0, 7/7.
- HTTP malformed/truncated/oversize/stalled requests: targeted library tests — exit 0, 4/4; the warning test subsequently passed in the full library run (5/5 combined).
- TAR malformed length/path/link tests: `cargo test --locked -p ti-sync tar::tests` — exit 0, 4/4.
- Unmodified full root TI command — exit 101 at the atomic-index test; exact isolated retry also exit 101, as detailed above.
- Container remainder: `cargo test --locked --features ti --tests -- --skip search::tests::concurrent_readers_never_observe_a_partial_index` — exit 0. Library 79 passed, 1 ignored, 1 filtered; all integration suites passed.
- CI crate formatting: `cargo fmt --check -p ti-contracts -p ti-core -p ti-store -p ti-ingest -p ti-sql -p ti-sync -p ti-bench -p ti-geo` — exit 0.
- CI root formatting: `rustfmt --check --edition 2021 src/ti_*.rs src/chat_sql.rs src/sql.rs src/document_extract.rs src/crawl_list.rs tests/ti_*.rs tests/chat_sql.rs tests/support/*.rs` — exit 0.

Final container full command: `cargo +1.96.0 test --locked -j 2 --features ti -- --skip search::tests::concurrent_readers_never_observe_a_partial_index` — exit 0 after aa43875/aab48f5. Library 81 passed, 1 ignored, 1 filtered; all integration suites passed; root doctests ran (0 tests), exit 0.

Combined whole-root/eight-crate `cargo +1.96.0 clippy --locked -j 2 -p lume -p ti-contracts -p ti-core -p ti-store -p ti-ingest -p ti-sql -p ti-sync -p ti-bench -p ti-geo --features ti --all-targets -- -D warnings` — exit 0 before the two final panic fixes. Its final recheck was stopped at the lead's instruction after he reported native gates green. A container default build was not run.

The lead reports merge 6e27791 and native exit 0 for whole-package/eight-crate strict clippy, both CI fmt checks, the full unfiltered TI test suite (including atomic-index concurrency), ti-sync tests and the default build. These are lead-reported results, not independently run here.

Cleanup completed: cargo clean removed 6,086 files (5.3 GiB reported); last measured target size was 5.1 GiB, below 8 GiB. Lane scratch was deleted.
