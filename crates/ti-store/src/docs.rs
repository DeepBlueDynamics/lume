//! Durable documents with D52 append-only transaction frames and atomic compaction.
//! Identity and the docs SQL schema remain unchanged; legacy JSON is migrated.

#[path = "docs_log.rs"]
mod log;
use arrow_array::{ArrayRef, Float64Array, RecordBatch, StringArray, TimestampSecondArray};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use ti_contracts::{docs_schema, Document, Error, Result};

/// Serde mirror of the frozen `Document` contract type.
#[derive(Clone, Serialize, Deserialize)]
struct StoredDocument {
    id: String,
    vessel: String,
    kind: String,
    ts_start: i64,
    ts_end: Option<i64>,
    title: String,
    body: String,
}

impl From<&Document> for StoredDocument {
    fn from(d: &Document) -> Self {
        Self {
            id: d.id.clone(),
            vessel: d.vessel.clone(),
            kind: d.kind.clone(),
            ts_start: d.ts_start,
            ts_end: d.ts_end,
            title: d.title.clone(),
            body: d.body.clone(),
        }
    }
}

impl From<StoredDocument> for Document {
    fn from(d: StoredDocument) -> Self {
        Self {
            id: d.id,
            vessel: d.vessel,
            kind: d.kind,
            ts_start: d.ts_start,
            ts_end: d.ts_end,
            title: d.title,
            body: d.body,
        }
    }
}

#[derive(Serialize, Deserialize)]
enum Operation {
    Upsert(StoredDocument),
    Delete { vessel: String, id: String },
}
impl Operation {
    fn key(&self) -> (String, String) {
        match self {
            Self::Upsert(doc) => (doc.vessel.clone(), doc.id.clone()),
            Self::Delete { vessel, id } => (vessel.clone(), id.clone()),
        }
    }
    fn validate(&self) -> Result<()> {
        if let Self::Upsert(doc) = self {
            Document::from(doc.clone())
                .validate()
                .map_err(|e| Error::Corrupt(format!("invalid document transaction: {e}")))?;
        }
        Ok(())
    }
}

