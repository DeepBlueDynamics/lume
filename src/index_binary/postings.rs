//! Flat term-major postings with inline tiny lists. No allocation per rare term.
use super::codec::{Reader, Writer};

const POSTINGS_KIND: u16 = 3;
const INLINE_LIMIT: usize = 4;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Posting {
    pub doc: u32,
    pub title_tf: u64,
    pub body_tf: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TermRange {
    Inline {
        length: u8,
        values: [Posting; INLINE_LIMIT],
    },
    Flat {
        start: usize,
        end: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostingsCsr {
    doc_count: u32,
    terms: Vec<TermRange>,
    entries: Vec<Posting>,
}

impl PostingsCsr {
    pub fn new(doc_count: u32, rows: Vec<Vec<Posting>>) -> Result<Self, String> {
        if rows.len() > u32::MAX as usize {
            return Err("V4 term count exceeds u32".into());
        }
        let mut terms = Vec::with_capacity(rows.len());
        let mut entries = Vec::new();
        for row in rows {
            Self::validate_row(doc_count, &row)?;
            if row.len() <= INLINE_LIMIT {
                let mut values = [Posting::default(); INLINE_LIMIT];
                values[..row.len()].copy_from_slice(&row);
                terms.push(TermRange::Inline {
                    length: row.len() as u8,
                    values,
                });
            } else {
                let start = entries.len();
                entries.extend(row);
                terms.push(TermRange::Flat {
                    start,
                    end: entries.len(),
                });
            }
        }
        Ok(Self {
            doc_count,
            terms,
            entries,
        })
    }

    fn validate_row(doc_count: u32, row: &[Posting]) -> Result<(), String> {
        let mut previous = None;
        for posting in row {
            if posting.doc >= doc_count
                || (posting.title_tf == 0 && posting.body_tf == 0)
                || previous.is_some_and(|doc| posting.doc <= doc)
            {
                return Err("Invalid or unsorted v4 posting".into());
            }
            previous = Some(posting.doc);
        }
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.terms.len()
    }
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }
    pub fn row(&self, term: u32) -> Option<&[Posting]> {
        match self.terms.get(term as usize)? {
            TermRange::Inline { length, values } => Some(&values[..usize::from(*length)]),
            TermRange::Flat { start, end } => self.entries.get(*start..*end),
        }
    }

    pub fn encode(&self) -> Result<Vec<u8>, String> {
        let mut writer = Writer::new(POSTINGS_KIND, self.terms.len() as u32, 0);
        writer.u32(self.doc_count);
        for term in 0..self.terms.len() {
            let row = self.row(term as u32).unwrap();
            writer.varint(row.len() as u64);
            let mut previous = 0;
            for posting in row {
                writer.varint(u64::from(posting.doc - previous));
                writer.varint(posting.title_tf);
                writer.varint(posting.body_tf);
                previous = posting.doc;
            }
        }
        writer.finish()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        let mut reader = Reader::new(bytes, POSTINGS_KIND)?;
        if reader.record_bytes != 0 {
            return Err("Postings require a variable payload".into());
        }
        let doc_count = reader.u32()?;
        if reader.count as usize > reader.remaining() {
            return Err("Postings term count exceeds payload".into());
        }
        let mut result = Self {
            doc_count,
            terms: Vec::new(),
            entries: Vec::new(),
        };
        result
            .terms
            .try_reserve_exact(reader.count as usize)
            .map_err(|_| "Cannot allocate postings terms")?;
        for _ in 0..reader.count {
            let count =
                usize::try_from(reader.varint()?).map_err(|_| "Posting count exceeds usize")?;
            if count > reader.remaining() / 3 || count > doc_count as usize {
                return Err("Posting count exceeds available payload or documents".into());
            }
            let start = result.entries.len();
            let mut values = [Posting::default(); INLINE_LIMIT];
            if count > INLINE_LIMIT {
                result
                    .entries
                    .try_reserve(count)
                    .map_err(|_| "Cannot allocate postings")?;
            }
            let mut previous = 0_u32;
            for index in 0..count {
                let delta =
                    u32::try_from(reader.varint()?).map_err(|_| "Posting delta exceeds u32")?;
                let doc = previous
                    .checked_add(delta)
                    .ok_or("Posting document overflow")?;
                let posting = Posting {
                    doc,
                    title_tf: reader.varint()?,
                    body_tf: reader.varint()?,
                };
                if doc >= doc_count
                    || (index > 0 && delta == 0)
                    || (posting.title_tf == 0 && posting.body_tf == 0)
                {
                    return Err("Invalid v4 posting".into());
                }
                if count <= INLINE_LIMIT {
                    values[index] = posting;
                } else {
                    result.entries.push(posting);
                }
                previous = doc;
            }
            result.terms.push(if count <= INLINE_LIMIT {
                TermRange::Inline {
                    length: count as u8,
                    values,
                }
            } else {
                TermRange::Flat {
                    start,
                    end: result.entries.len(),
                }
            });
        }
        reader.finish()?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_and_flat_rows_preserve_exact_tfs_and_document_boundaries() {
        let rows: Vec<_> = [0, 1, 4, 5, 129]
            .into_iter()
            .map(|count| {
                (0..count)
                    .map(|i| Posting {
                        doc: if i == count - 1 { 65536 } else { i },
                        title_tf: if i % 2 == 0 { u64::MAX } else { 0 },
                        body_tf: u64::from(i) + 1,
                    })
                    .collect()
            })
            .collect();
        let postings = PostingsCsr::new(65537, rows.clone()).unwrap();
        let decoded = PostingsCsr::decode(&postings.encode().unwrap()).unwrap();
        assert_eq!(decoded, postings);
        assert_eq!(decoded.len(), rows.len());
        assert!(!decoded.is_empty());
        for (term, row) in rows.iter().enumerate() {
            assert_eq!(decoded.row(term as u32), Some(row.as_slice()));
        }
        assert!(decoded.row(5).is_none());
        assert_eq!(decoded.entries.len(), 5 + 129);
        assert!(
            PostingsCsr::decode(&PostingsCsr::new(0, Vec::new()).unwrap().encode().unwrap())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn corrupt_postings_fail_closed() {
        for row in [
            vec![Posting {
                doc: 1,
                title_tf: 1,
                body_tf: 0,
            }],
            vec![Posting::default()],
            vec![
                Posting {
                    doc: 0,
                    title_tf: 1,
                    body_tf: 0
                };
                2
            ],
        ] {
            assert!(PostingsCsr::new(1, vec![row]).is_err());
        }
        let bytes = PostingsCsr::new(
            1,
            vec![vec![Posting {
                doc: 0,
                title_tf: 1,
                body_tf: 0,
            }]],
        )
        .unwrap()
        .encode()
        .unwrap();
        for length in 0..bytes.len() {
            assert!(PostingsCsr::decode(&bytes[..length]).is_err());
        }
        for offset in [44, 45, 46, 47] {
            let mut bad = bytes.clone();
            bad[offset] = 0xff;
            assert!(PostingsCsr::decode(&bad).is_err(), "{offset}");
        }
        let mut zero_tf = bytes.clone();
        zero_tf[46] = 0;
        assert!(PostingsCsr::decode(&zero_tf).is_err());
    }
}
