//! Write-Ahead Log (WAL) per vessel for ti-store.
//!
//! Envelopes and framing follow plan/spec/14-semantics.md §gap 6:
//! - Segment Header: `WAL_MAGIC` (LUMETIW1), version u16 LE, URN length u32 LE, URN bytes.
//! - Record Frame: payload_len u32 LE, sequence u64 LE (starts at 1), IEEE CRC32 u32 LE, payload bytes.
//! - CRC covers sequence bytes followed by payload bytes.
//! - Payload is a complete apply slice `Vec<BucketRecord>` serialized via bincode 1.x (fixed-int, LE).
//! - Replay stops cleanly at the first torn or CRC-bad record.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use bincode::Options;
use crc32fast::Hasher;
use ti_contracts::{BucketRecord, Error, Result, VesselOrd, FORMAT_VERSION, WAL_MAGIC};

fn bincode_options() -> impl bincode::Options {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_little_endian()
}

pub type WalReplayRecord = (u64, Vec<BucketRecord>);
pub type WalRecoveryResult = (String, u64, Vec<WalReplayRecord>, u64);

pub struct Wal {
    file: File,
    path: PathBuf,
    vessel_urn: String,
    next_sequence: u64,
    last_sync: Instant,
    has_unsynced: bool,
    header_len: u64,
}

