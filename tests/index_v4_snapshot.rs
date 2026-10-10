use lume::bm25::{Bm25BuildOptions, Bm25Index, Section};
use lume::search::{IndexState, LoadedIndex, OpenEnvChecks, SearchMode, SearchOptions};
use std::collections::HashMap;

#[test]
fn published_v4_snapshot_matches_legacy_and_resident_reload() {
    let root = std::env::temp_dir().join(format!("lume-v4-snapshot-{}", lume::uuid_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let sections = vec![Section {
        title: "Bilge".into(),
        body: "bilge pump water".into(),
        filename: Some("boat.txt".into()),
        line_number: 1,
        entities: vec![],
    }];
    let state = IndexState {
        format_version: 1,
        target_dir: "corpus".into(),
        db_dir: root.to_string_lossy().into_owned(),
        semantic_enabled: false,
        ollama_entities: false,
        ollama_model: String::new(),
        ollama_url: String::new(),
        tag_dict_path: None,
        semantic_session_id: None,
        cached_files: HashMap::from([("boat.txt".into(), (123, sections.clone()))]),
        stemmed: false,
        keep_hyphens: false,
    };
    let index = Bm25Index::build_with_options(
        sections,
        None,
        Bm25BuildOptions {
            stemmed: false,
            ..Default::default()
        },
    );
    let spelling = lume::spelling::SpellIndex::build(
        &[],
        &index.posting_lists.keys().cloned().collect::<Vec<_>>(),
    );
    let graph = lume::semantic_mesh::EntityGraph::build(
        &index.entity_posting_lists,
        &index.entity_kinds,
        &index.entity_labels,
        0.1,
        index.sections.len(),
    );
    let before = LoadedIndex::from_parts(index.clone());
    lume::index_binary::snapshot::publish(&root, &state, index.clone(), &spelling, &graph, None)
        .unwrap();
    assert!(!root.join("bm25.json").exists() && !root.join("state.json").exists());
    let after = LoadedIndex::open_with_checks(&root, OpenEnvChecks::default()).unwrap();
    let options = SearchOptions {
        mode: SearchMode::LexicalOnly,
        graph_beta: 0.0,
        ..Default::default()
    };
    assert_eq!(
        serde_json::to_value(lume::search::search(&before, "bilge", &options).unwrap()).unwrap(),
        serde_json::to_value(lume::search::search(&after, "bilge", &options).unwrap()).unwrap()
    );
    let restored = lume::index_binary::snapshot::restore_state(&root).unwrap();
    assert_eq!(
        serde_json::to_value(restored.cached_files).unwrap(),
        serde_json::to_value(&state.cached_files).unwrap()
    );
    assert!(after.state.as_ref().unwrap().cached_files.is_empty());
    // Clear the explicit analyzer check by using an unstemmed state and environment defaults.
    let cache = lume::resident_index::ResidentIndexCache::default();
    let first = cache.open(&root).unwrap();
    let second = cache.open(&root).unwrap();
    assert!(std::sync::Arc::ptr_eq(&first, &second));
    lume::index_binary::snapshot::publish(&root, &state, index, &spelling, &graph, None).unwrap();
    let refreshed = cache.open(&root).unwrap();
    assert!(!std::sync::Arc::ptr_eq(&first, &refreshed));
    // A corrupt pointer fails closed even if legacy files exist.
    lume::search::save_json(&root.join("state.json"), &state).unwrap();
    std::fs::write(root.join("index.json"), b"{broken").unwrap();
    assert!(LoadedIndex::open_with_checks(&root, OpenEnvChecks::default()).is_err());
    std::fs::remove_dir_all(root).unwrap();
}
