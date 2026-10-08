# D53 — WAL checkpoints in open-field snapshots

Status: **APPROVED by Annual Echidna on 2026-10-08; A14 implementation.** No new dependencies. Sealed field encoding, canonical hash input and transfer packages remain unchanged.

## Failure and recovery boundary

A crash between open-field publication and WAL truncation leaves a newer snapshot beside older WAL records. Replaying an old non-rewrite insert against the snapshot can fail the numeric replacement guard before reaching the later rewrite. The deterministic insert 30 / rewrite 107 / flush / reopen reproduction failed 20/20 at 67b44ca and passed at its parent a8f32bd. 67b44ca began propagating replay errors previously ignored. Three pre-fix 1000-kill runs passed despite this deterministic failure: their notification-to-kill timing does not always preserve the overlap.

Each WAL-managed open field file now carries a 20-byte trailer immediately after its existing rows: eight-byte ASCII magic LUMECP01, highest covered WAL frame sequence as u64 LE, then IEEE CRC32 as u32 LE over those 16 bytes. Stage the rows and trailer together, sync the WAL before publishing snapshots, sync staged files, rename each field atomically, and propagate directory-sync failures. Checkpoints are per field because a crash may publish only some fields of a flush. Recovery skips records at or below that field's checkpoint and replays later groups in their original order through normal validation. Successful replay updates the in-memory checkpoint for the next flush. Direct OpenShard users without a WAL still emit legacy open files.

Sequences must never restart at one after truncation. Before truncating, atomically publish wal/<vessel>.seq: eight-byte ASCII LUMESEQ1, next sequence as u64 LE, CRC32 over the preceding 16 bytes; sync the file and containing directory. Keep the next sequence in memory. On reopen use the greater of the valid WAL's last sequence plus one and the persisted floor (one when absent). If the floor exceeds the end of a shortened old tail, truncate that already-flushed tail before appending at the floor, so future replay cannot stop at a sequence gap. The first recovered frame may have any positive sequence, with consecutive sequences thereafter; frame/header/payload encoding is unchanged. A corrupt floor or checkpoint fails explicitly. An orphan temporary sidecar is ignored.

## Upgrade and downgrade

A legacy store without a sequence sidecar starts above its existing valid WAL records. Fields without checkpoints use a one-time overlap fallback for scalar numeric/count/ordinary-string groups: retain the most recent rewrite/reset group and the final group if different, in WAL order. Multi-source/provenance and geo groups remain ordered. Checkpointed fields never use coalescing. The ordinary replacement guard is retained; invalid replacements are not silently accepted.

**Downgrade requires a coordinated conversion while all writers are stopped.** With the new binary, seal every open shard and confirm that every WAL contains only its header; then archive the sequence sidecars together with a store backup before opening the store with an older binary. A fresh re-derivation into a separate legacy-compatible store is the alternative. Merely sealing one shard, leaving open checkpointed fields, or leaving stale sequence floors while an old binary resumes sequences at one is unsafe on a later upgrade. D52's document-log downgrade requirements apply independently. Older binaries do not understand checkpoints or sequence floors, so recovery or writes can discard or replay the wrong history even if their row reader happens to ignore trailing bytes. No existing store-level version marker reliably guards this: FORMAT_VERSION must remain unchanged for sealed hashes. Never delete sequence sidecars independently from a running or checkpointed store.

## Reader audit

- Store::open_or_create and Store::open_readonly load open fields only through OpenShard::load_open and its shared read_open_checkpoint validator. Open query materialization uses the decoded fields; no mmap or zero-copy open-file reader was found.
- Seal uses decoded ShardData and the unchanged encode_field_file routine, which emits no trailer. Open files are removed only after manifest publication.
- Query reload, repair and layered sealed-base restoration use those Store entry points. DocStore compaction handles documents.log, not telemetry field files.
- TI benchmark/verify and SQL engines query through Store/ShardSource; no separate open-row decoder was found. ti-sql::store_width also probes a representative .rbm header (including open files), reading only the unchanged prefix through width_seconds. It never interprets rows or trailing bytes; TiEngine::open then opens the Store, where load_open validates and consumes every open trailer. Keep that width probe header-only to avoid reintroducing full-field reads on startup.
- ti-sync::package_sealed_shard reads only shards/<vessel>/<shard>/v<version>/*.rbm. Transfer and shore paths never ship open files.
- Canonical shard hashing occurs on sealed encoder bytes or sealed transfer files. Open files are not canonical seal inputs.

## Validation

Deterministic overlap, clear/reinsert, numeric/count/string/source values, partial field publication, legacy reset plus final duplicate, explicit metadata corruption, and first-upgrade/truncate/restart sequence tests are in crates/ti-store/tests/wal_checkpoint.rs and crash_rewrite_diagnosis.rs. Query rows are compared with versus without trailers, and sealed bytes/hashes are compared against legacy OpenShard output. The sealed encoder and row/header serializers are unchanged from a47ff21; the legacy OpenShard comparison asserts both individual field bytes and the canonical seal hash.

Rust 1.96 validation completed with CARGO_INCREMENTAL=0 and local dev/test debug info disabled to bound build cache:
- cargo test --locked -p ti-store: **60 passed**, including deterministic overlap repeated 20 times, nine checkpoint integration tests, all-field seal determinism and sealed repair.
- cargo test --locked --features ti -- --skip concurrent_readers_never_observe_a_partial_index: **exit 0**; only that approved environment-dependent skip and the existing fixture-dependent ignored tests.
- Whole-package TI strict clippy and eight-crate --all-targets strict clippy: **exit 0**, -D warnings.
- Eight-crate cargo fmt --check, root TI rustfmt --check, and touched-file rustfmt --check: **exit 0**.
- Default cargo build --locked: **exit 0**.

Three final-source 1000-kill runs used the identical crash_recovery-e398dde3af284c98 binary:

| Run | Runtime | Mid-apply | Mid-flush | Flush→truncate | Failures |
|---|---:|---:|---:|---:|---:|
| 1 | 309.51 s | 332 | 344 | 324 | 0 |
| 2 | 269.51 s | 332 | 344 | 324 | 0 |
| 3 | 255.02 s | 332 | 344 | 324 | 0 |

Raw logs are retained outside Git in .lanes/data/crash-repro/a14-final-{1,2,3}.txt. These are process-kill tests, not hardware power-cut tests. Measured target size after all gates was 5.3 GiB; cargo clean and scratch cleanup follow the final run.