impl Wal {
    /// Open an existing WAL or create a new one. Replays valid records and returns them.
    /// If an existing WAL has torn or incomplete records at the end, replay stops cleanly
    /// and truncates to the last valid record.
    pub fn open_or_create(
        wal_dir: &Path,
        vessel_ord: VesselOrd,
        vessel_urn: &str,
    ) -> Result<(Self, Vec<WalReplayRecord>)> {
        std::fs::create_dir_all(wal_dir)?;
        let path = wal_dir.join(format!("{}.wal", vessel_ord));

        if path.exists() {
            let (header_urn, header_len, records, valid_end_offset) = Self::recover(&path)?;
            if header_urn != vessel_urn {
                return Err(Error::InvalidInput(format!(
                    "WAL vessel URN mismatch: expected '{}', found '{}'",
                    vessel_urn, header_urn
                )));
            }

            let mut file = OpenOptions::new().read(true).write(true).open(&path)?;

            // Cleanly truncate away any torn record at the end
            file.set_len(valid_end_offset)?;
            file.seek(SeekFrom::End(0))?;

            let next_sequence = records.last().map(|(seq, _)| seq + 1).unwrap_or(1);

            let wal = Self {
                file,
                path,
                vessel_urn: vessel_urn.to_string(),
                next_sequence,
                last_sync: Instant::now(),
                has_unsynced: false,
                header_len,
            };

            Ok((wal, records))
        } else {
            let mut file = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&path)?;

            let header = ti_contracts::WalHeader {
                vessel_urn: vessel_urn.to_string(),
                version: FORMAT_VERSION,
            };
            let header_bytes = header.encode()?;
            file.write_all(&header_bytes)?;
            file.sync_all()?;

            let header_len = header_bytes.len() as u64;

            let wal = Self {
                file,
                path,
                vessel_urn: vessel_urn.to_string(),
                next_sequence: 1,
                last_sync: Instant::now(),
                has_unsynced: false,
                header_len,
            };

            Ok((wal, Vec::new()))
        }
    }

    /// Append a transaction of bucket records to the WAL.
    /// Returns the sequence number assigned to this frame.
    pub fn append(&mut self, records: &[BucketRecord]) -> Result<u64> {
        if records.is_empty() {
            return Err(Error::InvalidInput(
                "Cannot append empty records slice".into(),
            ));
        }

        let payload = bincode_options()
            .serialize(records)
            .map_err(|e| Error::InvalidInput(format!("bincode serialization error: {}", e)))?;

        let seq = self.next_sequence;
        let mut hasher = Hasher::new();
        hasher.update(&seq.to_le_bytes());
        hasher.update(&payload);
        let crc = hasher.finalize();

        let frame_header = ti_contracts::WalRecordHeader {
            payload_len: payload.len() as u32,
            sequence: seq,
            crc32: crc,
        };
        let frame_bytes = frame_header.encode()?;

        self.file.write_all(&frame_bytes)?;
        self.file.write_all(&payload)?;
        self.file.flush()?;

        self.has_unsynced = true;
        self.next_sequence += 1;
        Ok(seq)
    }

    /// Fsync the WAL to disk (group commit).
    pub fn sync(&mut self) -> Result<()> {
        if self.has_unsynced {
            self.file.flush()?;
            self.file.sync_data()?;
            self.has_unsynced = false;
        }
        self.last_sync = Instant::now();
        Ok(())
    }

    /// Check if group commit interval (>= 1s) has elapsed.
    pub fn needs_sync(&self) -> bool {
        self.has_unsynced && self.last_sync.elapsed().as_millis() >= 1000
    }

    /// Returns true if there are un-synced appends.
    pub fn has_unsynced(&self) -> bool {
        self.has_unsynced
    }

    /// Returns true if all appended records have been fsynced.
    pub fn is_synced(&self) -> bool {
        !self.has_unsynced
    }

    /// Instant of last sync.
    pub fn last_sync(&self) -> Instant {
        self.last_sync
    }

    /// Truncate the WAL to the header point after a successful flush.
    pub fn truncate_after_flush(&mut self) -> Result<()> {
        self.file.flush()?;
        self.file.set_len(self.header_len)?;
        self.file.seek(SeekFrom::Start(self.header_len))?;
        self.file.sync_all()?;
        self.next_sequence = 1;
        self.has_unsynced = false;
        self.last_sync = Instant::now();
        Ok(())
    }

    /// Path to this WAL file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Canonical vessel URN.
    pub fn vessel_urn(&self) -> &str {
        &self.vessel_urn
    }

    /// Replay the WAL file from disk.
    /// Returns (vessel_urn, header_len, valid_records, valid_end_offset).
    /// Stops cleanly at the first torn or CRC-bad record.
    pub fn recover(path: &Path) -> Result<WalRecoveryResult> {
        let mut file = File::open(path)?;
        let mut magic = [0u8; 8];
        if file.read_exact(&mut magic).is_err() || magic != WAL_MAGIC {
            return Err(Error::Corrupt("Invalid WAL magic".into()));
        }

        let mut ver_bytes = [0u8; 2];
        file.read_exact(&mut ver_bytes)?;
        let version = u16::from_le_bytes(ver_bytes);
        if version != FORMAT_VERSION {
            return Err(Error::Unsupported(format!("WAL version {}", version)));
        }

        let mut urn_len_bytes = [0u8; 4];
        file.read_exact(&mut urn_len_bytes)?;
        let urn_len = u32::from_le_bytes(urn_len_bytes) as usize;
        let mut urn_bytes = vec![0u8; urn_len];
        file.read_exact(&mut urn_bytes)?;
        let vessel_urn = String::from_utf8(urn_bytes)
            .map_err(|_| Error::Corrupt("Invalid UTF-8 in vessel URN".into()))?;

        let header_len = 8 + 2 + 4 + (urn_len as u64);
        let mut valid_end_offset = header_len;
        let mut records = Vec::new();
        let mut expected_seq = 1u64;

        loop {
            let mut frame_bytes = [0u8; 16];
            match file.read_exact(&mut frame_bytes) {
                Ok(()) => {}
                Err(ref e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                    // Clean EOF between records
                    break;
                }
                Err(e) => return Err(Error::Io(e)),
            }

            let payload_len = u32::from_le_bytes(frame_bytes[..4].try_into().unwrap()) as usize;
            let sequence = u64::from_le_bytes(frame_bytes[4..12].try_into().unwrap());
            let stored_crc = u32::from_le_bytes(frame_bytes[12..16].try_into().unwrap());

            if sequence != expected_seq {
                // Sequence discontinuity -> stop cleanly
                break;
            }

            let mut payload = vec![0u8; payload_len];
            match file.read_exact(&mut payload) {
                Ok(()) => {}
                Err(_) => {
                    // Torn record payload -> stop cleanly
                    break;
                }
            }

            let mut hasher = Hasher::new();
            hasher.update(&sequence.to_le_bytes());
            hasher.update(&payload);
            let calculated_crc = hasher.finalize();

            if stored_crc != calculated_crc {
                // CRC mismatch -> torn/corrupted record -> stop cleanly
                break;
            }

            let batch: Vec<BucketRecord> = match bincode_options().deserialize(&payload) {
                Ok(b) => b,
                Err(_) => {
                    // Deserialization failure -> stop cleanly
                    break;
                }
            };

            records.push((sequence, batch));
            expected_seq += 1;
            valid_end_offset += 16 + (payload_len as u64);
        }

        Ok((vessel_urn, header_len, records, valid_end_offset))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use ti_contracts::FieldValue;

    fn make_test_record(vessel: VesselOrd, bucket: u32, field: u32, val: i64) -> BucketRecord {
        BucketRecord {
            vessel,
            bucket,
            field,
            value: FieldValue::Int(val),
            rewrite: false,
        }
    }

    #[test]
    fn wal_create_append_replay_and_truncate() {
        let dir = tempdir().unwrap();
        let urn = "vessels.urn:mrn:signalk:uuid:test-vessel";

        let (mut wal, initial_records) = Wal::open_or_create(dir.path(), 0, urn).unwrap();
        assert!(initial_records.is_empty());

        let rec1 = vec![make_test_record(0, 100, 1, 42)];
        let seq1 = wal.append(&rec1).unwrap();
        assert_eq!(seq1, 1);

        let rec2 = vec![
            make_test_record(0, 101, 1, 43),
            make_test_record(0, 101, 2, 99),
        ];
        let seq2 = wal.append(&rec2).unwrap();
        assert_eq!(seq2, 2);

        wal.sync().unwrap();
        drop(wal);

        // Re-open and verify replay
        let (mut wal2, replayed) = Wal::open_or_create(dir.path(), 0, urn).unwrap();
        assert_eq!(replayed.len(), 2);
        assert_eq!(replayed[0].0, 1);
        assert_eq!(replayed[0].1, rec1);
        assert_eq!(replayed[1].0, 2);
        assert_eq!(replayed[1].1, rec2);

        // Truncate after flush
        wal2.truncate_after_flush().unwrap();
        drop(wal2);

        // Re-open after truncate
        let (_wal3, replayed_after_trunc) = Wal::open_or_create(dir.path(), 0, urn).unwrap();
        assert!(replayed_after_trunc.is_empty());
    }

    #[test]
    fn wal_torn_record_recovery() {
        let dir = tempdir().unwrap();
        let urn = "vessels.urn:mrn:signalk:uuid:test-vessel";

        let (mut wal, _) = Wal::open_or_create(dir.path(), 0, urn).unwrap();
        let rec1 = vec![make_test_record(0, 100, 1, 42)];
        wal.append(&rec1).unwrap();
        wal.sync().unwrap();

        // Write a second record but corrupt its payload bytes (simulating torn write)
        let rec2 = vec![make_test_record(0, 101, 1, 43)];
        wal.append(&rec2).unwrap();
        wal.sync().unwrap();
        let path = wal.path().to_path_buf();
        drop(wal);

        // Corrupt the last 2 bytes of the file
        let len = std::fs::metadata(&path).unwrap().len();
        let file = OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len(len - 2).unwrap();
        drop(file);

        // Recover: should recover rec1 cleanly and discard torn rec2
        let (_wal2, replayed) = Wal::open_or_create(dir.path(), 0, urn).unwrap();
        assert_eq!(replayed.len(), 1);
        assert_eq!(replayed[0].0, 1);
        assert_eq!(replayed[0].1, rec1);
    }
}
