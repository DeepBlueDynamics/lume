# ti-store

`ti-store` is the storage engine for Lume TI, implementing:
- **Write-Ahead Log (WAL)**: Per-vessel segmented WAL with IEEE CRC32 frame checksums and torn-write recovery.
- **Roaring Row Models**: In-memory and `.rbm` serialized roaring bitmap representations for Presence, Set, BSI (signed integer and float metrics), Count, and Geo fields.
- **Catalog & Manifest**: Atomic persistence via temporary files, fsync, and atomic rename with directory fsync on Unix.
- **Open & Sealed Shards**: Local column addressing (0..=65535) per shard, bit-sliced predicate evaluation, deterministic `.rbm` encoding, canonical BLAKE3 content hashing, and shard repair.

## Durability & Crash Recovery Semantics

- **Process Crash Durability (`kill -9`)**:
  `apply` appends records to the vessel WAL and flushes user-space file buffers (`file.flush()`) before returning an acknowledgment. This guarantees that all acknowledged records reach the OS buffer cache before acknowledgment, ensuring they survive process termination (`kill -9`). Note that `kill -9` tests process crashes, not power loss.
- **Power Loss Durability**:
  Power-loss durability rests on fsync (`file.sync_data()` / `sync_all()`). Under decision D16, WAL files are fsynced at least every 1 s (timer-based group commit), whenever an idle interval elapses via `tick()`, immediately prior to shard flush and WAL truncation, and upon explicit store shutdown or drop. Power loss is thus bounded to ≈1 s of un-synced data.
- **WAL Format Versioning (D24)**:
  WAL record payloads use length-prefixed little-endian fixed-integer `bincode` 1.3 serialization. The payload format is an on-disk format versioned by the frozen WAL header (`v1`). Any change of encoder or major version requires a WAL header version bump plus a migration note.
