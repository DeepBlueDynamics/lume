//! Resumable chunked upload framing with per-chunk BLAKE3 digests.
//!
//! Enforces:
//! - Fixed-size chunks with individual BLAKE3 verification
//! - Byte-offset addressing
//! - Idempotent upload replay per spec 14 §84

use ti_contracts::{Error, Result, ShardManifestEntry, TransferIdentity};

use crate::package::ShardPackage;

/// Default chunk size: 64 KiB.
pub const DEFAULT_CHUNK_SIZE: usize = 64 * 1024;

/// A single verifiable chunk of an upload session.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UploadChunk {
    /// Identity of the shard version being uploaded.
    pub transfer: TransferIdentity,
    /// 0-indexed chunk number.
    pub chunk_index: u32,
    /// Total number of chunks in this upload.
    pub total_chunks: u32,
    /// Byte offset within the complete packaged stream.
    pub offset: u64,
    /// Total byte size of the packaged stream.
    pub total_bytes: u64,
    /// Chunk payload bytes.
    pub data: Vec<u8>,
    /// Per-chunk BLAKE3 digest.
    pub chunk_hash: [u8; 32],
}

impl UploadChunk {
    pub fn new(
        transfer: TransferIdentity,
        chunk_index: u32,
        total_chunks: u32,
        offset: u64,
        total_bytes: u64,
        data: Vec<u8>,
    ) -> Self {
        let chunk_hash = *blake3::hash(&data).as_bytes();
        Self {
            transfer,
            chunk_index,
            total_chunks,
            offset,
            total_bytes,
            data,
            chunk_hash,
        }
    }

    /// Validate the chunk's cryptographic integrity and bounds.
    pub fn validate(&self) -> Result<()> {
        let computed = *blake3::hash(&self.data).as_bytes();
        if computed != self.chunk_hash {
            return Err(Error::Corrupt(format!(
                "chunk {} hash mismatch: expected {:?}, got {:?}",
                self.chunk_index, self.chunk_hash, computed
            )));
        }
        let end = self
            .offset
            .checked_add(self.data.len() as u64)
            .ok_or_else(|| Error::InvalidInput("chunk offset plus length overflows".into()))?;
        if end > self.total_bytes {
            return Err(Error::InvalidInput(
                "chunk bounds exceed total_bytes".into(),
            ));
        }
        Ok(())
    }
}

/// Acknowledgment of a received chunk from the shore receiver.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ChunkAck {
    pub chunk_index: u32,
    pub offset: u64,
    pub bytes_acknowledged: u64,
    pub total_bytes: u64,
    pub is_complete: bool,
}

/// Status of an upload session on the shore receiver.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum UploadStatus {
    /// No chunks have been received for this transfer.
    NotStarted,
    /// Upload is in progress; lists acknowledged chunk indices.
    InProgress {
        acknowledged_chunks: Vec<u32>,
        bytes_received: u64,
        total_bytes: u64,
    },
    /// All chunks received, verified, and installed into manifest.
    Completed { manifest_entry: ShardManifestEntry },
}

/// Split a packaged shard into fixed-size upload chunks.
pub fn chunk_package(package: &ShardPackage, chunk_size: usize) -> Vec<UploadChunk> {
    let chunk_size = chunk_size.max(1);
    let total_bytes = package.tar_bytes.len() as u64;
    let total_chunks = if package.tar_bytes.is_empty() {
        1
    } else {
        package.tar_bytes.len().div_ceil(chunk_size) as u32
    };

    if package.tar_bytes.is_empty() {
        return vec![UploadChunk::new(
            package.transfer.clone(),
            0,
            1,
            0,
            0,
            Vec::new(),
        )];
    }

    let mut chunks = Vec::with_capacity(total_chunks as usize);
    for (i, chunk_slice) in package.tar_bytes.chunks(chunk_size).enumerate() {
        let offset = (i * chunk_size) as u64;
        chunks.push(UploadChunk::new(
            package.transfer.clone(),
            i as u32,
            total_chunks,
            offset,
            total_bytes,
            chunk_slice.to_vec(),
        ));
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_offset_overflow_and_out_of_bounds_return_errors() {
        let transfer = TransferIdentity {
            vessel_urn: "vessels.urn:test".into(),
            shard: 0,
            version: 1,
            width_seconds: 10,
            from: 0,
            to: 10,
            hash: [0; 32],
            catalog_hash: [0; 32],
        };
        let overflow = UploadChunk::new(transfer.clone(), 0, 1, u64::MAX, u64::MAX, vec![1]);
        assert!(
            matches!(overflow.validate(), Err(Error::InvalidInput(message)) if message.contains("overflows"))
        );
        let past_end = UploadChunk::new(transfer.clone(), 0, 1, 4, 4, vec![1]);
        assert!(
            matches!(past_end.validate(), Err(Error::InvalidInput(message)) if message.contains("exceed"))
        );
        let exact = UploadChunk::new(transfer, 0, 1, 3, 4, vec![1]);
        assert!(exact.validate().is_ok());
    }

    #[test]
    fn test_chunk_validation_and_tampering() {
        let transfer = TransferIdentity {
            vessel_urn: "vessels.urn:test".into(),
            shard: 0,
            version: 1,
            width_seconds: 10,
            from: 0,
            to: 10,
            hash: [0u8; 32],
            catalog_hash: [0u8; 32],
        };

        let mut chunk = UploadChunk::new(transfer, 0, 1, 0, 4, vec![1, 2, 3, 4]);
        assert!(chunk.validate().is_ok());

        // Tamper with data
        chunk.data[0] = 99;
        assert!(chunk.validate().is_err());
    }
}
