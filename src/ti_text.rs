//! Lume TI text index (W5): `match()` over notes, logbook and alerts with Lume BM25.
//!
//! Enforces:
//! - Lexical only: no network and no Shivvr, so `match()` works on the boat (W5 open
//!   question; the semantic path can come later as an opt-in).
//! - Query syntax: terms are OR'ed (Lume BM25 candidate semantics); an uppercase `OR`
//!   is a no-op separator and an uppercase `AND` intersects the groups on either side.
//! - A document covers `[ts_start, ts_end)`, or only its start bucket when `ts_end` is
//!   missing (spec/14). `match_buckets` returns global ColumnIds `(vessel << 32) | bucket`.
//! - Index and query caches are rebuilt whenever the document set's version changes.

use crate::bm25::{
    filter_query_stopwords, Bm25BuildOptions, Bm25Index, Bm25Params, SearchVariant, Section,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use ti_contracts::{
    bucket_of, BucketIx, Catalog, Document, DocumentIndex, Error, RecordBatch, Result,
    RoaringTreemap, TextIndex, VesselOrd,
};

/// Resolves a store-local vessel ordinal to its canonical URN.
pub type VesselResolver = Arc<dyn Fn(VesselOrd) -> Result<String> + Send + Sync>;
use ti_store::{docs::documents_batch, DocStore};

/// Maximum cached `(vessel, kind, query)` results before the cache is cleared.
const QUERY_CACHE_LIMIT: usize = 1024;

struct KindIndex {
    docs: Vec<Document>,
    bm25: Bm25Index,
}

impl KindIndex {
    fn build(docs: Vec<Document>) -> Self {
        let sections = docs
            .iter()
            .map(|d| Section {
                title: d.title.clone(),
                body: d.body.clone(),
                line_number: 0,
                filename: None,
                entities: Vec::new(),
            })
            .collect();
        // match() tokenizes queries without stemming, so TI documents are indexed
        // unstemmed regardless of the LUME_STEM default for ordinary indexes.
        let options = Bm25BuildOptions {
            stemmed: false,
            keep_hyphens: false,
        };
        Self {
            bm25: Bm25Index::build_with_options(sections, None, options),
            docs,
        }
    }

    /// Documents matching the query, as indexes into `docs`.
    fn matches(&self, query: &str) -> BTreeSet<usize> {
        let mut result: Option<BTreeSet<usize>> = None;
        for group in query_groups(query) {
            let mut hits = BTreeSet::new();
            for token in filter_query_stopwords(crate::tokenize(&group)) {
                if let Some(list) = self.bm25.candidate_posting(&token.bytes) {
                    hits.extend(list.iter().into_iter().map(|id| id as usize));
                }
            }
            result = Some(match result {
                Some(prior) => prior.intersection(&hits).copied().collect(),
                None => hits,
            });
        }
        result.unwrap_or_default()
    }

    /// BM25 score per matching document (0.0 if BM25 did not rank it).
    fn scored(&self, query: &str) -> Vec<(usize, f64)> {
        let matched = self.matches(query);
        let text = query_groups(query).join(" ");
        let scores: HashMap<usize, f64> = self
            .bm25
            .search_quiet(&text, SearchVariant::Classic, &Bm25Params::default(), None)
            .into_iter()
            .map(|h| (h.section_index, h.score))
            .collect();
        matched
            .into_iter()
            .map(|i| (i, scores.get(&i).copied().unwrap_or(0.0)))
            .collect()
    }
}

/// Split on uppercase `AND`; drop uppercase `OR` separators.
fn query_groups(query: &str) -> Vec<String> {
    let mut groups = vec![Vec::new()];
    for word in query.split_whitespace() {
        match word {
            "AND" => groups.push(Vec::new()),
            "OR" => {}
            _ => groups.last_mut().expect("nonempty").push(word),
        }
    }
    groups
        .into_iter()
        .filter(|g| !g.is_empty())
        .map(|g| g.join(" "))
        .collect()
}

/// `(vessel URN, kind, query)` -> matching documents' `(ts_start, ts_end)`.
type SpanCache = HashMap<(String, String, String), Arc<Vec<(i64, Option<i64>)>>>;

struct State {
    store: DocStore,
    built: Option<u64>,
    indexes: BTreeMap<(String, String), KindIndex>,
    cache: SpanCache,
}

impl State {
    fn indexes(&mut self) -> &BTreeMap<(String, String), KindIndex> {
        if self.built != Some(self.store.version()) {
            let mut grouped: BTreeMap<(String, String), Vec<Document>> = BTreeMap::new();
            for doc in self.store.iter() {
                grouped
                    .entry((doc.vessel.clone(), doc.kind.clone()))
                    .or_default()
                    .push(doc.clone());
            }
            self.indexes = grouped
                .into_iter()
                .map(|(key, docs)| (key, KindIndex::build(docs)))
                .collect();
            self.cache.clear();
            self.built = Some(self.store.version());
        }
        &self.indexes
    }
}

/// `TextIndex` + `DocumentIndex` over a store's `docs/` with Lume BM25.
pub struct LumeText {
    state: Mutex<State>,
    vessel_urn: VesselResolver,
    width_seconds: u64,
}

impl LumeText {
    /// Wrap a document set; `vessel_urn` resolves ordinals in `match_buckets`.
    pub fn new(store: DocStore, vessel_urn: VesselResolver, width_seconds: u64) -> Self {
        Self {
            state: Mutex::new(State {
                store,
                built: None,
                indexes: BTreeMap::new(),
                cache: HashMap::new(),
            }),
            vessel_urn,
            width_seconds,
        }
    }

    /// Open `<store_root>/docs/`, resolving vessels through the store catalog.
    pub fn open(store_root: &Path, catalog: Arc<dyn Catalog>, width_seconds: u64) -> Result<Self> {
        Ok(Self::new(
            DocStore::open(store_root)?,
            Arc::new(move |vessel| catalog.vessel_urn(vessel)),
            width_seconds,
        ))
    }

    fn lock(&self) -> Result<MutexGuard<'_, State>> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::Corrupt("text index lock poisoned".into()))?;
        state.store.refresh()?;
        Ok(state)
    }
}

