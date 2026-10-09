//! One resident ordinary search index, refreshed on index/dictionary metadata changes.
//! In-flight searches own an Arc snapshot; loading a new snapshot cannot mutate them.
//! `lume index` rewrites trigger reloads; rewrites with equal mtime and length are not detected.
use crate::search::LoadedIndex;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

#[derive(Clone, Debug, PartialEq, Eq)]
struct FileStamp {
    path: PathBuf,
    modified: Option<SystemTime>,
    length: u64,
    present: bool,
}

fn stamp(path: PathBuf) -> Result<FileStamp, String> {
    match std::fs::metadata(&path) {
        Ok(metadata) => Ok(FileStamp {
            path,
            modified: Some(metadata.modified().map_err(|error| error.to_string())?),
            length: metadata.len(),
            present: true,
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(FileStamp {
            path,
            modified: None,
            length: 0,
            present: false,
        }),
        Err(error) => Err(format!("Cannot inspect {}: {error}", path.display())),
    }
}

fn fingerprint(root: &Path, dictionary: Option<&Path>) -> Result<Vec<FileStamp>, String> {
    let mut result = [
        "state.json",
        "bm25.json",
        "spelling.json",
        "entity_graph.json",
    ]
    .into_iter()
    .map(|file| stamp(root.join(file)))
    .collect::<Result<Vec<_>, _>>()?;
    if let Some(dictionary) = dictionary {
        result.push(stamp(dictionary.to_path_buf())?);
    }
    Ok(result)
}

struct Entry {
    root: PathBuf,
    fingerprint: Vec<FileStamp>,
    dictionary: Option<PathBuf>,
    index: Arc<LoadedIndex>,
}

/// Bounded to one cached index. Changing databases replaces the cached entry;
/// existing searches may finish with their immutable snapshot.
#[derive(Default)]
pub struct ResidentIndexCache {
    entry: Mutex<Option<Entry>>,
}

impl ResidentIndexCache {
    pub fn open(&self, root: impl AsRef<Path>) -> Result<Arc<LoadedIndex>, String> {
        let root = std::fs::canonicalize(root.as_ref())
            .map_err(|error| format!("Cannot open index {}: {error}", root.as_ref().display()))?;
        // Serialize cold loads, preventing concurrent requests from loading
        // duplicate copies. Warm searches hold the lock only for stat calls.
        let mut entry = self
            .entry
            .lock()
            .map_err(|_| "Resident index lock poisoned")?;
        if let Some(cached) = entry.as_ref() {
            if cached.root == root
                && fingerprint(&root, cached.dictionary.as_deref())? == cached.fingerprint
            {
                return Ok(Arc::clone(&cached.index));
            }
        }
        for _ in 0..3 {
            // Discover the external dictionary before loading it. This extra
            // state read happens only on a cold load or refresh, never warm.
            let state: crate::search::IndexState =
                crate::search::load_json(&root.join("state.json"))?;
            let before_dictionary = state.tag_dict_path.as_ref().map(PathBuf::from);
            drop(state);
            let before = fingerprint(&root, before_dictionary.as_deref())?;
            let index = LoadedIndex::open(&root)?;
            let dictionary = index
                .state
                .as_ref()
                .and_then(|state| state.tag_dict_path.as_ref())
                .map(PathBuf::from);
            // The dictionary is external to the index directory. Reopen if its
            // metadata changes while it is read, just like the JSON files.
            let after = fingerprint(&root, dictionary.as_deref())?;
            if before == after {
                let index = Arc::new(index);
                *entry = Some(Entry {
                    root,
                    fingerprint: after,
                    dictionary,
                    index: Arc::clone(&index),
                });
                return Ok(index);
            }
        }
        Err("Index changed repeatedly while loading; retry the search".to_string())
    }
}

pub fn open(root: impl AsRef<Path>) -> Result<Arc<LoadedIndex>, String> {
    static CACHE: OnceLock<ResidentIndexCache> = OnceLock::new();
    CACHE.get_or_init(ResidentIndexCache::default).open(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bm25::{Bm25Index, Section};
    use crate::search::{save_json, search, IndexState, SearchMode, SearchOptions};
    use std::collections::HashMap;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "lume-resident-{}-{}",
                std::process::id(),
                crate::uuid_v4()
            ));
            std::fs::create_dir_all(&root).unwrap();
            let state = IndexState {
                target_dir: root.to_string_lossy().into_owned(),
                db_dir: root.to_string_lossy().into_owned(),
                semantic_enabled: false,
                ollama_entities: false,
                ollama_model: String::new(),
                ollama_url: String::new(),
                tag_dict_path: None,
                semantic_session_id: None,
                cached_files: HashMap::new(),
                stemmed: false,
                keep_hyphens: false,
            };
            save_json(&root.join("state.json"), &state).unwrap();
            let fixture = Self(root);
            fixture.write("captain", "original");
            fixture
        }
        fn write(&self, body: &str, title: &str) {
            let bm25 = Bm25Index::build(
                vec![Section {
                    title: title.to_string(),
                    body: body.to_string(),
                    line_number: 1,
                    filename: Some("paper.txt".to_string()),
                    entities: Vec::new(),
                }],
                None,
            );
            save_json(&self.0.join("bm25.json"), &bm25).unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn reuse_refresh_and_snapshot_rankings() {
        let fixture = Fixture::new();
        let cache = ResidentIndexCache::default();
        let first = cache.open(&fixture.0).unwrap();
        let second = cache.open(fixture.0.join(".")).unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        let options = SearchOptions {
            mode: SearchMode::LexicalOnly,
            graph_beta: 0.0,
            ..Default::default()
        };
        let uncached = LoadedIndex::open(&fixture.0).unwrap();
        assert_eq!(
            serde_json::to_value(search(&first, "captain", &options).unwrap()).unwrap(),
            serde_json::to_value(search(&uncached, "captain", &options).unwrap()).unwrap()
        );
        fixture.write("ancient treasure beneath the waves", "updated paper");
        let updated = cache.open(&fixture.0).unwrap();
        assert!(!Arc::ptr_eq(&first, &updated));
        assert_eq!(search(&first, "captain", &options).unwrap().hits.len(), 1);
        assert!(search(&updated, "captain", &options)
            .unwrap()
            .hits
            .is_empty());
        assert_eq!(
            search(&updated, "treasure", &options).unwrap().hits.len(),
            1
        );
        std::fs::remove_file(fixture.0.join("bm25.json")).unwrap();
        assert!(cache.open(&fixture.0).is_err());
    }

    #[test]
    fn external_dictionary_changes_refresh_the_snapshot() {
        let fixture = Fixture::new();
        let dictionary = fixture.0.join("terms.csv");
        std::fs::write(&dictionary, "phrase,action\ncaptain,CAPTAIN\n").unwrap();
        let mut state: IndexState =
            crate::search::load_json(&fixture.0.join("state.json")).unwrap();
        state.tag_dict_path = Some(dictionary.to_string_lossy().into_owned());
        save_json(&fixture.0.join("state.json"), &state).unwrap();
        let cache = ResidentIndexCache::default();
        let first = cache.open(&fixture.0).unwrap();
        assert_eq!(first.tagger.as_ref().unwrap().record_count(), 1);
        std::fs::write(
            &dictionary,
            "phrase,action\ncaptain,CAPTAIN\ntreasure,TREASURE\n",
        )
        .unwrap();
        let updated = cache.open(&fixture.0).unwrap();
        assert!(!Arc::ptr_eq(&first, &updated));
        assert_eq!(updated.tagger.as_ref().unwrap().record_count(), 2);
    }

    #[test]
    fn concurrent_cold_requests_share_one_snapshot() {
        let fixture = Fixture::new();
        let cache = Arc::new(ResidentIndexCache::default());
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let workers = (0..8)
            .map(|_| {
                let cache = Arc::clone(&cache);
                let barrier = Arc::clone(&barrier);
                let root = fixture.0.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    cache.open(root).unwrap()
                })
            })
            .collect::<Vec<_>>();
        let snapshots = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>();
        assert!(snapshots
            .iter()
            .all(|index| Arc::ptr_eq(index, &snapshots[0])));
    }

    #[test]
    fn database_switch_replaces_the_single_cache_entry() {
        let first = Fixture::new();
        let second = Fixture::new();
        let cache = ResidentIndexCache::default();
        let a = cache.open(&first.0).unwrap();
        cache.open(&second.0).unwrap();
        let reloaded = cache.open(&first.0).unwrap();
        assert!(!Arc::ptr_eq(&a, &reloaded));
    }
}
