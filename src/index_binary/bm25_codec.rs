//! Draft child module of bm25: checked core segment integration.
use super::*;
use crate::index_binary::{csr, postings, profiles, sections, terms};
use std::collections::BTreeMap;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Auxiliary {
    num_docs: u32,
    avg_title_len_bits: u64,
    avg_body_len_bits: u64,
    stemmed: bool,
    keep_hyphens: bool,
    tag_prime_map: HashMap<String, u128>,
    entity_posting_lists: HashMap<String, MiniRoaring>,
    entity_kinds: HashMap<String, String>,
    entity_labels: HashMap<String, String>,
}

pub enum Segment<'a> {
    Bytes(Vec<u8>),
    Text(&'a [crate::bm25::Section]),
}

pub fn encode(index: &mut Bm25Index) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let mut result = BTreeMap::new();
    encode_with(index, |name, segment| {
        let bytes = match segment {
            Segment::Bytes(bytes) => bytes,
            Segment::Text(sections) => {
                let mut bytes = Vec::new();
                sections::write_borrowed_text(sections, &mut bytes)?;
                bytes
            }
        };
        result.insert(name.into(), bytes);
        Ok(())
    })?;
    Ok(result)
}

pub fn encode_with(
    index: &mut Bm25Index,
    mut write: impl for<'a> FnMut(&str, Segment<'a>) -> Result<(), String>,
) -> Result<(), String> {
    let _encode = crate::index_timing::Span::new("v4.encode.core");
    {
        let _compact = crate::index_timing::Span::new("v4.encode.compact");
        index.compact_for_v4()?;
    }
    let forward = index
        .compact_forward
        .as_ref()
        .ok_or("Missing compact forward rows")?;
    let interned = index
        .interned
        .get()
        .ok_or("Missing compact scoring postings")?;
    let flat = interned
        .flat_postings
        .as_ref()
        .ok_or("Missing compact postings")?;
    let doc_count = u32::try_from(index.num_docs).map_err(|_| "Documents exceed u32")?;
    let mut dictionary: Vec<_> = forward.vocabulary.iter().collect();
    dictionary.sort_unstable_by_key(|(_, id)| **id);
    let dictionary: Vec<_> = dictionary
        .into_iter()
        .map(|(word, _)| terms::Term {
            word: word.clone(),
            title_df: index.title_dfs.get(word).copied().unwrap_or(0) as u64,
            body_df: index.body_dfs.get(word).copied().unwrap_or(0) as u64,
        })
        .collect();
    let (term_table, term_text) = terms::encode(&dictionary, doc_count)?;
    drop(dictionary);
    write("terms.tbl", Segment::Bytes(term_table))?;
    write("term-text.bin", Segment::Bytes(term_text))?;
    write(
        "sections.tbl",
        Segment::Bytes(sections::borrowed_table(&index.sections)?),
    )?;
    write("text.bin", Segment::Text(&index.sections))?;
    crate::index_timing::memory_checkpoint("v4.memory.text_streamed");
    if index.title_lens.len() != index.num_docs
        || index.body_lens.len() != index.num_docs
        || index.prime_filters.len() != index.num_docs
        || index.sections.len() != index.num_docs
    {
        return Err("Document metadata count mismatch".into());
    }
    let document_profiles: Vec<_> = (0..index.num_docs)
        .map(|doc| profiles::DocumentProfile {
            title_len: index.title_lens[doc] as u64,
            body_len: index.body_lens[doc] as u64,
            term_mask: index.prime_filters[doc].term_mask,
            tag_signature: index.prime_filters[doc].tag_signature,
        })
        .collect();
    write(
        "profiles.bin",
        Segment::Bytes(profiles::encode(&document_profiles)?),
    )?;
    drop(document_profiles);
    write("forward-title.bin", Segment::Bytes(forward.title.encode()?))?;
    write("forward-body.bin", Segment::Bytes(forward.body.encode()?))?;
    write("postings.bin", Segment::Bytes(flat.encode()?))?;
    let auxiliary = Auxiliary {
        num_docs: doc_count,
        avg_title_len_bits: index.avg_title_len.to_bits(),
        avg_body_len_bits: index.avg_body_len.to_bits(),
        stemmed: index.stemmed,
        keep_hyphens: index.keep_hyphens,
        tag_prime_map: index.tag_prime_map.clone(),
        entity_posting_lists: index.entity_posting_lists.clone(),
        entity_kinds: index.entity_kinds.clone(),
        entity_labels: index.entity_labels.clone(),
    };
    write(
        "bm25-aux.json",
        Segment::Bytes(serde_json::to_vec(&auxiliary).map_err(|e| e.to_string())?),
    )?;
    Ok(())
}

