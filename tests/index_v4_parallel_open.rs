use lume::bm25::{Bm25BuildOptions, Bm25Index, Section};
use lume::search::{LoadedIndex, SearchMode, SearchOptions};
use std::collections::BTreeMap;

fn fixture() -> BTreeMap<String, Vec<u8>> {
    let sections = (0..256)
        .map(|n| Section {
            title: format!("Boat {n}"),
            body: format!(
                "bilge pump battery café wind water {}",
                "anchor ".repeat(n % 13 + 1)
            ),
            filename: Some(format!("boat-{n}.txt")),
            line_number: n + 1,
            entities: vec!["Lagoon".into()],
        })
        .collect();
    let mut index = Bm25Index::build_with_options(
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
    let mut segments = index.v4_segments().unwrap();
    segments.insert(
        "spelling.bin".into(),
        lume::index_binary::spelling::encode(&spelling).unwrap(),
    );
    segments
}
#[test]
fn all_worker_counts_preserve_decoded_state_and_exact_search_scores() {
    let segments = fixture();
    for deep in [false, true] {
        let (serial, spelling) =
            Bm25Index::from_v4_segments_with_spelling(&segments, deep, 1).unwrap();
        let expected = serde_json::to_value(&serial).unwrap();
        let serial = LoadedIndex::from_parts(serial);
        for workers in 2..=4 {
            let (parallel, parallel_spelling) =
                Bm25Index::from_v4_segments_with_spelling(&segments, deep, workers).unwrap();
            assert_eq!(serde_json::to_value(&parallel).unwrap(), expected);
            let parallel = LoadedIndex::from_parts(parallel);
            for query in [
                "bilge pump",
                "battery NOT wind",
                "café anchor",
                "Boat 255",
                "unknown",
            ] {
                let options = SearchOptions {
                    mode: SearchMode::LexicalOnly,
                    graph_beta: 0.0,
                    ..Default::default()
                };
                let a = lume::search::search(&serial, query, &options).unwrap();
                let b = lume::search::search(&parallel, query, &options).unwrap();
                let bits = |reply: &lume::search::SearchResults| {
                    reply
                        .hits
                        .iter()
                        .map(|hit| {
                            (
                                hit.section_index,
                                hit.score.to_bits(),
                                hit.bm25_score.to_bits(),
                            )
                        })
                        .collect::<Vec<_>>()
                };
                assert_eq!(bits(&a), bits(&b));
                assert_eq!(
                    serde_json::to_value(a).unwrap(),
                    serde_json::to_value(b).unwrap()
                );
            }
            let a = spelling.as_ref().unwrap();
            let b = parallel_spelling.as_ref().unwrap();
            assert_eq!(a.unique_words, b.unique_words);
            assert_eq!(a.avg_word_len.to_bits(), b.avg_word_len.to_bits());
            for word in ["bilg", "batter", "cafe", "anchr"] {
                let bits = |index: &lume::spelling::SpellIndex| {
                    index
                        .correct_word(word, 20)
                        .into_iter()
                        .map(|(word, score)| (word, score.to_bits()))
                        .collect::<Vec<_>>()
                };
                assert_eq!(bits(a), bits(b));
            }
        }
    }
}
#[test]
fn malformed_segments_fail_in_serial_and_parallel_modes() {
    let original = fixture();
    for name in [
        "sections.tbl",
        "text.bin",
        "forward-body.bin",
        "postings.bin",
        "spelling.bin",
        "profiles.bin",
        "terms.tbl",
    ] {
        for workers in 1..=4 {
            for deep in [false, true] {
                let mut bad = original.clone();
                bad.get_mut(name).unwrap().pop();
                assert!(
                    Bm25Index::from_v4_segments_with_spelling(&bad, deep, workers).is_err(),
                    "{name}/{workers}"
                );
                let mut bad = original.clone();
                bad.remove(name);
                if name == "spelling.bin" {
                    bad.insert("spelling.json".into(), b"{malformed".to_vec());
                }
                assert!(
                    Bm25Index::from_v4_segments_with_spelling(&bad, deep, workers).is_err(),
                    "missing {name}/{workers}"
                );
            }
        }
    }
}