impl TextIndex for LumeText {
    fn match_buckets(
        &self,
        vessel: VesselOrd,
        kind: &str,
        q: &str,
        from: BucketIx,
        to: BucketIx,
    ) -> Result<RoaringTreemap> {
        let urn = (self.vessel_urn)(vessel)?;
        let key = (urn, kind.to_string(), q.to_string());
        let spans = {
            let mut state = self.lock()?;
            state.indexes();
            if let Some(hit) = state.cache.get(&key) {
                Arc::clone(hit)
            } else {
                let spans: Vec<(i64, Option<i64>)> = state
                    .indexes
                    .get(&(key.0.clone(), key.1.clone()))
                    .map(|index| {
                        index
                            .matches(q)
                            .into_iter()
                            .map(|i| (index.docs[i].ts_start, index.docs[i].ts_end))
                            .collect()
                    })
                    .unwrap_or_default();
                let spans = Arc::new(spans);
                if state.cache.len() >= QUERY_CACHE_LIMIT {
                    state.cache.clear();
                }
                state.cache.insert(key, Arc::clone(&spans));
                spans
            }
        };
        let base = (vessel as u64) << 32;
        let mut out = RoaringTreemap::new();
        for &(start, end) in spans.iter() {
            let first = bucket_of(start, self.width_seconds)?;
            let last = match end {
                Some(end) => bucket_of(end - 1, self.width_seconds)?,
                None => first,
            };
            let (lo, hi) = (first.max(from), last.min(to));
            if lo <= hi {
                out.insert_range(base | lo as u64..(base | hi as u64) + 1);
            }
        }
        Ok(out)
    }
}

impl DocumentIndex for LumeText {
    fn upsert(&self, doc: &Document) -> Result<()> {
        self.lock()?.store.upsert_all([doc.clone()])
    }

    fn delete(&self, vessel: &str, id: &str) -> Result<()> {
        self.lock()?.store.delete(vessel, id)
    }

