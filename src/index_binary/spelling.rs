//! Sealed spelling vocabulary and delta-coded trigram posting IDs.
//! Word order is preserved: stable ties in the corrector depend on these IDs.
use super::codec::{Reader, Writer};
use crate::fast_retrieval::MiniRoaring;
use crate::spelling::SpellIndex;
use std::collections::{HashMap, HashSet};

const KIND: u16 = 9;
pub const FILE: &str = "spelling.bin";

fn text(writer: &mut Writer, value: &str) {
    writer.varint(value.len() as u64);
    writer.raw(value.as_bytes());
}
fn read_text(reader: &mut Reader<'_>) -> Result<String, String> {
    let length = usize::try_from(reader.varint()?).map_err(|_| "Spelling text length overflow")?;
    let value = reader.text(length)?;
    if value.is_empty() {
        return Err("Empty spelling word or trigram".into());
    }
    Ok(value.to_string())
}
fn validate(index: &SpellIndex) -> Result<(), String> {
    if index.num_words != index.unique_words.len()
        || index.word_lens.len() != index.num_words
        || index.vocab_set.len() != index.num_words
        || !index.avg_word_len.is_finite()
        || index.avg_word_len < 0.0
        || index.trigram_dfs.len() != index.trigram_postings.len()
    {
        return Err("Inconsistent spelling index counts or statistics".into());
    }
    let mut total = 0usize;
    let mut seen = HashSet::new();
    for (word, &length) in index.unique_words.iter().zip(&index.word_lens) {
        if word.is_empty() || !index.vocab_set.contains(word) || !seen.insert(word) || length == 0 {
            return Err("Invalid spelling vocabulary".into());
        }
        total = total
            .checked_add(length)
            .ok_or("Spelling length total overflow")?;
    }
    let average = if index.num_words == 0 {
        0.0
    } else {
        total as f64 / index.num_words as f64
    };
    if average.to_bits() != index.avg_word_len.to_bits() {
        return Err("Spelling average length mismatch".into());
    }
    for (trigram, posting) in &index.trigram_postings {
        if trigram.is_empty()
            || posting.is_empty()
            || index.trigram_dfs.get(trigram) != Some(&posting.len())
            || posting
                .iter()
                .last()
                .is_some_and(|&id| id as usize >= index.num_words)
        {
            return Err("Invalid spelling trigram posting or DF".into());
        }
    }
    Ok(())
}

pub fn encode(index: &SpellIndex) -> Result<Vec<u8>, String> {
    validate(index)?;
    let count = u32::try_from(index.num_words).map_err(|_| "Spelling count exceeds u32")?;
    let mut writer = Writer::new(KIND, count, 0);
    writer.f64(index.avg_word_len);
    writer.varint(index.trigram_postings.len() as u64);
    for (word, &length) in index.unique_words.iter().zip(&index.word_lens) {
        text(&mut writer, word);
        writer.varint(length as u64);
    }
    let mut keys: Vec<_> = index.trigram_postings.keys().collect();
    keys.sort();
    for key in keys {
        text(&mut writer, key);
        let ids = index.trigram_postings[key].iter();
        writer.varint(ids.len() as u64);
        let mut previous = 0u32;
        for id in ids {
            writer.varint(u64::from(id - previous));
            previous = id;
        }
    }
    writer.finish()
}

pub fn decode(bytes: &[u8]) -> Result<SpellIndex, String> {
    let mut reader = Reader::new(bytes, KIND)?;
    if reader.record_bytes != 0 {
        return Err("Invalid spelling record width".into());
    }
    let num_words = reader.count as usize;
    let avg_word_len = reader.f64()?;
    let trigram_count = usize::try_from(reader.varint()?).map_err(|_| "Trigram count overflow")?;
    // Each word takes at least a length, one UTF-8 byte, and a word length;
    // each trigram takes at least those three bytes plus one posting delta.
    let minimum = num_words
        .checked_mul(3)
        .and_then(|n| trigram_count.checked_mul(4).and_then(|t| n.checked_add(t)))
        .ok_or("Spelling count overflow")?;
    if minimum > reader.remaining() {
        return Err("Spelling counts exceed payload".into());
    }
    let mut unique_words = Vec::new();
    let mut word_lens = Vec::new();
    let mut vocab_set = HashSet::new();
    unique_words
        .try_reserve_exact(num_words)
        .map_err(|_| "Cannot allocate spelling words")?;
    word_lens
        .try_reserve_exact(num_words)
        .map_err(|_| "Cannot allocate spelling lengths")?;
    vocab_set
        .try_reserve(num_words)
        .map_err(|_| "Cannot allocate spelling vocabulary")?;
    for _ in 0..num_words {
        let word = read_text(&mut reader)?;
        if !vocab_set.insert(word.clone()) {
            return Err("Duplicate spelling word".into());
        }
        unique_words.push(word);
        word_lens.push(usize::try_from(reader.varint()?).map_err(|_| "Word length overflow")?);
    }
    let mut trigram_postings = HashMap::new();
    let mut trigram_dfs = HashMap::new();
    trigram_postings
        .try_reserve(trigram_count)
        .map_err(|_| "Cannot allocate spelling postings")?;
    trigram_dfs
        .try_reserve(trigram_count)
        .map_err(|_| "Cannot allocate spelling DFs")?;
    let mut previous_key: Option<String> = None;
    for _ in 0..trigram_count {
        let key = read_text(&mut reader)?;
        if previous_key
            .as_ref()
            .is_some_and(|previous| previous >= &key)
        {
            return Err("Duplicate or unsorted spelling trigrams".into());
        }
        previous_key = Some(key.clone());
        let count = usize::try_from(reader.varint()?).map_err(|_| "Posting count overflow")?;
        if count == 0 || count > num_words || count > reader.remaining() {
            return Err("Invalid spelling posting count".into());
        }
        let mut ids = Vec::new();
        ids.try_reserve_exact(count)
            .map_err(|_| "Cannot allocate spelling posting")?;
        let mut previous = 0u32;
        for ordinal in 0..count {
            let delta = u32::try_from(reader.varint()?).map_err(|_| "Spelling delta overflow")?;
            if ordinal > 0 && delta == 0 {
                return Err("Duplicate spelling posting ID".into());
            }
            let id = previous
                .checked_add(delta)
                .ok_or("Spelling posting ID overflow")?;
            if id as usize >= num_words {
                return Err("Spelling posting ID exceeds vocabulary".into());
            }
            ids.push(id);
            previous = id;
        }
        trigram_dfs.insert(key.clone(), count);
        trigram_postings.insert(key, MiniRoaring::from_sorted(&ids));
    }
    reader.finish()?;
    let index = SpellIndex {
        unique_words,
        vocab_set,
        trigram_postings,
        word_lens,
        trigram_dfs,
        avg_word_len,
        num_words,
    };
    validate(&index)?;
    Ok(index)
}
