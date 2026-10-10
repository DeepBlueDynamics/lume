use lume::bm25::{Bm25BuildOptions, Bm25Index, Bm25Params, SearchVariant, Section};

fn fixture(count: usize) -> Bm25Index {
    Bm25Index::build_with_options(
        (0..count)
            .map(|doc| Section {
                title: if doc % 3 == 0 {
                    "Bilge café"
                } else {
                    "Anchor"
                }
                .into(),
                body: format!(
                    "{} {}",
                    "pump ".repeat(doc % 7 + 1),
                    if doc % 5 == 0 { "wind" } else { "anchor water" }
                ),
                line_number: doc + 1,
                filename: if doc % 11 == 0 {
                    None
                } else {
                    Some(format!("doc-{}.txt", doc / 13))
                },
                entities: Vec::new(),
            })
            .collect(),
        None,
        Bm25BuildOptions::default(),
    )
}

fn assert_rankings(legacy: &Bm25Index, decoded: &Bm25Index) {
    for variant in [
        SearchVariant::Classic,
        SearchVariant::Plus,
        SearchVariant::L,
    ] {
        for floor in [0.0, 0.5, 1.0] {
            let params = Bm25Params {
                coord_floor: floor,
                ..Default::default()
            };
            for query in [
                "anchor",
                "pump pump café",
                "water wind",
                "anchor NOT wind",
                "missing",
            ] {
                for limit in [0, 1, 7, 100] {
                    let expected = legacy.search_top_k(query, variant, &params, None, limit);
                    let actual = decoded.search_top_k(query, variant, &params, None, limit);
                    assert_eq!(actual.len(), expected.len());
                    for (actual, expected) in actual.iter().zip(&expected) {
                        assert_eq!(actual.section_index, expected.section_index, "{query}");
                        assert_eq!(actual.score.to_bits(), expected.score.to_bits(), "{query}");
                    }
                }
            }
        }
    }
}

#[test]
fn binary_core_empty_index_round_trips() {
    let mut empty = fixture(0);
    let before = serde_json::to_value(&empty).unwrap();
    let decoded = Bm25Index::from_v4_segments(&empty.v4_segments().unwrap()).unwrap();
    assert_eq!(serde_json::to_value(&decoded).unwrap(), before);
    assert!(decoded
        .search_top_k(
            "pump",
            SearchVariant::Classic,
            &Bm25Params::default(),
            None,
            3
        )
        .is_empty());
}

#[test]
fn binary_core_preserves_legacy_json_and_every_ranking_bit() {
    let legacy = fixture(144);
    let before = serde_json::to_value(&legacy).unwrap();
    let mut compact = legacy.clone();
    let segments = compact.v4_segments().unwrap();
    assert!(compact.title_tfs.is_empty() && compact.body_tfs.is_empty());
    assert_eq!(serde_json::to_value(&compact).unwrap(), before);
    let decoded = Bm25Index::from_v4_segments(&segments).unwrap();
    assert!(decoded.title_tfs.is_empty() && decoded.body_tfs.is_empty());
    assert_eq!(serde_json::to_value(&decoded).unwrap(), before);
    assert_rankings(&legacy, &decoded);
    let reopened: Bm25Index = serde_json::from_value(before).unwrap();
    assert_rankings(&reopened, &decoded);
}

#[test]
fn binary_core_keeps_postings_above_native_container_boundary() {
    let legacy = fixture(65539);
    let mut compact = legacy.clone();
    let decoded = Bm25Index::from_v4_segments(&compact.v4_segments().unwrap()).unwrap();
    let mut allow = lume::fast_retrieval::MiniRoaring::new();
    for doc in [65534, 65535, 65536, 65537, 65538] {
        allow.insert(doc);
    }
    for query in ["anchor", "pump café", "anchor NOT wind"] {
        let params = Bm25Params::default();
        let expected = legacy.search_top_k_filtered(
            query,
            SearchVariant::Classic,
            &params,
            None,
            10,
            Some(&allow),
        );
        let actual = decoded.search_top_k_filtered(
            query,
            SearchVariant::Classic,
            &params,
            None,
            10,
            Some(&allow),
        );
        assert_eq!(actual.len(), expected.len());
        for (actual, expected) in actual.iter().zip(&expected) {
            assert_eq!(actual.section_index, expected.section_index);
            assert_eq!(actual.score.to_bits(), expected.score.to_bits());
        }
    }
}

#[test]
fn binary_core_rejects_missing_segments_and_semantic_disagreements() {
    let mut index = fixture(12);
    let segments = index.v4_segments().unwrap();
    for key in segments.keys() {
        let mut missing = segments.clone();
        missing.remove(key);
        assert!(Bm25Index::from_v4_segments(&missing).is_err(), "{key}");
    }
    let mut bad = segments.clone();
    // First document's title length, still a valid fixed-width segment.
    bad.get_mut("profiles.bin").unwrap()[40] ^= 1;
    assert!(Bm25Index::from_v4_segments(&bad).is_err());
    let mut bad = segments.clone();
    // First document's prime mask; it cannot silently exclude candidates.
    bad.get_mut("profiles.bin").unwrap()[56] ^= 1;
    assert!(Bm25Index::from_v4_segments(&bad).is_err());
    let mut bad = segments.clone();
    let mut settings: serde_json::Value = serde_json::from_slice(&bad["bm25-aux.json"]).unwrap();
    settings["num_docs"] = 13.into();
    bad.insert(
        "bm25-aux.json".into(),
        serde_json::to_vec(&settings).unwrap(),
    );
    assert!(Bm25Index::from_v4_segments(&bad).is_err());
}
