//! Write-Ahead Log (WAL) per vessel for ti-store.
//!
//! Envelopes and framing follow plan/spec/14-semantics.md §gap 6:
//! - Segment Header: `WAL_MAGIC` (LUMETIW1), version u16 LE, URN length u32 LE, URN bytes.
//! - Record Frame: payload_len u32 LE, sequence u64 LE, IEEE CRC32 u32 LE, payload bytes.
//! - D53: sequence starts at 1 for a new store and stays monotonic across truncation/restart.
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

/// OTLP metrics frame. Vessel ingest never writes this magic.
pub const OTLP_COUNTER_MAGIC: &[u8; 8] = b"LUMEOC01";
const MAX_OTLP_COUNTER_BYTES: usize = 8 * 1024 * 1024;

struct ScannedFrame {
    sequence: u64,
    records: Vec<BucketRecord>,
    counters: Option<Vec<u8>>,
}

struct WalScan {
    vessel_urn: String,
    header_len: u64,
    frames: Vec<ScannedFrame>,
    valid_end_offset: u64,
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

            let recovered_next = match records.last() {
                Some((seq, _)) => seq.checked_add(1).ok_or(Error::Overflow("WAL sequence"))?,
                None => 1,
            };
            let floor = read_sequence_floor(&path)?;
            let next_sequence = recovered_next.max(floor);
            if floor > recovered_next {
                // The floor acknowledges a complete flush. A shortened old tail
                // must not precede a new higher sequence and create a replay gap.
                file.set_len(header_len)?;
                file.seek(SeekFrom::Start(header_len))?;
                file.sync_all()?;
            }

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
            #[cfg(unix)]
            File::open(wal_dir)?.sync_all()?;

            let header_len = header_bytes.len() as u64;

            let wal = Self {
                file,
                next_sequence: read_sequence_floor(&path)?,
                path,
                vessel_urn: vessel_urn.to_string(),
                last_sync: Instant::now(),
                has_unsynced: false,
                header_len,
            };

            Ok((wal, Vec::new()))
        }
    }

    /// Append a transaction of bucket records to the WAL.
    /// Returns the sequence number assigned to this frame.
    ///
    /// The bytes are the legacy bincode payload. Vessel ingest uses this path
    /// and must stay free of `LUMEOC01`.
    pub fn append(&mut self, records: &[BucketRecord]) -> Result<u64> {
        if records.is_empty() {
            return Err(Error::InvalidInput(
                "Cannot append empty records slice".into(),
            ));
        }

        let payload = bincode_options()
            .serialize(records)
            .map_err(|e| Error::InvalidInput(format!("bincode serialization error: {}", e)))?;
        self.write_payload(&payload)
    }

    /// Append one OTLP metrics frame: `LUMEOC01`, the counter JSON length, the
    /// JSON, then the same bincode `Vec<BucketRecord>` vessel frames use.
    /// An empty record list is allowed when the counter JSON is present.
    pub fn append_otlp(&mut self, records: &[BucketRecord], counters: &[u8]) -> Result<u64> {
        if records.is_empty() && counters.is_empty() {
            return Err(Error::InvalidInput(
                "Cannot append an empty OTLP WAL frame".into(),
            ));
        }
        if counters.len() > MAX_OTLP_COUNTER_BYTES {
            return Err(Error::InvalidInput(
                "OTLP WAL counter section exceeds 8 MiB".into(),
            ));
        }
        let body = bincode_options()
            .serialize(records)
            .map_err(|e| Error::InvalidInput(format!("bincode serialization error: {}", e)))?;
        let counters_len =
            u32::try_from(counters.len()).map_err(|_| Error::Overflow("OTLP counter length"))?;
        let mut payload = Vec::with_capacity(12 + counters.len() + body.len());
        payload.extend_from_slice(OTLP_COUNTER_MAGIC);
        payload.extend_from_slice(&counters_len.to_le_bytes());
        payload.extend_from_slice(counters);
        payload.extend_from_slice(&body);
        self.write_payload(&payload)
    }

    fn write_payload(&mut self, payload: &[u8]) -> Result<u64> {
        let payload_len =
            u32::try_from(payload.len()).map_err(|_| Error::Overflow("WAL payload"))?;
        let seq = self.next_sequence;
        let next = seq.checked_add(1).ok_or(Error::Overflow("WAL sequence"))?;
        let mut hasher = Hasher::new();
        hasher.update(&seq.to_le_bytes());
        hasher.update(payload);
        let crc = hasher.finalize();

        let frame_header = ti_contracts::WalRecordHeader {
            payload_len,
            sequence: seq,
            crc32: crc,
        };
        let frame_bytes = frame_header.encode()?;

        self.file.write_all(&frame_bytes)?;
        self.file.write_all(payload)?;
        self.file.flush()?;

        self.has_unsynced = true;
        self.next_sequence = next;
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
        write_sequence_floor(&self.path, self.next_sequence)?;
        self.file.set_len(self.header_len)?;
        self.file.seek(SeekFrom::Start(self.header_len))?;
        self.file.sync_all()?;
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
    /// `LUMEOC01` frames decode to their bucket records. A magic prefix with a
    /// bad counter section or body stops replay, the same as a bad bincode frame.
    pub fn recover(path: &Path) -> Result<WalRecoveryResult> {
        let scan = scan_wal(path)?;
        let records = scan
            .frames
            .into_iter()
            .map(|frame| (frame.sequence, frame.records))
            .collect();
        Ok((
            scan.vessel_urn,
            scan.header_len,
            records,
            scan.valid_end_offset,
        ))
    }

    /// Counter JSON from each valid `LUMEOC01` frame, in sequence order.
    /// Legacy frames contribute nothing. Stops at the first torn frame.
    pub fn read_otlp_counters(path: &Path) -> Result<Vec<(u64, Vec<u8>)>> {
        let scan = scan_wal(path)?;
        Ok(scan
            .frames
            .into_iter()
            .filter_map(|frame| frame.counters.map(|counters| (frame.sequence, counters)))
            .collect())
    }
}

