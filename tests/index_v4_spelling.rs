use lume::index_binary::{codec::Writer, spelling};
use lume::spelling::SpellIndex;

fn fixture() -> SpellIndex {
    let phrases = vec![
        "Bilge pump".into(),
        "Monte Cristo".into(),
        "café café".into(),
    ];
    let words: Vec<_> = [
        "this",
        "that",
        "battery",
        "temperature",
        "banana",
        "aaaaaa",
        "café",
        "航海",
        "a",
    ]
    .iter()
    .map(|word| word.as_bytes().to_vec())
    .collect();
    SpellIndex::build(&phrases, &words)
}
fn equivalent(expected: &SpellIndex, actual: &SpellIndex) {
    assert_eq!(actual.unique_words, expected.unique_words);
    assert_eq!(actual.vocab_set, expected.vocab_set);
    assert_eq!(actual.word_lens, expected.word_lens);
    assert_eq!(actual.trigram_dfs, expected.trigram_dfs);
    assert_eq!(
        actual.avg_word_len.to_bits(),
        expected.avg_word_len.to_bits()
    );
    assert_eq!(actual.num_words, expected.num_words);
    assert_eq!(
        actual.trigram_postings.len(),
        expected.trigram_postings.len()
    );
    for (key, posting) in &expected.trigram_postings {
        assert_eq!(actual.trigram_postings[key].iter(), posting.iter());
    }
    for word in [
        "htis",
        "bilg",
        "pmp",
        "cafe",
        "batter",
        "temperture",
        "monte",
        "aaaaa",
        "航",
        "",
        "unknown",
    ] {
        for limit in [0, 1, 5, 20] {
            let bits = |index: &SpellIndex| {
                index
                    .correct_word(word, limit)
                    .into_iter()
                    .map(|(word, score)| (word, score.to_bits()))
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                bits(actual),
                bits(expected),
                "correction score bits for {word}"
            );
        }
    }
}
#[test]
fn binary_preserves_word_ids_unicode_postings_and_correction_score_bits() {
    let index = fixture();
    let json: SpellIndex = serde_json::from_slice(&serde_json::to_vec(&index).unwrap()).unwrap();
    let bytes = spelling::encode(&index).unwrap();
    equivalent(&json, &spelling::decode(&bytes).unwrap());
    assert_eq!(
        spelling::encode(&spelling::decode(&bytes).unwrap()).unwrap(),
        bytes
    );
    let empty = SpellIndex::build(&[], &[]);
    equivalent(
        &empty,
        &spelling::decode(&spelling::encode(&empty).unwrap()).unwrap(),
    );
}
#[test]
fn large_shared_trigram_postings_are_smaller_than_json() {
    let words: Vec<_> = (0..5000)
        .map(|n| format!("navigation{n:05}").into_bytes())
        .collect();
    let index = SpellIndex::build(&[], &words);
    let bytes = spelling::encode(&index).unwrap();
    let json = serde_json::to_vec(&index).unwrap();
    assert!(
        bytes.len() < json.len(),
        "{} vs {}",
        bytes.len(),
        json.len()
    );
    equivalent(&index, &spelling::decode(&bytes).unwrap());
    println!(
        "spelling fixture: binary {} bytes, JSON {} bytes",
        bytes.len(),
        json.len()
    );
}
#[test]
fn malformed_header_truncation_counts_and_posting_ids_fail_closed() {
    let bytes = spelling::encode(&fixture()).unwrap();
    for end in 0..bytes.len() {
        assert!(spelling::decode(&bytes[..end]).is_err(), "truncation {end}");
    }
    for offset in [0, 8, 10, 12, 16, 24, 32, 36, 40] {
        let mut corrupt = bytes.clone();
        corrupt[offset] ^= 0xff;
        assert!(
            spelling::decode(&corrupt).is_err(),
            "header/statistic {offset}"
        );
    }
    fn invalid(id: u64, length: u64) -> Vec<u8> {
        let mut writer = Writer::new(9, 1, 0);
        writer.f64(3.0);
        writer.varint(1); // trigram count
        writer.varint(3);
        writer.raw(b"cat");
        writer.varint(length);
        writer.varint(3);
        writer.raw(b"_ca");
        writer.varint(1);
        writer.varint(id);
        writer.finish().unwrap()
    }
    assert!(spelling::decode(&invalid(99, 3)).is_err());
    assert!(spelling::decode(&invalid(0, 0)).is_err());
    assert!(spelling::decode(&invalid(0, 4)).is_err());
    let mut valid = invalid(0, 3);
    valid.push(0);
    let length = valid.len() as u64;
    valid[16..24].copy_from_slice(&length.to_le_bytes());
    assert!(spelling::decode(&valid).is_err(), "trailing bytes");
    let mut invalid_index = fixture();
    invalid_index.unique_words[1] = invalid_index.unique_words[0].clone();
    assert!(spelling::encode(&invalid_index).is_err());
}
