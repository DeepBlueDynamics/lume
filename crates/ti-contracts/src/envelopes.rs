use crate::{BucketIx, Error, Result};
use std::collections::BTreeSet;

/// Format version for WAL and shard envelopes; readers reject unknown versions.
pub const FORMAT_VERSION: u16 = 1;
/// WAL segment magic, independent of the record payload codec.
pub const WAL_MAGIC: [u8; 8] = *b"LUMETIW1";
/// Shard file magic; payload contains ordered roaring-portable rows.
pub const SHARD_MAGIC: [u8; 8] = *b"LUMETIS1";

/// Versioned WAL segment header. Binary representation is magic, version and
/// vessel-URN byte length (u32 LE), followed by URN UTF-8 bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalHeader {
    /// Canonical vessel URN.
    pub vessel_urn: String,
    /// Envelope version.
    pub version: u16,
}

/// Record frame metadata: payload length u32 LE, sequence u64 LE, CRC32 u32 LE.
/// CRC covers sequence bytes followed by payload bytes. Payload codec is fixed in spec 14.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalRecordHeader {
    /// Encoded payload bytes.
    pub payload_len: u32,
    /// Monotonic sequence within a vessel's WAL, starting at one.
    pub sequence: u64,
    /// IEEE CRC32 over sequence and payload.
    pub crc32: u32,
}

/// Portable shard-file header. Identity uses URN, never a foreign ordinal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShardFileHeader {
    /// Format version.
    pub version: u16,
    /// Canonical vessel URN.
    pub vessel_urn: String,
    /// 65,536-bucket shard number.
    pub shard: u32,
    /// Store-local field ID, resolved using the transferred catalog snapshot.
    pub field: u32,
    /// Fixed bucket width in seconds.
    pub width_seconds: u64,
    /// Number of portable roaring row payloads.
    pub row_count: u32,
}

/// Federation identity for one immutable shard version and its catalog snapshot.
/// hash identifies canonical field contents; catalog_hash prevents ambiguous row IDs.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TransferIdentity {
    /// Canonical URN used to remap the vessel.
    pub vessel_urn: String,
    /// Shard number.
    pub shard: u32,
    /// Repair version, starting at one.
    pub version: u64,
    /// Bucket width.
    pub width_seconds: u64,
    /// Inclusive first bucket.
    pub from: BucketIx,
    /// Inclusive last bucket.
    pub to: BucketIx,
    /// Raw BLAKE3 digest over the canonical field stream.
    pub hash: [u8; 32],
    /// BLAKE3 digest of canonical catalog snapshot required to decode fields/rows.
    pub catalog_hash: [u8; 32],
}

fn urn_bytes(urn: &str) -> Result<Vec<u8>> {
    if crate::validate_entity_urn(urn).is_err() {
        return Err(Error::InvalidInput(
            "vessel_urn must be a canonical entity URN".into(),
        ));
    }
    let len = u32::try_from(urn.len()).map_err(|_| Error::Overflow("vessel_urn length"))?;
    let mut out = len.to_le_bytes().to_vec();
    out.extend_from_slice(urn.as_bytes());
    Ok(out)
}

impl WalHeader {
    /// Encode the stable header; unknown versions and aliases are rejected.
    pub fn encode(&self) -> Result<Vec<u8>> {
        if self.version != FORMAT_VERSION {
            return Err(Error::Unsupported("wal.version".into()));
        }
        let mut out = WAL_MAGIC.to_vec();
        out.extend_from_slice(&self.version.to_le_bytes());
        out.extend(urn_bytes(&self.vessel_urn)?);
        Ok(out)
    }
}

impl WalRecordHeader {
    /// Encode a frame header. Sequence zero is reserved for no-record checkpoints.
    pub fn encode(&self) -> Result<[u8; 16]> {
        if self.sequence == 0 || self.payload_len == 0 {
            return Err(Error::InvalidInput(
                "wal.record sequence/payload_len must be positive".into(),
            ));
        }
        let mut out = [0; 16];
        out[..4].copy_from_slice(&self.payload_len.to_le_bytes());
        out[4..12].copy_from_slice(&self.sequence.to_le_bytes());
        out[12..].copy_from_slice(&self.crc32.to_le_bytes());
        Ok(out)
    }
}