/// Documents keyed by (vessel URN, id), with D52 durable transaction frames.
pub struct DocStore {
    persistence: Option<log::Persistence>,
    docs: BTreeMap<(String, String), Document>,
    version: u64,
}
impl DocStore {
    pub fn in_memory() -> Self {
        Self {
            persistence: None,
            docs: BTreeMap::new(),
            version: 0,
        }
    }
    /// Open the versioned log, migrate legacy JSON, and recover a torn tail.
    pub fn open(store_root: &Path) -> Result<Self> {
        let mut persistence = log::Persistence::new(store_root);
        persistence.migrate()?;
        persistence.invalidate(); // Rebuild memory from a newly migrated segment too.
        let mut store = Self {
            persistence: Some(persistence),
            ..Self::in_memory()
        };
        store.refresh()?;
        if store.persistence.as_ref().is_some_and(|p| p.torn) {
            let _lock = store.persistence.as_ref().unwrap().writer_lock()?;
            store.sync_locked(true)?;
        }
        store.version = 0;
        Ok(store)
    }
    fn apply(&mut self, operations: Vec<Operation>) {
        for operation in operations {
            let key = operation.key();
            match operation {
                Operation::Upsert(doc) => {
                    self.docs.insert(key, Document::from(doc));
                }
                Operation::Delete { .. } => {
                    self.docs.remove(&key);
                }
            }
        }
    }
    fn sync_locked(&mut self, repair: bool) -> Result<bool> {
        let Some(persistence) = self.persistence.as_mut() else {
            return Ok(false);
        };
        let replay = match persistence.replay_locked(repair) {
            Ok(replay) => replay,
            Err(error) => {
                persistence.invalidate();
                return Err(error);
            }
        };
        let changed = if replay.reset {
            let previous = std::mem::take(&mut self.docs);
            self.apply(replay.operations);
            self.docs != previous
        } else {
            let before = replay
                .operations
                .iter()
                .map(|operation| {
                    let key = operation.key();
                    let value = self.docs.get(&key).cloned();
                    (key, value)
                })
                .collect::<BTreeMap<_, _>>();
            self.apply(replay.operations);
            before
                .into_iter()
                .any(|(key, value)| self.docs.get(&key) != value.as_ref())
        };
        if changed {
            self.version += 1;
        }
        Ok(changed)
    }
    /// Incrementally reload externally committed changes; invalidate text caches.
    pub fn refresh(&mut self) -> Result<bool> {
        let Some(persistence) = &self.persistence else {
            return Ok(false);
        };
        let _lock = persistence.reader_lock()?;
        self.sync_locked(false)
    }
    fn mutate(
        &mut self,
        build: impl FnOnce(&BTreeMap<(String, String), Document>) -> Vec<Operation>,
    ) -> Result<()> {
        let _lock = self
            .persistence
            .as_ref()
            .map(log::Persistence::writer_lock)
            .transpose()?;
        self.sync_locked(true)?;
        let operations = build(&self.docs);
        if operations.is_empty() {
            return Ok(());
        }
        if let Some(persistence) = &mut self.persistence {
            persistence.append_locked(&operations)?;
        }
        self.apply(operations);
        self.version += 1;
        if let Some(persistence) = &mut self.persistence {
            if let Err(error) = persistence.compact_locked(self.docs.values(), self.docs.len()) {
                // The transaction is already durable. Maintenance failure must
                // not falsely report the committed mutation as failed.
                eprintln!("DocStore compaction failed; retaining committed log: {error}");
            }
        }
        Ok(())
    }
    /// Validate all inputs, then replace changed identities in one durable frame.
    pub fn upsert_all(&mut self, docs: impl IntoIterator<Item = Document>) -> Result<()> {
        let incoming = docs.into_iter().collect::<Vec<_>>();
        for doc in &incoming {
            doc.validate()?;
        }
        self.mutate(move |existing| {
            let mut latest = BTreeMap::new();
            let mut operations = Vec::new();
            for doc in incoming {
                let key = (doc.vessel.clone(), doc.id.clone());
                if latest.get(&key).or_else(|| existing.get(&key)) != Some(&doc) {
                    operations.push(Operation::Upsert(StoredDocument::from(&doc)));
                    latest.insert(key, doc);
                }
            }
            operations
        })
    }

    /// Reconcile a caller-owned subset atomically, preserving unowned documents.
    pub fn reconcile(
        &mut self,
        vessel: &str,
        owned: &std::collections::BTreeSet<String>,
        documents: Vec<Document>,
    ) -> Result<()> {
        let mut incoming = BTreeMap::new();
        for document in documents {
            document.validate()?;
            if document.vessel != vessel {
                return Err(Error::InvalidInput(
                    "reconcile document vessel mismatch".into(),
                ));
            }
            if incoming.insert(document.id.clone(), document).is_some() {
                return Err(Error::InvalidInput(
                    "duplicate reconcile document id".into(),
                ));
            }
        }
        self.mutate(move |existing| {
            let mut operations = Vec::new();
            for id in owned {
                if !incoming.contains_key(id) && existing.contains_key(&(vessel.into(), id.clone()))
                {
                    operations.push(Operation::Delete {
                        vessel: vessel.into(),
                        id: id.clone(),
                    });
                }
            }
            operations.extend(
                incoming
                    .into_values()
                    .filter(|doc| existing.get(&(vessel.into(), doc.id.clone())) != Some(doc))
                    .map(|doc| Operation::Upsert(StoredDocument::from(&doc))),
            );
            operations
        })
    }
    /// Delete a single identity; deleting a missing document is a no-op.
    pub fn delete(&mut self, vessel: &str, id: &str) -> Result<()> {
        self.mutate(|existing| {
            if existing.contains_key(&(vessel.into(), id.into())) {
                vec![Operation::Delete {
                    vessel: vessel.into(),
                    id: id.into(),
                }]
            } else {
                vec![]
            }
        })
    }
    /// Documents in (vessel, id) order.
    pub fn iter(&self) -> impl Iterator<Item = &Document> {
        self.docs.values()
    }
    pub fn len(&self) -> usize {
        self.docs.len()
    }
    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }
    /// Changes only when the observable document set changes.
    pub fn version(&self) -> u64 {
        self.version
    }
}