fn decode_frame_payload(payload: &[u8]) -> Option<(Vec<BucketRecord>, Option<Vec<u8>>)> {
    if payload.starts_with(OTLP_COUNTER_MAGIC) {
        if payload.len() < 12 {
            return None;
        }
        let counters_len = u32::from_le_bytes(payload[8..12].try_into().ok()?) as usize;
        if counters_len > MAX_OTLP_COUNTER_BYTES || payload.len() < 12 + counters_len {
            return None;
        }
        let counters = payload[12..12 + counters_len].to_vec();
        let records = bincode_options()
            .deserialize(&payload[12 + counters_len..])
            .ok()?;
        Some((records, Some(counters)))
    } else {
        let records = bincode_options().deserialize(payload).ok()?;
        Some((records, None))
    }
}

fn scan_wal(path: &Path) -> Result<WalScan> {
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
    let mut frames = Vec::new();
    let mut expected_seq = None;

    loop {
        let mut frame_bytes = [0u8; 16];
        match file.read_exact(&mut frame_bytes) {
            Ok(()) => {}
            Err(ref e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(Error::Io(e)),
        }

        let payload_len = u32::from_le_bytes(frame_bytes[..4].try_into().unwrap()) as usize;
        let sequence = u64::from_le_bytes(frame_bytes[4..12].try_into().unwrap());
        let stored_crc = u32::from_le_bytes(frame_bytes[12..16].try_into().unwrap());

        if sequence == 0 || expected_seq.is_some_and(|expected| sequence != expected) {
            break;
        }

        let mut payload = vec![0u8; payload_len];
        if file.read_exact(&mut payload).is_err() {
            break;
        }

        let mut hasher = Hasher::new();
        hasher.update(&sequence.to_le_bytes());
        hasher.update(&payload);
        if stored_crc != hasher.finalize() {
            break;
        }

        let Some((records, counters)) = decode_frame_payload(&payload) else {
            break;
        };

        frames.push(ScannedFrame {
            sequence,
            records,
            counters,
        });
        expected_seq = Some(
            sequence
                .checked_add(1)
                .ok_or(Error::Overflow("WAL sequence"))?,
        );
        valid_end_offset += 16 + (payload_len as u64);
    }

    Ok(WalScan {
        vessel_urn,
        header_len,
        frames,
        valid_end_offset,
    })
}

// The floor is published before truncation, so a crash cannot reuse a checkpointed sequence.
const SEQUENCE_MAGIC: &[u8; 8] = b"LUMESEQ1";

fn read_sequence_floor(path: &Path) -> Result<u64> {
    let path = path.with_extension("seq");
    let mut file = match File::open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(1),
        Err(e) => return Err(e.into()),
    };
    let mut bytes = Vec::new();
    (&mut file).take(21).read_to_end(&mut bytes)?;
    if bytes.len() != 20
        || &bytes[..8] != SEQUENCE_MAGIC
        || crc32fast::hash(&bytes[..16]) != u32::from_le_bytes(bytes[16..20].try_into().unwrap())
    {
        return Err(Error::Corrupt("invalid WAL sequence floor".into()));
    }
    let sequence = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
    if sequence == 0 {
        return Err(Error::Corrupt("zero WAL sequence floor".into()));
    }
    Ok(sequence)
}

