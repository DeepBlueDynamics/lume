//! Opt-in fusion experiments. Legacy blends never enter this module.
use std::collections::HashMap;
use crate::bm25::SearchHit;
use crate::hybrid::HybridHit;

pub(crate) fn enabled(mode: Option<&str>) -> bool {
    matches!(mode, Some("rrf" | "normalized-v2" | "vector"))
}

pub(crate) fn fuse(
    lexical: &[SearchHit], semantic: &HashMap<usize, f64>, mode: &str,
    k: f64, alpha: f64,
) -> Vec<HybridHit> {
    assert!(enabled(Some(mode)));
    let lexical: HashMap<usize, f64> = lexical.iter().map(|hit| (hit.section_index, hit.score)).collect();
    let lexical_ranks = ranks(&lexical);
    let semantic_ranks = ranks(semantic);
    let mut ids: Vec<usize> = lexical.keys().chain(semantic.keys()).copied().collect();
    ids.sort_unstable();
    ids.dedup();
    if mode == "vector" {
        ids.retain(|id| semantic.contains_key(id));
    }
    let lexical_range = range(ids.iter().map(|id| lexical.get(id).copied().unwrap_or(0.0)));
    let semantic_range = range(semantic.values().copied());
    let mut hits: Vec<_> = ids.into_iter().map(|id| {
        let score = if mode == "vector" {
            semantic[&id]
        } else if mode == "normalized-v2" {
            scale(lexical.get(&id).copied().unwrap_or(0.0), lexical_range)
                + alpha * semantic.get(&id).map_or(0.0, |score| scale(*score, semantic_range))
        } else {
            lexical_ranks.get(&id).map_or(0.0, |rank| 1.0 / (k + *rank as f64))
                + semantic_ranks.get(&id).map_or(0.0, |rank| 1.0 / (k + *rank as f64))
        };
        HybridHit {
            section_index: id, bm25_score: lexical.get(&id).copied().unwrap_or(0.0),
            semantic_score: semantic.get(&id).copied().unwrap_or(0.0),
            skg_score: 0.0, hybrid_score: score, boosted: semantic.contains_key(&id),
        }
    }).collect();
    hits.sort_by(|a, b| b.hybrid_score.total_cmp(&a.hybrid_score)
        .then_with(|| a.section_index.cmp(&b.section_index)));
    hits
}

fn range(values: impl Iterator<Item = f64>) -> (f64, f64) {
    values.fold((f64::INFINITY, f64::NEG_INFINITY), |(min, max), value| {
        (min.min(value), max.max(value))
    })
}

// Constant or empty lists contribute zero; missing semantic candidates also contribute zero.
fn scale(value: f64, (min, max): (f64, f64)) -> f64 {
    if max > min { (value - min) / (max - min) } else { 0.0 }
}

fn ranks(scores: &HashMap<usize, f64>) -> HashMap<usize, usize> {
    let mut rows: Vec<_> = scores.iter().collect();
    rows.sort_by(|(a, sa), (b, sb)| sb.total_cmp(sa).then_with(|| a.cmp(b)));
    rows.into_iter().enumerate().map(|(rank, (id, _))| (*id, rank + 1)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rrf_matches_hand_calculation_and_breaks_ties_by_section() {
        let lexical = vec![SearchHit { section_index: 0, score: 9.0 },
                           SearchHit { section_index: 1, score: 8.0 }];
        let semantic = HashMap::from([(1, 0.9), (0, 0.8), (2, 0.7)]);
        let hits = fuse(&lexical, &semantic, "rrf", 60.0, 0.7);
        assert_eq!(hits.iter().map(|hit| hit.section_index).collect::<Vec<_>>(), vec![0, 1, 2]);
        assert_eq!(hits[0].hybrid_score.to_bits(), (1.0f64 / 61.0 + 1.0 / 62.0).to_bits());
        assert_eq!(hits[2].hybrid_score.to_bits(), (1.0f64 / 63.0).to_bits());
        assert_eq!(ranks(&HashMap::from([(5, 1.0), (3, 1.0)]))[&3], 1);
    }
    #[test]
    fn normalized_v2_scales_both_lists_and_handles_constant_scores() {
        let lexical = vec![SearchHit { section_index: 0, score: 10.0 },
                           SearchHit { section_index: 1, score: 5.0 }];
        let semantic = HashMap::from([(1, 0.4), (2, 0.6)]);
        let hits = fuse(&lexical, &semantic, "normalized-v2", 60.0, 2.0);
        assert_eq!(hits.iter().map(|hit| hit.section_index).collect::<Vec<_>>(), vec![2, 0, 1]);
        assert_eq!(hits[0].hybrid_score, 2.0);
        assert_eq!(hits[1].hybrid_score, 1.0);
        assert_eq!(hits[2].hybrid_score, 0.5);
        assert_eq!(scale(0.4, (0.4, 0.4)), 0.0);
    }
    #[test]
    fn vector_only_excludes_lexical_only_candidates() {
        let lexical = vec![SearchHit { section_index: 0, score: 100.0 }];
        let semantic = HashMap::from([(1, 0.8), (2, 0.8)]);
        let hits = fuse(&lexical, &semantic, "vector", 60.0, 1.0);
        assert_eq!(hits.iter().map(|hit| hit.section_index).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(hits[0].hybrid_score, 0.8);
    }
    #[test]
    fn legacy_modes_do_not_enable_experimental_fusion() {
        for mode in [None, Some("normalized"), Some("multiplicative"), Some("")] {
            assert!(!enabled(mode));
        }
    }
}
