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
    let borrowed = lume::index_binary::snapshot::publish(
        &root,
        &state,
        index.clone(),
        &spelling,
        &graph,
        None,
    )
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
    // Same text generation, new head: every reader must observe replacement entities.
    let replacement = |entities: &[&str]| lume::index_binary::overlays::Replacement {
        section: 0,
        source_hash: lume::index_binary::overlays::source_hash(&index.sections[0]),
        entities: entities.iter().map(|value| value.to_string()).collect(),
    };
    lume::index_binary::overlays::publish(&root, vec![replacement(&["Lagoon"])], |_| Ok(()))
        .unwrap();
    let overlaid = cache.open(&root).unwrap();
    assert!(!std::sync::Arc::ptr_eq(&first, &overlaid));
    assert_eq!(overlaid.bm25.entity_posting_lists["lagoon"].len(), 1);
    assert_eq!(
        lume::index_binary::snapshot::load_bm25(&root)
            .unwrap()
            .sections[0]
            .entities,
        ["Lagoon"]
    );
    let overlaid_graph: lume::semantic_mesh::EntityGraph =
        lume::index_binary::snapshot::component(&root, "entity_graph.json").unwrap();
    assert!(overlaid_graph.nodes.iter().any(|node| node.id == "lagoon"));
    let original = lume::search::search(&before, "bilge", &options).unwrap();
    let mut updated = lume::search::search(&overlaid, "bilge", &options).unwrap();
    assert_eq!(updated.hits[0].entities, ["Lagoon"]);
    assert_eq!(
        updated.hits[0].score.to_bits(),
        original.hits[0].score.to_bits()
    );
    assert_eq!(
        updated.hits[0].bm25_score.to_bits(),
        original.hits[0].bm25_score.to_bits()
    );
    updated.hits[0].entities.clear();
    assert_eq!(
        serde_json::to_value(original).unwrap(),
        serde_json::to_value(updated).unwrap()
    );
    lume::index_binary::overlays::publish(&root, vec![replacement(&[])], |_| Ok(())).unwrap();
    let emptied = cache.open(&root).unwrap();
    assert!(!emptied.bm25.entity_posting_lists.contains_key("lagoon"));
    assert_eq!(
        lume::index_binary::snapshot::restore_state(&root)
            .unwrap()
            .cached_files["boat.txt"]
            .1[0]
            .entities,
        ["__LUME_PROCESSED__"]
    );
    let owned = lume::index_binary::snapshot::publish_owned(
        &root,
        state.clone(),
        index,
        &spelling,
        &graph,
        None,
    )
    .unwrap();
    assert_eq!(
        lume::index_binary::generation::read_segments(&root, &borrowed).unwrap(),
        lume::index_binary::generation::read_segments(&root, &owned).unwrap()
    );
    let refreshed = cache.open(&root).unwrap();
    assert!(!std::sync::Arc::ptr_eq(&first, &refreshed));
    assert!(owned.segments.contains_key("spelling.bin"));
    assert!(!owned.segments.contains_key("spelling.json"));
    let binary_spell: lume::spelling::SpellIndex =
        lume::index_binary::snapshot::component(&root, "spelling.json").unwrap();
    assert_eq!(binary_spell.unique_words, spelling.unique_words);
    assert_eq!(
        binary_spell.avg_word_len.to_bits(),
        spelling.avg_word_len.to_bits()
    );
    // Older v4 generations keep working without a binary spelling segment.
    let mut legacy_segments = lume::index_binary::generation::read_segments(&root, &owned).unwrap();
    legacy_segments.remove("spelling.bin");
    legacy_segments.insert(
        "spelling.json".into(),
        serde_json::to_vec(&spelling).unwrap(),
    );
    let mut legacy_manifest = owned.clone();
    legacy_manifest.generation = lume::uuid_v4();
    let legacy_manifest =
        lume::index_binary::generation::publish(&root, legacy_manifest, &legacy_segments, |_| {
            Ok(())
        })
        .unwrap();
    let legacy_loaded = LoadedIndex::open_with_checks(&root, OpenEnvChecks::default()).unwrap();
    assert_eq!(
        legacy_loaded.spelling.as_ref().unwrap().unique_words,
        spelling.unique_words
    );
    let mut ambiguous = legacy_manifest;
    ambiguous.segments.insert(
        "spelling.bin".into(),
        owned.segments["spelling.bin"].clone(),
    );
    assert!(ambiguous.validate().is_err());
    // A corrupt pointer fails closed even if legacy files exist.
    lume::search::save_json(&root.join("state.json"), &state).unwrap();
    std::fs::write(root.join("index.json"), b"{broken").unwrap();
    assert!(LoadedIndex::open_with_checks(&root, OpenEnvChecks::default()).is_err());
    std::fs::remove_dir_all(root).unwrap();
}
