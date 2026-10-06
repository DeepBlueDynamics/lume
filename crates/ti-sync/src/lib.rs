//! Synchronization and replication engine for the Lume Telemetry Index.
//!
//! Provides:
//! - Manifest diffing local vs shore -> missing (shard, version, hash) list
//! - Resumable chunked upload with per-chunk BLAKE3 digests and byte-level idempotence (§84)
//! - Cryptographic verification of canonical shard content digest (§81) and catalog hash (§82)
//! - Shore import with vessel URN re-mapping via `remap_vessel`
//! - Transport traits, loopback in-process transport, and lossy transport for simulation

pub mod chunk;
pub mod client;
pub mod diff;
pub mod package;
pub mod shore;
pub mod tar;
pub mod transport;

pub use chunk::{chunk_package, ChunkAck, UploadChunk, UploadStatus, DEFAULT_CHUNK_SIZE};
pub use client::{SyncClient, SyncReport};
pub use diff::{diff_manifests, diff_manifests_with_resolver, MissingShard};
pub use package::{
    compute_catalog_hash, package_sealed_shard, unpack_and_verify_shard, CatalogSnapshot,
    DictionaryEntry, ShardPackage, UnpackedShard,
};
pub use shore::ShoreReceiver;
pub use tar::{create_tar, parse_tar};
pub use transport::{LoopbackTransport, LossyTransport, Transport};
