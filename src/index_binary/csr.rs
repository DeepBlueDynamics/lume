//! Flat per-document TF rows. Term IDs refer to the shared lexical dictionary.
//! Wire payload: term_count:u32, entry_count:u64, (row_count+1) u64 offsets,
//! then row-local term-ID deltas and exact u64 TFs as canonical unsigned varints.
use super::codec::{Reader, Writer};

const FORWARD_KIND: u16 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ForwardEntry {
    pub term: u32,
    pub tf: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardCsr {
    term_count: u32,
    offsets: Vec<u64>,
    entries: Vec<ForwardEntry>,
}
impl ForwardCsr {
    pub fn new(
        term_count: u32,
        offsets: Vec<u64>,
        entries: Vec<ForwardEntry>,
    ) -> Result<Self, String> {
        if offsets.first() != Some(&0)
            || offsets.last().copied() != Some(entries.len() as u64)
            || offsets.len() - 1 > u32::MAX as usize
        {
            return Err("Invalid v4 forward row offsets".into());
        }
        for pair in offsets.windows(2) {
            if pair[0] > pair[1] {
                return Err("Unsorted v4 forward row offsets".into());
            }
            let start = usize::try_from(pair[0]).map_err(|_| "Forward offset exceeds usize")?;
            let end = usize::try_from(pair[1]).map_err(|_| "Forward offset exceeds usize")?;
            let row = entries
                .get(start..end)
                .ok_or("Forward row exceeds entries")?;
            let mut previous = None;
            for entry in row {
                if entry.term >= term_count
                    || entry.tf == 0
                    || previous.is_some_and(|term| entry.term <= term)
                {
                    return Err("Invalid or unsorted v4 forward entry".into());
                }
                previous = Some(entry.term);
            }
        }
        Ok(Self {
            term_count,
            offsets,
            entries,
        })
    }
    pub fn term_count(&self) -> u32 {
        self.term_count
    }
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }
    pub fn len(&self) -> usize {
        self.offsets.len() - 1
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn row(&self, doc: usize) -> Option<&[ForwardEntry]> {
        let start = usize::try_from(*self.offsets.get(doc)?).ok()?;
        let end = usize::try_from(*self.offsets.get(doc.checked_add(1)?)?).ok()?;
        self.entries.get(start..end)
    }
    pub fn get(&self, doc: usize, term: u32) -> u64 {
        let Some(row) = self.row(doc) else { return 0 };
        row.binary_search_by_key(&term, |entry| entry.term)
            .map(|index| row[index].tf)
            .unwrap_or(0)
    }
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        let mut writer = Writer::new(
            FORWARD_KIND,
            u32::try_from(self.len()).map_err(|_| "Forward rows exceed u32")?,
            0,
        );
        writer.u32(self.term_count);
        writer.u64(self.entries.len() as u64);
        for offset in &self.offsets {
            writer.u64(*offset);
        }
        for doc in 0..self.len() {
            let mut previous = 0;
            for entry in self.row(doc).unwrap() {
                writer.varint(u64::from(entry.term - previous));
                writer.varint(entry.tf);
                previous = entry.term;
            }
        }
        writer.finish()
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        let mut reader = Reader::new(bytes, FORWARD_KIND)?;
        if reader.record_bytes != 0 {
            return Err("Forward rows require variable payload".into());
        }
        let rows = reader.count as usize;
        let term_count = reader.u32()?;
        let entry_count =
            usize::try_from(reader.u64()?).map_err(|_| "Forward count exceeds usize")?;
        let offset_count = rows.checked_add(1).ok_or("Forward offset count overflow")?;
        let offset_bytes = offset_count
            .checked_mul(8)
            .ok_or("Forward table size overflow")?;
        if offset_bytes > reader.remaining() {
            return Err("Truncated forward offsets".into());
        }
        let mut offsets = Vec::new();
        offsets
            .try_reserve_exact(offset_count)
            .map_err(|_| "Cannot allocate forward offsets")?;
        for _ in 0..offset_count {
            offsets.push(reader.u64()?);
        }
        if offsets.first() != Some(&0)
            || offsets.last().copied() != Some(entry_count as u64)
            || offsets.windows(2).any(|pair| pair[0] > pair[1])
        {
            return Err("Invalid forward offsets".into());
        }
        if entry_count > reader.remaining() / 2 {
            return Err("Forward count exceeds available entries".into());
        }
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(entry_count)
            .map_err(|_| "Cannot allocate forward entries")?;
        for pair in offsets.windows(2) {
            let mut previous = 0_u32;
            for entry_index in pair[0]..pair[1] {
                let delta =
                    u32::try_from(reader.varint()?).map_err(|_| "Forward term exceeds u32")?;
                let term = previous.checked_add(delta).ok_or("Forward term overflow")?;
                if entry_index > pair[0] && delta == 0 {
                    return Err("Duplicate forward term".into());
                }
                let tf = reader.varint()?;
                if term >= term_count || tf == 0 {
                    return Err("Invalid forward term or TF".into());
                }
                entries.push(ForwardEntry { term, tf });
                previous = term;
            }
        }
        reader.finish()?;
        Self::new(term_count, offsets, entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_rows_and_full_width_tfs_round_trip() {
        let rows = ForwardCsr::new(
            100,
            vec![0, 2, 2, 3],
            vec![
                ForwardEntry { term: 0, tf: 1 },
                ForwardEntry {
                    term: 99,
                    tf: u64::MAX,
                },
                ForwardEntry { term: 5, tf: 65536 },
            ],
        )
        .unwrap();
        let bytes = rows.encode().unwrap();
        let decoded = ForwardCsr::decode(&bytes).unwrap();
        assert_eq!(decoded, rows);
        assert_eq!(decoded.get(0, 99), u64::MAX);
        assert_eq!(decoded.get(1, 99), 0);
        assert_eq!(decoded.get(3, 5), 0);
        assert_eq!(decoded.row(1), Some([].as_slice()));
        assert_eq!(decoded.len(), 3);
        let empty = ForwardCsr::new(0, vec![0], Vec::new()).unwrap();
        assert!(ForwardCsr::decode(&empty.encode().unwrap())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn invalid_rows_and_truncated_inputs_are_rejected() {
        for offsets in [vec![], vec![1], vec![0, 2], vec![0, 1, 0, 1]] {
            assert!(ForwardCsr::new(1, offsets, vec![ForwardEntry { term: 0, tf: 1 }]).is_err());
        }
        for entries in [
            vec![ForwardEntry { term: 1, tf: 1 }],
            vec![ForwardEntry { term: 0, tf: 0 }],
            vec![
                ForwardEntry { term: 0, tf: 1 },
                ForwardEntry { term: 0, tf: 2 },
            ],
        ] {
            assert!(ForwardCsr::new(1, vec![0, entries.len() as u64], entries).is_err());
        }
        let rows = ForwardCsr::new(1, vec![0, 1], vec![ForwardEntry { term: 0, tf: 1 }]).unwrap();
        let bytes = rows.encode().unwrap();
        for length in 0..bytes.len() {
            assert!(ForwardCsr::decode(&bytes[..length]).is_err());
        }
        for offset in [44, 52, 60, 68, 69] {
            let mut bad = bytes.clone();
            bad[offset] = 0xff;
            assert!(ForwardCsr::decode(&bad).is_err(), "{offset}");
        }
    }
}
