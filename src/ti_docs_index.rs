//! Debounced publication tracking for an ordinary Lume index.
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamp {
    modified: SystemTime,
    len: u64,
}

pub(crate) struct DocsIndex {
    pub(crate) root: PathBuf,
    published: Option<Stamp>,
    pending: Option<(Option<Stamp>, Instant)>,
}

impl DocsIndex {
    pub(crate) fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            published: stamp(root),
            pending: None,
        }
    }

    // Called before taking the query gate. Failed reloads retain the last working snapshot.
    pub(crate) fn ready(&mut self, now: Instant) -> bool {
        let current = stamp(&self.root);
        if current == self.published {
            self.pending = None;
            return false;
        }
        match &self.pending {
            Some((candidate, since)) if *candidate == current => {
                now.duration_since(*since) >= Duration::from_secs(2)
            }
            _ => {
                self.pending = Some((current, now));
                false
            }
        }
    }

    pub(crate) fn acknowledge(&mut self) {
        if let Some((candidate, _)) = self.pending.take() {
            self.published = candidate;
        }
    }

}

fn stamp(root: &Path) -> Option<Stamp> {
    // Old indexes have no publication manifest. Their BM25 file is the fallback marker.
    let marker = if root.join("manifest.json").is_file() {
        root.join("manifest.json")
    } else {
        root.join("bm25.json")
    };
    let metadata = std::fs::metadata(marker).ok()?;
    Some(Stamp {
        modified: metadata.modified().ok()?,
        len: metadata.len(),
    })
}

pub(crate) fn register(session: &ti_sql::SqlSession, root: &Path) -> Result<(), String> {
    if !root.join("bm25.json").exists() {
        let index = crate::LoadedIndex::from_parts(crate::bm25::Bm25Index::build(vec![], None));
        return crate::sql::register_index(session, std::sync::Arc::new(index))
            .map_err(|e| e.to_string());
    }
    crate::sql::register(session, root).map_err(|e| e.to_string())
}