    fn documents(
        &self,
        vessel: Option<&str>,
        kind: Option<&str>,
        q: Option<&str>,
    ) -> Result<Vec<RecordBatch>> {
        let mut state = self.lock()?;
        let mut rows: Vec<(&Document, Option<f64>)> = Vec::new();
        for ((v, k), index) in state.indexes() {
            if vessel.is_some_and(|x| x != v) || kind.is_some_and(|x| x != k) {
                continue;
            }
            match q {
                Some(q) => rows.extend(
                    index
                        .scored(q)
                        .into_iter()
                        .map(|(i, score)| (&index.docs[i], Some(score))),
                ),
                None => rows.extend(index.docs.iter().map(|d| (d, None))),
            }
        }
        Ok(vec![documents_batch(rows)?])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const URN: &str = "vessels.urn:mrn:imo:mmsi:367000000";
    const T0: i64 = 1_780_000_000; // a multiple of 10

    fn doc(id: &str, start: i64, end: Option<i64>, body: &str) -> Document {
        Document {
            id: id.into(),
            vessel: URN.into(),
            kind: "notes".into(),
            ts_start: start,
            ts_end: end,
            title: format!("Note {id}"),
            body: body.into(),
        }
    }

    fn index() -> LumeText {
        let mut store = DocStore::in_memory();
        store
            .upsert_all([
                doc(
                    "a",
                    T0,
                    Some(T0 + 30),
                    "Leak observed near the engine room.",
                ),
                doc("b", T0 + 100, None, "Water in the bilge."),
                doc("c", T0 + 200, Some(T0 + 201), "Routine check, all good."),
            ])
            .unwrap();
        LumeText::new(store, Arc::new(|_| Ok(URN.to_string())), 10)
    }

    #[test]
    fn recorded_notification_messages_cover_buckets_and_replay() {
        let config = ti_contracts::TiConfig::default();
        let mut tracker =
            ti_ingest::notifications::NotificationDocuments::from_store(DocStore::in_memory());
        let messages: Vec<_> = [
            ("2026-05-28T20:26:40.000Z", "warn"),
            ("2026-05-28T20:26:50.000Z", "alarm"),
            ("2026-05-28T20:27:10.000Z", "normal"),
        ]
        .into_iter()
        .map(|(ts, state)| {
            serde_json::json!({
                "context": "vessels.self",
                "updates": [{
                    "timestamp": ts,
                    "values": [{"path": "notifications.battery", "value": {
                        "state": state, "message": "house battery voltage low", "method": ["sound"]
                    }}]
                }]
            })
            .to_string()
        })
        .collect();
        for message in &messages {
            tracker.ingest_message(message, URN, 0, &config).unwrap();
        }
        let before: Vec<_> = tracker.documents().iter().cloned().collect();
        for message in &messages {
            tracker.ingest_message(message, URN, 0, &config).unwrap();
        }
        assert_eq!(
            tracker.documents().iter().cloned().collect::<Vec<_>>(),
            before
        );
        assert_eq!(before.len(), 1);
        let mut docs = DocStore::in_memory();
        docs.upsert_all(before).unwrap();
        let text = LumeText::new(docs, Arc::new(|_| Ok(URN.into())), 10);
        let b0 = bucket_of(T0, 10).unwrap();
        assert_eq!(
            text.match_buckets(0, "alerts", "battery", 0, u32::MAX)
                .unwrap()
                .iter()
                .collect::<Vec<_>>(),
            vec![u64::from(b0), u64::from(b0 + 1), u64::from(b0 + 2)]
        );
    }

    #[test]
    fn external_documents_refresh_cached_matches() {
        let dir = std::env::temp_dir().join(format!("lume-doc-refresh-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let text = LumeText::new(
            DocStore::open(&dir).unwrap(),
            Arc::new(|_| Ok(URN.into())),
            10,
        );
        assert!(text
            .match_buckets(0, "notes", "battery", 0, u32::MAX)
            .unwrap()
            .is_empty());
        let mut writer = DocStore::open(&dir).unwrap();
        writer
            .upsert_all([doc("battery", T0, None, "battery low")])
            .unwrap();
        assert_eq!(
            text.match_buckets(0, "notes", "battery", 0, u32::MAX)
                .unwrap()
                .len(),
            1
        );
        writer
            .upsert_all([doc("battery", T0, Some(T0 + 30), "battery low cleared")])
            .unwrap();
        assert_eq!(
            text.match_buckets(0, "notes", "battery", 0, u32::MAX)
                .unwrap()
                .len(),
            3
        );
        assert_eq!(
            text.documents(None, None, Some("battery")).unwrap()[0].num_rows(),
            1
        );
        text.upsert(&doc("other", T0 + 40, None, "other note"))
            .unwrap();
        writer
            .upsert_all([doc("third", T0 + 50, None, "third note")])
            .unwrap();
        assert_eq!(text.documents(None, None, None).unwrap()[0].num_rows(), 3);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn match_covers_half_open_ranges_and_points() {
        let text = index();
        let b0 = bucket_of(T0, 10).unwrap();
        let ids: Vec<u64> = text
            .match_buckets(0, "notes", "leak OR water", 0, u32::MAX)
            .unwrap()
            .iter()
            .collect();
        // a covers [T0, T0+30) = 3 buckets; b is a point in its start bucket.
        let expected: Vec<u64> = [b0, b0 + 1, b0 + 2, b0 + 10]
            .iter()
            .map(|&b| b as u64)
            .collect();
        assert_eq!(ids, expected);
        let clipped = text
            .match_buckets(0, "notes", "leak", b0 + 1, b0 + 1)
            .unwrap();
        assert_eq!(clipped.iter().collect::<Vec<_>>(), vec![(b0 + 1) as u64]);
        assert!(text
            .match_buckets(0, "notes", "leak AND water", 0, u32::MAX)
            .unwrap()
            .is_empty());
        assert!(text
            .match_buckets(0, "logbook", "leak", 0, u32::MAX)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn documents_attach_scores_only_under_a_query_and_upserts_invalidate() {
        let text = index();
        let all = text.documents(Some(URN), None, None).unwrap();
        assert_eq!(all[0].num_rows(), 3);
        assert!(format!("{:?}", all[0].column(7)).contains("null"));
        let hits = text.documents(None, Some("notes"), Some("water")).unwrap();
        assert_eq!(hits[0].num_rows(), 1);
        assert!(!format!("{:?}", hits[0].column(7)).contains("null"));
        text.upsert(&doc("d", T0 + 300, None, "More water aft."))
            .unwrap();
        assert_eq!(
            text.match_buckets(0, "notes", "water", 0, u32::MAX)
                .unwrap()
                .len(),
            2
        );
    }
}