fn write_sequence_floor(path: &Path, sequence: u64) -> Result<()> {
    let target = path.with_extension("seq");
    let tmp = path.with_extension(format!("seq.tmp.{}", std::process::id()));
    let mut bytes = SEQUENCE_MAGIC.to_vec();
    bytes.extend_from_slice(&sequence.to_le_bytes());
    bytes.extend_from_slice(&crc32fast::hash(&bytes).to_le_bytes());
    let mut file = File::create(&tmp)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(tmp, target)?;
    #[cfg(unix)]
    File::open(
        path.parent()
            .ok_or_else(|| Error::InvalidInput("WAL has no parent".into()))?,
    )?
    .sync_all()?;
    Ok(())
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
    fn floor_publication_before_truncate_survives_restart() {
        let dir = tempdir().unwrap();
        let (mut wal, _) = Wal::open_or_create(dir.path(), 0, "vessels.urn:floor").unwrap();
        for value in [1, 2] {
            wal.append(&[make_test_record(0, 3, 0, value)]).unwrap();
        }
        wal.sync().unwrap();
        // Crash after the atomic floor publication but before truncation.
        write_sequence_floor(wal.path(), wal.next_sequence).unwrap();
        drop(wal);
        let (mut wal, records) = Wal::open_or_create(dir.path(), 0, "vessels.urn:floor").unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(wal.append(&[make_test_record(0, 3, 0, 3)]).unwrap(), 3);
    }

    #[test]
    fn floor_ahead_of_shortened_tail_cannot_create_a_replay_gap() {
        let dir = tempdir().unwrap();
        let (mut wal, _) = Wal::open_or_create(dir.path(), 0, "vessels.urn:floor").unwrap();
        wal.append(&[make_test_record(0, 3, 0, 1)]).unwrap();
        wal.sync().unwrap();
        // Durable floor from a completed flush, with only an earlier old frame
        // left in the WAL after interrupted truncation.
        write_sequence_floor(wal.path(), 10).unwrap();
        drop(wal);
        let (mut wal, records) = Wal::open_or_create(dir.path(), 0, "vessels.urn:floor").unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(wal.append(&[make_test_record(0, 4, 0, 2)]).unwrap(), 10);
        wal.sync().unwrap();
        drop(wal);
        let (_, records) = Wal::open_or_create(dir.path(), 0, "vessels.urn:floor").unwrap();
        assert_eq!(
            records.iter().map(|(seq, _)| *seq).collect::<Vec<_>>(),
            vec![10]
        );
    }

    #[test]
    fn orphan_floor_temp_is_ignored_and_exhaustion_is_explicit() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("0.seq.tmp.abandoned"), b"torn").unwrap();
        let (mut wal, _) = Wal::open_or_create(dir.path(), 0, "vessels.urn:floor").unwrap();
        assert_eq!(wal.append(&[make_test_record(0, 3, 0, 1)]).unwrap(), 1);
        wal.next_sequence = u64::MAX;
        assert!(matches!(
            wal.append(&[make_test_record(0, 3, 0, 2)]),
            Err(Error::Overflow(_))
        ));
        assert_eq!(Wal::recover(wal.path()).unwrap().2.len(), 1);
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

    fn legacy_frame(urn: &str, seq: u64, records: &[BucketRecord]) -> Vec<u8> {
        let header = ti_contracts::WalHeader {
            vessel_urn: urn.to_string(),
            version: ti_contracts::FORMAT_VERSION,
        }
        .encode()
        .unwrap();
        let payload = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_little_endian()
            .serialize(records)
            .unwrap();
        let mut hasher = Hasher::new();
        hasher.update(&seq.to_le_bytes());
        hasher.update(&payload);
        let frame = ti_contracts::WalRecordHeader {
            payload_len: payload.len() as u32,
            sequence: seq,
            crc32: hasher.finalize(),
        }
        .encode()
        .unwrap();
        let mut bytes = header;
        bytes.extend_from_slice(&frame);
        bytes.extend_from_slice(&payload);
        bytes
    }

    #[test]
    fn legacy_append_matches_a_hand_rolled_frame_and_has_no_otlp_magic() {
        let dir = tempdir().unwrap();
        let urn = "vessels.urn:mrn:signalk:uuid:byte-identity";
        let records = vec![
            make_test_record(0, 100, 1, 42),
            make_test_record(0, 101, 2, -7),
        ];
        let (mut wal, _) = Wal::open_or_create(dir.path(), 0, urn).unwrap();
        wal.append(&records).unwrap();
        wal.sync().unwrap();
        let bytes = std::fs::read(wal.path()).unwrap();
        assert_eq!(bytes, legacy_frame(urn, 1, &records));
        assert!(!bytes.windows(8).any(|window| window == b"LUMEOC01"));
        assert!(wal.is_synced());
    }

    #[test]
    fn otlp_frame_round_trips_records_and_counters_and_a_bad_body_stops() {
        let dir = tempdir().unwrap();
        let urn = "vessels.urn:mrn:signalk:uuid:otlp-frame";
        let legacy = vec![make_test_record(0, 3, 0, 1)];
        let otlp = vec![make_test_record(0, 4, 0, 2)];
        let counters = br#"{"version":1,"counters":[]}"#;
        let (mut wal, _) = Wal::open_or_create(dir.path(), 0, urn).unwrap();
        wal.append(&legacy).unwrap();
        wal.append_otlp(&otlp, counters).unwrap();
        wal.sync().unwrap();
        let path = wal.path().to_path_buf();
        drop(wal);

        let (_wal, replayed) = Wal::open_or_create(dir.path(), 0, urn).unwrap();
        assert_eq!(replayed.len(), 2);
        assert_eq!(replayed[0].1, legacy);
        assert_eq!(replayed[1].1, otlp);
        assert_eq!(
            Wal::read_otlp_counters(&path).unwrap(),
            vec![(2, counters.to_vec())]
        );
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.windows(8).any(|window| window == b"LUMEOC01"));

        // A magic prefix whose body is not bincode stops replay and is truncated.
        let (mut wal, _) = Wal::open_or_create(dir.path(), 0, urn).unwrap();
        wal.append_otlp(&otlp, counters).unwrap();
        wal.sync().unwrap();
        let path = wal.path().to_path_buf();
        let len = std::fs::metadata(&path).unwrap().len();
        drop(wal);
        let file = OpenOptions::new().write(true).open(&path).unwrap();
        // Keep the magic and length, drop the bincode body.
        file.set_len(len - 4).unwrap();
        drop(file);
        let (_wal, replayed) = Wal::open_or_create(dir.path(), 0, urn).unwrap();
        assert_eq!(replayed.len(), 2);
        assert_eq!(Wal::read_otlp_counters(&path).unwrap().len(), 1);
    }
}