/// Build a `docs_schema` batch; `score` is null unless a query attached one (spec/14).
pub fn documents_batch<'a>(
    rows: impl IntoIterator<Item = (&'a Document, Option<f64>)>,
) -> Result<RecordBatch> {
    let rows: Vec<_> = rows.into_iter().collect();
    let strings = |f: fn(&Document) -> &str| -> ArrayRef {
        Arc::new(StringArray::from_iter_values(
            rows.iter().map(|(d, _)| f(d)),
        ))
    };
    let columns: Vec<ArrayRef> = vec![
        strings(|d| &d.id),
        strings(|d| &d.vessel),
        strings(|d| &d.kind),
        Arc::new(
            TimestampSecondArray::from_iter_values(rows.iter().map(|(d, _)| d.ts_start))
                .with_timezone("UTC"),
        ),
        Arc::new(
            TimestampSecondArray::from(rows.iter().map(|(d, _)| d.ts_end).collect::<Vec<_>>())
                .with_timezone("UTC"),
        ),
        strings(|d| &d.title),
        strings(|d| &d.body),
        Arc::new(Float64Array::from(
            rows.iter().map(|(_, s)| *s).collect::<Vec<_>>(),
        )),
    ];
    RecordBatch::try_new(docs_schema(), columns).map_err(Error::Arrow)
}

#[cfg(test)]
#[path = "docs_tests.rs"]
mod append_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(id: &str, body: &str) -> Document {
        Document {
            id: id.into(),
            vessel: "vessels.urn:mrn:imo:mmsi:367000000".into(),
            kind: "notes".into(),
            ts_start: 1_780_000_000,
            ts_end: Some(1_780_000_060),
            title: "Note".into(),
            body: body.into(),
        }
    }

    #[test]
    fn reconcile_is_atomic_idempotent_and_preserves_unowned_documents() {
        let mut store = DocStore::in_memory();
        store
            .upsert_all([doc("a", "old"), doc("manual", "keep")])
            .unwrap();
        let owned = std::collections::BTreeSet::from(["a".to_string()]);
        let vessel = "vessels.urn:mrn:imo:mmsi:367000000";
        store
            .reconcile(vessel, &owned, vec![doc("b", "new")])
            .unwrap();
        assert_eq!(
            store.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(),
            ["b", "manual"]
        );
        let version = store.version();
        store
            .reconcile(vessel, &owned, vec![doc("b", "new")])
            .unwrap();
        assert_eq!(store.version(), version);
        let mut invalid = doc("bad", "bad");
        invalid.ts_end = Some(invalid.ts_start);
        assert!(store
            .reconcile(
                vessel,
                &std::collections::BTreeSet::from(["b".to_string()]),
                vec![doc("valid", "valid"), invalid]
            )
            .is_err());
        assert_eq!(store.version(), version);
        assert_eq!(
            store.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(),
            ["b", "manual"]
        );
    }

    #[test]
    fn upsert_replaces_delete_is_idempotent_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = DocStore::open(dir.path()).unwrap();
        store
            .upsert_all([doc("a", "leak"), doc("b", "water")])
            .unwrap();
        store.upsert_all([doc("a", "leak fixed")]).unwrap();
        store
            .delete("vessels.urn:mrn:imo:mmsi:367000000", "missing")
            .unwrap();
        assert_eq!(store.version(), 2);
        let reopened = DocStore::open(dir.path()).unwrap();
        let bodies: Vec<_> = reopened.iter().map(|d| d.body.as_str()).collect();
        assert_eq!(bodies, ["leak fixed", "water"]);
        let mut bad = doc("c", "x");
        bad.ts_end = Some(bad.ts_start);
        assert!(DocStore::in_memory().upsert_all([bad]).is_err());
        let batch = documents_batch(reopened.iter().map(|d| (d, None))).unwrap();
        assert_eq!(batch.schema(), docs_schema());
        assert_eq!(batch.num_rows(), 2);
    }
}
