//! Durable document set for W5 (`docs` table, `match()`): spec/05 text mapping, spec/14.
//!
//! Enforces:
//! - Every stored document passed `Document::validate`.
//! - `(vessel, id)` is the identity; upsert replaces, delete of a missing id is a no-op.
//! - Persistence is one atomically replaced JSON file, `<store>/docs/documents.json`,
//!   and a version counter that bumps on every change (text caches key on it).

use crate::catalog::atomic_write_json;
use arrow_array::{ArrayRef, Float64Array, RecordBatch, StringArray, TimestampSecondArray};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use ti_contracts::{docs_schema, Document, Error, Result};

/// Serde mirror of the frozen `Document` contract type.
#[derive(Serialize, Deserialize)]
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

/// Documents keyed by `(vessel URN, id)`, persisted under the store root.
pub struct DocStore {
    path: Option<PathBuf>,
    docs: BTreeMap<(String, String), Document>,
    version: u64,
    stamp: Option<(std::time::SystemTime, u64)>,
}

impl DocStore {
    /// An unpersisted store (tests, fixtures).
    pub fn in_memory() -> Self {
        Self {
            path: None,
            docs: BTreeMap::new(),
            version: 0,
            stamp: None,
        }
    }

    /// Open `<store_root>/docs/documents.json`; a missing file is an empty set.
    pub fn open(store_root: &Path) -> Result<Self> {
        let path = store_root.join("docs").join("documents.json");
        let mut store = Self {
            path: Some(path.clone()),
            ..Self::in_memory()
        };
        if path.exists() {
            let mut file = std::fs::File::open(&path)?;
            let metadata = file.metadata()?;
            store.stamp = Some((metadata.modified()?, metadata.len()));
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut file, &mut bytes)?;
            let stored: Vec<StoredDocument> = serde_json::from_slice(&bytes)
                .map_err(|e| Error::Corrupt(format!("{}: {e}", path.display())))?;
            for doc in stored.into_iter().map(Document::from) {
                doc.validate()?;
                store.docs.insert((doc.vessel.clone(), doc.id.clone()), doc);
            }
        }
        Ok(store)
    }

    fn file_stamp(path: &Path) -> Result<Option<(std::time::SystemTime, u64)>> {
        match std::fs::metadata(path) {
            Ok(meta) => Ok(Some((meta.modified()?, meta.len()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Reload externally published documents and invalidate readers' caches.
    pub fn refresh(&mut self) -> Result<bool> {
        let Some(path) = &self.path else { return Ok(false) };
        let stamp = Self::file_stamp(path)?;
        if stamp == self.stamp { return Ok(false) }
        let root = path.parent().and_then(Path::parent)
            .ok_or_else(|| Error::Corrupt("invalid document store path".into()))?;
        let fresh = Self::open(root)?;
        let changed = fresh.docs != self.docs;
        if changed {
            self.docs = fresh.docs;
            self.version += 1;
        }
        self.stamp = fresh.stamp;
        Ok(changed)
    }

    /// Insert or replace each document, then persist once.
    pub fn upsert_all(&mut self, docs: impl IntoIterator<Item = Document>) -> Result<()> {
        self.refresh()?;
        let mut changed = false;
        for doc in docs {
            doc.validate()?;
            let key = (doc.vessel.clone(), doc.id.clone());
            if self.docs.get(&key) != Some(&doc) {
                self.docs.insert(key, doc);
                changed = true;
            }
        }
        if changed {
            self.commit()?;
        }
        Ok(())
    }

    /// Remove the vessel's document; a missing id is idempotent.
    pub fn delete(&mut self, vessel: &str, id: &str) -> Result<()> {
        self.refresh()?;
        if self.docs.remove(&(vessel.into(), id.into())).is_some() {
            self.commit()?;
        }
        Ok(())
    }

    /// All documents in `(vessel, id)` order.
    pub fn iter(&self) -> impl Iterator<Item = &Document> {
        self.docs.values()
    }

    /// Number of stored documents.
    pub fn len(&self) -> usize {
        self.docs.len()
    }

    /// True when no documents are stored.
    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    /// Bumps on every persisted change.
    pub fn version(&self) -> u64 {
        self.version
    }

    fn commit(&mut self) -> Result<()> {
        self.version += 1;
        if let Some(path) = &self.path {
            let stored: Vec<StoredDocument> =
                self.docs.values().map(StoredDocument::from).collect();
            atomic_write_json(path, &stored)?;
            // Force the next read to verify the published snapshot, including a concurrent rename.
            self.stamp = None;
        }
        Ok(())
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
