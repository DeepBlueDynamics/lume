//! Explicit legacy JSON export of either legacy or compact in-memory BM25.
//! Binary generation writes do not use this export path.
use super::*;
use serde::ser::{SerializeMap, SerializeSeq, SerializeStruct};

struct ByteMap<'a, T>(&'a HashMap<Vec<u8>, T>);
impl<T: Serialize> Serialize for ByteMap<'_, T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serialize_u8_map(self.0, serializer)
    }
}

struct ForwardRows<'a> {
    index: &'a Bm25Index,
    title: bool,
}
struct ForwardRow<'a> {
    entries: &'a [crate::index_binary::csr::ForwardEntry],
    words: &'a [&'a [u8]],
}
impl Serialize for ForwardRow<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.entries.len()))?;
        for entry in self.entries {
            let word = self.words.get(entry.term as usize).ok_or_else(|| {
                serde::ser::Error::custom("Forward term absent from export dictionary")
            })?;
            map.serialize_entry(&String::from_utf8_lossy(word), &entry.tf)?;
        }
        map.end()
    }
}
impl Serialize for ForwardRows<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let Some(compact) = &self.index.compact_forward else {
            let rows = if self.title {
                &self.index.title_tfs
            } else {
                &self.index.body_tfs
            };
            return serialize_vec_u8_map(rows, serializer);
        };
        let mut words = vec![&[][..]; compact.vocabulary.len()];
        for (word, &id) in &compact.vocabulary {
            let slot = words
                .get_mut(id as usize)
                .ok_or_else(|| serde::ser::Error::custom("Invalid export term ID"))?;
            *slot = word;
        }
        let rows = if self.title {
            &compact.title
        } else {
            &compact.body
        };
        let mut sequence = serializer.serialize_seq(Some(rows.len()))?;
        for doc in 0..rows.len() {
            let entries = rows
                .row(doc)
                .ok_or_else(|| serde::ser::Error::custom("Invalid export forward row"))?;
            sequence.serialize_element(&ForwardRow {
                entries,
                words: &words,
            })?;
        }
        sequence.end()
    }
}

struct CandidateRows<'a>(&'a Bm25Index);
impl Serialize for CandidateRows<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let Some(compact) = &self.0.compact_forward else {
            return serialize_u8_map(&self.0.posting_lists, serializer);
        };
        let interned = self
            .0
            .interned
            .get()
            .ok_or_else(|| serde::ser::Error::custom("Missing export postings"))?;
        let mut map = serializer.serialize_map(Some(compact.vocabulary.len()))?;
        for (word, &id) in &compact.vocabulary {
            let mut bitmap = MiniRoaring::new();
            for posting in interned.postings(id) {
                bitmap.insert(posting.doc);
            }
            map.serialize_entry(&String::from_utf8_lossy(word), &bitmap)?;
        }
        map.end()
    }
}

impl Serialize for Bm25Index {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut value = serializer.serialize_struct("Bm25Index", 18)?;
        value.serialize_field("sections", &self.sections)?;
        value.serialize_field("num_docs", &self.num_docs)?;
        value.serialize_field(
            "title_tfs",
            &ForwardRows {
                index: self,
                title: true,
            },
        )?;
        value.serialize_field(
            "body_tfs",
            &ForwardRows {
                index: self,
                title: false,
            },
        )?;
        value.serialize_field("title_lens", &self.title_lens)?;
        value.serialize_field("body_lens", &self.body_lens)?;
        value.serialize_field("avg_title_len", &self.avg_title_len)?;
        value.serialize_field("avg_body_len", &self.avg_body_len)?;
        value.serialize_field("title_dfs", &ByteMap(&self.title_dfs))?;
        value.serialize_field("body_dfs", &ByteMap(&self.body_dfs))?;
        value.serialize_field("posting_lists", &CandidateRows(self))?;
        value.serialize_field("prime_filters", &self.prime_filters)?;
        value.serialize_field("tag_prime_map", &self.tag_prime_map)?;
        value.serialize_field("entity_posting_lists", &self.entity_posting_lists)?;
        value.serialize_field("entity_kinds", &self.entity_kinds)?;
        value.serialize_field("entity_labels", &self.entity_labels)?;
        value.serialize_field("stemmed", &self.stemmed)?;
        value.serialize_field("keep_hyphens", &self.keep_hyphens)?;
        value.end()
    }
}