impl ShardFileHeader {
    /// Encode magic, version, URN, shard, field, width and row count in little endian.
    pub fn encode(&self) -> Result<Vec<u8>> {
        if self.version != FORMAT_VERSION {
            return Err(Error::Unsupported("shard.version".into()));
        }
        if self.width_seconds == 0 || self.shard > 65535 {
            return Err(Error::InvalidInput("shard.width_seconds/shard".into()));
        }
        let mut out = SHARD_MAGIC.to_vec();
        out.extend_from_slice(&self.version.to_le_bytes());
        out.extend(urn_bytes(&self.vessel_urn)?);
        out.extend_from_slice(&self.shard.to_le_bytes());
        out.extend_from_slice(&self.field.to_le_bytes());
        out.extend_from_slice(&self.width_seconds.to_le_bytes());
        out.extend_from_slice(&self.row_count.to_le_bytes());
        Ok(out)
    }
}

impl TransferIdentity {
    /// Check coverage and namespace before import.
    pub fn validate(&self) -> Result<()> {
        urn_bytes(&self.vessel_urn)?;
        if self.version == 0
            || self.width_seconds == 0
            || self.from > self.to
            || (self.from >> 16) != self.shard
            || (self.to >> 16) != self.shard
        {
            return Err(Error::InvalidInput(
                "transfer.version/width_seconds/coverage".into(),
            ));
        }
        Ok(())
    }
}

/// Exact bytes a hasher consumes: domain tag, then ascending field IDs with
/// u32 LE ID, u64 LE file length, and exact versioned file bytes.
/// This helper does not hash; W2/W8 stream the same ordering through BLAKE3.
pub fn canonical_shard_input(files: &[(u32, Vec<u8>)]) -> Result<Vec<u8>> {
    let mut ordered: Vec<_> = files.iter().collect();
    ordered.sort_by_key(|(id, _)| *id);
    let mut seen = BTreeSet::new();
    let mut out = b"LumeTI/shard/v1\0".to_vec();
    for (id, bytes) in ordered {
        if !seen.insert(*id) {
            return Err(Error::InvalidInput("shard.field: duplicate".into()));
        }
        out.extend_from_slice(&id.to_le_bytes());
        out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        out.extend_from_slice(bytes);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wal_header_domain_and_version() {
        let mut h = WalHeader {
            vessel_urn: "vessels.urn:test".into(),
            version: 1,
        };
        let bytes = h.encode().unwrap();
        assert_eq!(&bytes[..8], &WAL_MAGIC);
        assert_eq!(&bytes[8..10], &1u16.to_le_bytes());
        h.version = 2;
        assert!(h.encode().is_err());
        h.version = 1;
        h.vessel_urn = "vessels.self".into();
        assert!(h.encode().is_err());
    }
    #[test]
    fn wal_frame_layout_and_sequence_validation() {
        let h = WalRecordHeader {
            payload_len: 12,
            sequence: 7,
            crc32: 123,
        };
        let b = h.encode().unwrap();
        assert_eq!(&b[..4], &12u32.to_le_bytes());
        assert_eq!(&b[4..12], &7u64.to_le_bytes());
        assert!(WalRecordHeader { sequence: 0, ..h }.encode().is_err());
    }
    #[test]
    fn shard_header_identity_and_width() {
        let mut h = ShardFileHeader {
            version: 1,
            vessel_urn: "vessels.urn:test".into(),
            shard: 65535,
            field: 2,
            width_seconds: 10,
            row_count: 3,
        };
        assert_eq!(&h.encode().unwrap()[..8], &SHARD_MAGIC);
        h.width_seconds = 0;
        assert!(h.encode().is_err());
    }
    #[test]
    fn canonical_input_is_order_independent_and_framed() {
        let a = (1, vec![2, 3]);
        let b = (2, vec![4]);
        assert_eq!(
            canonical_shard_input(&[a.clone(), b.clone()]).unwrap(),
            canonical_shard_input(&[b, a.clone()]).unwrap()
        );
        assert!(canonical_shard_input(&[a.clone(), a]).is_err());
        assert_ne!(
            canonical_shard_input(&[(1, vec![2, 3])]).unwrap(),
            canonical_shard_input(&[(1, vec![2]), (3, vec![])]).unwrap()
        );
    }
    #[test]
    fn transfer_coverage_and_version() {
        let mut t = TransferIdentity {
            vessel_urn: "vessels.urn:test".into(),
            shard: 1,
            version: 1,
            width_seconds: 10,
            from: 65536,
            to: 131071,
            hash: [1; 32],
            catalog_hash: [2; 32],
        };
        assert!(t.validate().is_ok());
        assert_eq!(t, t.clone());
        t.to = 131072;
        assert!(t.validate().is_err());
    }
}