pub fn decode(segments: &BTreeMap<String, Vec<u8>>) -> Result<Bm25Index, String> {
    decode_checked(segments, true)
}

pub fn decode_checked(
    segments: &BTreeMap<String, Vec<u8>>,
    deep: bool,
) -> Result<Bm25Index, String> {
    let bytes = |name: &str| -> Result<&[u8], String> {
        segments
            .get(name)
            .map(Vec::as_slice)
            .ok_or_else(|| format!("Missing v4 segment {name}"))
    };
    let decode_span = crate::index_timing::Span::new("v4.decode.aux");
    let auxiliary: Auxiliary = serde_json::from_slice(bytes("bm25-aux.json")?)
        .map_err(|e| format!("Invalid v4 BM25 settings: {e}"))?;
    drop(decode_span);
    let doc_count = auxiliary.num_docs;
    let dictionary = {
        let _span = crate::index_timing::Span::new("v4.decode.terms");
        terms::decode(bytes("terms.tbl")?, bytes("term-text.bin")?, doc_count)?
    };
    let section_text = {
        let _span = crate::index_timing::Span::new("v4.decode.sections");
        sections::decode(bytes("sections.tbl")?, bytes("text.bin")?)?
    };
    let document_profiles = {
        let _span = crate::index_timing::Span::new("v4.decode.profiles");
        profiles::decode(bytes("profiles.bin")?)?
    };
    let title = {
        let _span = crate::index_timing::Span::new("v4.decode.forward_title");
        csr::ForwardCsr::decode(bytes("forward-title.bin")?)?
    };
    let body = {
        let _span = crate::index_timing::Span::new("v4.decode.forward_body");
        csr::ForwardCsr::decode(bytes("forward-body.bin")?)?
    };
    let flat = {
        let _span = crate::index_timing::Span::new("v4.decode.postings");
        postings::PostingsCsr::decode(bytes("postings.bin")?)?
    };
    let docs = doc_count as usize;
    if section_text.len() != docs
        || document_profiles.len() != docs
        || title.len() != docs
        || body.len() != docs
        || flat.doc_count() != doc_count
        || title.term_count() as usize != dictionary.len()
        || body.term_count() as usize != dictionary.len()
        || flat.len() != dictionary.len()
    {
        return Err("V4 BM25 cross-segment count mismatch".into());
    }
    let avg_title_len = f64::from_bits(auxiliary.avg_title_len_bits);
    let avg_body_len = f64::from_bits(auxiliary.avg_body_len_bits);
    if !avg_title_len.is_finite()
        || avg_title_len < 0.0
        || !avg_body_len.is_finite()
        || avg_body_len < 0.0
    {
        return Err("Invalid v4 field averages".into());
    }
    if deep {
        let forward_validation = crate::index_timing::Span::new("v4.validate.forward_masks");
        for (doc, profile) in document_profiles.iter().enumerate() {
            let length = |rows: &csr::ForwardCsr| -> Result<u64, String> {
                rows.row(doc)
                    .ok_or("Missing forward row")?
                    .iter()
                    .try_fold(0_u64, |n, entry| {
                        n.checked_add(entry.tf)
                            .ok_or("Forward TF sum overflow".into())
                    })
            };
            if length(&title)? != profile.title_len || length(&body)? != profile.body_len {
                return Err("V4 forward TFs do not match document lengths".into());
            }
            let mut filter = PrimeFilter::new();
            for entry in title.row(doc).unwrap().iter().chain(body.row(doc).unwrap()) {
                filter.add_term(&dictionary[entry.term as usize].word);
            }
            if filter.term_mask != profile.term_mask {
                return Err("V4 prime mask does not match forward terms".into());
            }
        }
        drop(forward_validation);
        let posting_validation =
            crate::index_timing::Span::new("v4.validate.postings_and_candidates");
        let mut title_entries = 0_usize;
        let mut body_entries = 0_usize;

        for (id, term) in dictionary.iter().enumerate() {
            let row = flat.row(id as u32).ok_or("Missing term postings")?;
            let mut title_df = 0_u64;
            let mut body_df = 0_u64;

            for posting in row {
                if title.get(posting.doc as usize, id as u32) != posting.title_tf
                    || body.get(posting.doc as usize, id as u32) != posting.body_tf
                {
                    return Err("V4 forward/posting TF disagreement".into());
                }
                if posting.title_tf != 0 {
                    title_df += 1;
                    title_entries += 1;
                }
                if posting.body_tf != 0 {
                    body_df += 1;
                    body_entries += 1;
                }
            }
            if title_df != term.title_df || body_df != term.body_df {
                return Err("V4 posting DFs disagree with dictionary".into());
            }
        }
        if title_entries != title.entry_count() || body_entries != body.entry_count() {
            return Err("V4 forward entries are absent from postings".into());
        }
        for bitmap in auxiliary.entity_posting_lists.values() {
            if bitmap.iter().iter().any(|doc| *doc >= doc_count) {
                return Err("V4 entity posting exceeds document count".into());
            }
        }
        drop(posting_validation);
    }
    let _reconstruct = crate::index_timing::Span::new("v4.reconstruct.maps_profiles");
    let mut title_dfs = HashMap::new();
    let mut body_dfs = HashMap::new();
    let mut vocabulary = HashMap::new();
    for (id, term) in dictionary.into_iter().enumerate() {
        if term.title_df != 0 {
            title_dfs.insert(
                term.word.clone(),
                usize::try_from(term.title_df).map_err(|_| "Title DF exceeds usize")?,
            );
        }
        if term.body_df != 0 {
            body_dfs.insert(
                term.word.clone(),
                usize::try_from(term.body_df).map_err(|_| "Body DF exceeds usize")?,
            );
        }
        vocabulary.insert(term.word, id as u32);
    }
    let interned = InternedIndex {
        vocabulary: vocabulary.clone(),
        postings: Vec::new(),
        candidate_cache: (0..flat.len())
            .map(|_| std::sync::OnceLock::new())
            .collect(),
        flat_postings: Some(flat),
        bounds: std::sync::Mutex::new(HashMap::new()),
    };
    let mut sections = Vec::with_capacity(docs);
    for section in section_text {
        sections.push(Section {
            title: section.title,
            body: section.body,
            filename: section.filename,
            line_number: usize::try_from(section.line_number)
                .map_err(|_| "Section line exceeds usize")?,
            entities: section.entities,
        });
    }
    let mut title_lens = Vec::with_capacity(docs);
    let mut body_lens = Vec::with_capacity(docs);
    let mut prime_filters = Vec::with_capacity(docs);
    for profile in document_profiles {
        title_lens
            .push(usize::try_from(profile.title_len).map_err(|_| "Title length exceeds usize")?);
        body_lens.push(usize::try_from(profile.body_len).map_err(|_| "Body length exceeds usize")?);
        prime_filters.push(PrimeFilter {
            term_mask: profile.term_mask,
            tag_signature: profile.tag_signature,
        });
    }
    Ok(Bm25Index {
        sections,
        num_docs: docs,
        title_tfs: Vec::new(),
        body_tfs: Vec::new(),
        title_lens,
        body_lens,
        avg_title_len,
        avg_body_len,
        title_dfs,
        body_dfs,
        posting_lists: HashMap::new(),
        prime_filters,
        tag_prime_map: auxiliary.tag_prime_map,
        entity_posting_lists: auxiliary.entity_posting_lists,
        entity_kinds: auxiliary.entity_kinds,
        entity_labels: auxiliary.entity_labels,
        stemmed: auxiliary.stemmed,
        keep_hyphens: auxiliary.keep_hyphens,
        compact_forward: Some(std::sync::Arc::new(CompactForward {
            vocabulary,
            title,
            body,
        })),
        interned: std::sync::OnceLock::from(std::sync::Arc::new(interned)),
    })
}
