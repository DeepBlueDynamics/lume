# PostgreSQL frontend frame bounds (A12)

Reviewed pgwire 0.41.0 before changing Lume. Its [decode_packet](https://github.com/sunng87/pgwire/blob/v0.41.0/src/messages/codec.rs#L70) reads the declared length and checks the maximum before waiting for the body; it does not reserve the declared body size. [Startup](https://github.com/sunng87/pgwire/blob/v0.41.0/src/messages/startup.rs#L7) is capped at 10,000 bytes. [Query](https://github.com/sunng87/pgwire/blob/v0.41.0/src/messages/simplequery.rs#L24) permits 0x3fffffff - 1 bytes (about 1 GiB). That bound is too large for 32 clients on an 8 GiB Pi.

Lume now wraps the plaintext stream (including decrypted TLS) in a header-first reader. It accepts at most 1 MiB per frontend frame by default, including the four-byte length field; Query, Parse, Bind, authentication and other messages use the same cap. Startup retains the smaller 10,000-byte upstream cap. The existing PgOptions struct exposes max_frame_bytes for embedders; no CLI/default policy change is added. Invalid settings fail before listening.

Only the four-byte startup header or five-byte tagged header is read before validation. Body reads stop at the current frame boundary, so a pipelined next frame is checked separately. Signed-negative, oversized and undersized declarations fail before body buffering. Decoder failures send a fatal SQLSTATE 08P01 ErrorResponse and close the connection. Existing startup, idle and write deadlines still apply.

The raw TCP regression in tests/ti_pg_frames.rs sends incomplete oversized packets without half-closing: startup declarations of 2 GiB and 0x7fffffff, and Query/Parse/Bind declarations of 2 MiB and both huge lengths. It requires an ErrorResponse and connection closure within a two-second socket timeout, then sends a 512 KiB Query frame, which passes transport validation and receives a non-fatal 22000 error from the unchanged 64 KiB SQL-text evaluation limit. SELECT 1 succeeds on that same connection, and the listener serves another client. Input-frame and SQL-text caps are independent. Allocation safety follows from validating the fixed-size header before exposing it or its body to pgwire; the test checks fast rejection, not process RSS.

## Verification

Run in .lanes/w4 on rustc 1.96.0 with CARGO_INCREMENTAL=0, -j1, lane-local TMPDIR/CARGO_TARGET_TMPDIR, and local CARGO_PROFILE_TEST_DEBUG=0 / CARGO_PROFILE_DEV_DEBUG=0 for disk usage (no shipped profile change).

- Full cargo test --locked --features ti: exit 0 with the lead-approved skip of search::tests::concurrent_readers_never_observe_a_partial_index; existing fixture ignores unchanged. Root library: 85 passed, 1 ignored, 1 filtered. The frame regression passed in 1.29 s; all other integration suites and doctests passed.
- Whole-root plus eight TI crates: cargo clippy --locked --features ti --all-targets -- -D warnings, exit 0 (5m 59s).
- Both CI formatting lines: exit 0; TI crate cargo fmt --check and root rustfmt --check globs, including the new tests/ti_pg_frames.rs.
- Default cargo build --locked -p lume: exit 0 with zero warnings (39.31 s). The five former unused-variable warnings are removed without changing argument validation.
- Largest observed target directory: 4.9 GiB, below the 8 GiB cap; not a continuously sampled peak.

Final cargo clean removed 6,129 files (4.9 GiB); target/ and lane test scratch are absent.

No new dependencies. No merge or push.
