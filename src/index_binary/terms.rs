//! Lexically ordered term IDs and field DFs. Text and descriptors are separate.
use super::codec::{Reader, Writer};

const TERMS_KIND: u16 = 6;
const TERM_TEXT_KIND: u16 = 7;
const RECORD_BYTES: u32 = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Term {
    pub word: Vec<u8>,
    pub title_df: u64,
    pub body_df: u64,
}

pub fn encode(terms: &[Term], doc_count: u32) -> Result<(Vec<u8>, Vec<u8>), String> {
    validate(terms, doc_count)?;
    let mut records = Writer::new(
        TERMS_KIND,
        u32::try_from(terms.len()).map_err(|_| "Term count exceeds u32")?,
        RECORD_BYTES,
    );
    let mut text = Writer::new(TERM_TEXT_KIND, 0, 0);
    let mut offset = 0_u64;
    for term in terms {
        records.u64(offset);
        records.u64(term.word.len() as u64);
        records.u64(term.title_df);
        records.u64(term.body_df);
        text.raw(&term.word);
        offset = offset
            .checked_add(term.word.len() as u64)
            .ok_or("Term text overflow")?;
    }
    Ok((records.finish()?, text.finish()?))
}

pub fn decode(records: &[u8], text: &[u8], doc_count: u32) -> Result<Vec<Term>, String> {
    let mut table = Reader::new(records, TERMS_KIND)?;
    if table.record_bytes != RECORD_BYTES {
        return Err("Invalid term record width".into());
    }
    let mut blob = Reader::new(text, TERM_TEXT_KIND)?;
    if blob.count != 0 || blob.record_bytes != 0 {
        return Err("Invalid term text framing".into());
    }
    let bytes = blob.raw(blob.remaining())?;
    blob.finish()?;
    let mut terms = Vec::new();
    terms
        .try_reserve_exact(table.count as usize)
        .map_err(|_| "Cannot allocate terms")?;
    let mut previous_end = 0;
    for _ in 0..table.count {
        let offset = usize::try_from(table.u64()?).map_err(|_| "Term offset exceeds usize")?;
        let length = usize::try_from(table.u64()?).map_err(|_| "Term length exceeds usize")?;
        if offset != previous_end {
            return Err("Noncontiguous term text offsets".into());
        }
        let end = offset.checked_add(length).ok_or("Term range overflow")?;
        let word = bytes
            .get(offset..end)
            .ok_or("Term range exceeds text")?
            .to_vec();
        let title_df = table.u64()?;
        let body_df = table.u64()?;
        terms.push(Term {
            word,
            title_df,
            body_df,
        });
        previous_end = end;
    }
    if previous_end != bytes.len() {
        return Err("Trailing term text".into());
    }
    table.finish()?;
    validate(&terms, doc_count)?;
    Ok(terms)
}

fn validate(terms: &[Term], doc_count: u32) -> Result<(), String> {
    let mut previous: Option<&[u8]> = None;
    for term in terms {
        if term.word.is_empty()
            || std::str::from_utf8(&term.word).is_err()
            || previous.is_some_and(|word| word >= term.word.as_slice())
            || term.title_df > u64::from(doc_count)
            || term.body_df > u64::from(doc_count)
            || (term.title_df == 0 && term.body_df == 0)
        {
            return Err("Invalid or unsorted term dictionary".into());
        }
        previous = Some(&term.word);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dictionary_preserves_unicode_dfs_and_ids() {
        let terms = vec![
            Term {
                word: b"anchor".to_vec(),
                title_df: 1,
                body_df: 3,
            },
            Term {
                word: "café".as_bytes().to_vec(),
                title_df: 0,
                body_df: 2,
            },
        ];
        let (records, text) = encode(&terms, 3).unwrap();
        assert_eq!(decode(&records, &text, 3).unwrap(), terms);
        let (records, text) = encode(&[], 0).unwrap();
        assert!(decode(&records, &text, 0).unwrap().is_empty());
    }

    #[test]
    fn bad_offsets_dfs_words_and_truncation_fail_closed() {
        let term = Term {
            word: b"anchor".to_vec(),
            title_df: 1,
            body_df: 3,
        };
        let (records, text) = encode(std::slice::from_ref(&term), 3).unwrap();
        for n in 0..records.len() {
            assert!(decode(&records[..n], &text, 3).is_err());
        }
        for n in 0..text.len() {
            assert!(decode(&records, &text[..n], 3).is_err());
        }
        for offset in [40, 48, 56, 64] {
            let mut bad = records.clone();
            bad[offset] = 0xff;
            assert!(decode(&bad, &text, 3).is_err(), "{offset}");
        }
        assert!(encode(&[term.clone(), term.clone()], 3).is_err());
        assert!(encode(std::slice::from_ref(&term), 2).is_err());
        assert!(encode(
            &[Term {
                word: vec![0xff],
                ..term
            }],
            3
        )
        .is_err());
    }
}
