//! SKG (Semantic Knowledge Graph) traversal — Primitive 6 wired into the
//! Primitive 7 HATCHERIK boost.
//!
//! The entity co-occurrence graph (`EntityGraph`, built from pairwise roaring-
//! bitset intersections — "counting the counts of things") is otherwise a
//! write-only export. This module turns it into a query-time ranking signal:
//! resolve the entities named in the query, walk one hop to their strongest
//! co-occurring neighbors, and score every section by how much related-entity
//! mass it contains. The resulting per-section score feeds the multiplicative
//! HATCHERIK blend as a third term. It runs fully locally — no remote vector
//! service or token required.

use std::collections::{HashMap, HashSet};

use crate::bm25::{Bm25Index, SearchHit};
use crate::semantic_mesh::EntityGraph;
use crate::tokenize;

/// Sections whose normalized SKG mass is at least this are allowed to enter the
/// result set even without a lexical/semantic match (recall expansion). Keeps
/// strongly-related sections while avoiding a flood of weakly-touched ones.
pub const SKG_EXPAND_MIN: f64 = 0.5;

/// Tuning knobs for the SKG boost.
#[derive(Debug, Clone)]
pub struct SkgBoostParams {
    /// Blend weight of the SKG term in `bm25 * (1 + .. + beta*skg)`.
    pub beta: f64,
    /// Ignore neighbor edges weaker than this Jaccard similarity.
    pub min_jaccard: f64,
    /// Max neighbors expanded per seed entity.
    pub top_k: usize,
    /// Neighbor weight = `decay * edge.similarity`.
    pub decay: f64,
    /// Longest query phrase (in tokens) to try to match as a single entity.
    pub max_ngram: usize,
}

impl Default for SkgBoostParams {
    fn default() -> Self {
        Self { beta: 0.4, min_jaccard: 0.05, top_k: 8, decay: 0.5, max_ngram: 4 }
    }
}

/// Normalized form of an entity key or query phrase: folded, lowercased tokens
/// joined by single spaces. Uses the same [`tokenize`] as the index, so a query
/// term like "mercedes" matches the stored Ollama key "mercédès", and
/// "edmond dantes" matches "edmond dantès".
fn normalize_phrase(s: &str) -> String {
    tokenize(s)
        .iter()
        .map(|t| String::from_utf8_lossy(&t.bytes).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Builds `normalized_phrase -> entity_key` over every SKG node (the keys of
/// `entity_posting_lists`). Built once per query.
fn build_entity_lookup(index: &Bm25Index) -> HashMap<String, String> {
    let mut lookup = HashMap::with_capacity(index.entity_posting_lists.len());
    for key in index.entity_posting_lists.keys() {
        let norm = normalize_phrase(key);
        if !norm.is_empty() {
            lookup.entry(norm).or_insert_with(|| key.clone());
        }
    }
    lookup
}

/// Resolves the entities named in `query` to SKG node keys. Slides n-grams from
/// longest (`max_ngram`) down to single tokens; longest match wins and consumed
/// tokens are not reused.
pub fn resolve_query_entities(index: &Bm25Index, query: &str, max_ngram: usize) -> Vec<String> {
    let lookup = build_entity_lookup(index);
    let toks: Vec<String> = tokenize(query)
        .iter()
        .map(|t| String::from_utf8_lossy(&t.bytes).into_owned())
        .collect();
    let n = toks.len();
    let mut used = vec![false; n];
    let mut seeds: Vec<String> = Vec::new();

    let mut i = 0;
    while i < n {
        if used[i] {
            i += 1;
            continue;
        }
        let max_len = max_ngram.min(n - i);
        let mut matched = false;
        for len in (1..=max_len).rev() {
            if (i..i + len).any(|j| used[j]) {
                continue;
            }
            let phrase = toks[i..i + len].join(" ");
            if let Some(key) = lookup.get(&phrase) {
                if !seeds.contains(key) {
                    seeds.push(key.clone());
                }
                for j in i..i + len {
                    used[j] = true;
                }
                i += len;
                matched = true;
                break;
            }
        }
        if !matched {
            i += 1;
        }
    }
    seeds
}

/// Adjacency map from the precomputed SKG edges: `entity_key -> [(neighbor,
/// similarity)]`, each list sorted descending by similarity.
pub fn build_adjacency(graph: &EntityGraph) -> HashMap<String, Vec<(String, f64)>> {
    let mut adj: HashMap<String, Vec<(String, f64)>> = HashMap::new();
    for edge in &graph.edges {
        adj.entry(edge.source.clone())
            .or_default()
            .push((edge.target.clone(), edge.similarity));
        adj.entry(edge.target.clone())
            .or_default()
            .push((edge.source.clone(), edge.similarity));
    }
    for list in adj.values_mut() {
        list.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    }
    adj
}

/// The outcome of an SKG walk: per-section boost in `[0,1]`, the resolved seed
/// entities, and the expanded neighbor weights (both for the debug trace).
pub struct SkgWalk {
    pub scores: HashMap<usize, f64>,
    pub seeds: Vec<String>,
    /// Non-seed entities reached by the walk, sorted by weight descending.
    pub expanded: Vec<(String, f64)>,
}

/// Walks the SKG from the query's entities and scores each section by the mass
/// of related entities it contains. Seeds carry weight 1.0; one-hop neighbors
/// carry `decay * similarity` (capped, not summed, so a shared hub neighbor
/// can't dominate). Section scores are normalized to `[0,1]`.
pub fn compute_skg_scores(
    index: &Bm25Index,
    graph: &EntityGraph,
    query: &str,
    params: &SkgBoostParams,
) -> SkgWalk {
    let seeds = resolve_query_entities(index, query, params.max_ngram);
    let adj = build_adjacency(graph);

    // entity_key -> weight. Seeds pinned at 1.0; neighbors take the max of
    // their decayed edge weights across all seeds that reach them.
    let mut weights: HashMap<String, f64> = HashMap::new();
    for s in &seeds {
        weights.insert(s.clone(), 1.0);
    }
    let seed_set: HashSet<&String> = seeds.iter().collect();
    for s in &seeds {
        if let Some(neigh) = adj.get(s) {
            for (nbr, sim) in neigh.iter().filter(|(_, sim)| *sim >= params.min_jaccard).take(params.top_k) {
                if seed_set.contains(nbr) {
                    continue; // already pinned at 1.0
                }
                let w = params.decay * sim;
                let e = weights.entry(nbr.clone()).or_insert(0.0);
                if w > *e {
                    *e = w;
                }
            }
        }
    }

    // Accumulate per-section mass: each weighted entity contributes its weight
    // to every section it appears in.
    let mut raw: HashMap<usize, f64> = HashMap::new();
    for (ent, w) in &weights {
        if let Some(list) = index.entity_posting_lists.get(ent) {
            for sid in list.iter() {
                *raw.entry(sid as usize).or_insert(0.0) += *w;
            }
        }
    }

    let max_mass = raw.values().cloned().fold(0.0_f64, f64::max);
    let scores: HashMap<usize, f64> = if max_mass > 0.0 {
        raw.into_iter().map(|(k, v)| (k, v / max_mass)).collect()
    } else {
        HashMap::new()
    };

    let mut expanded: Vec<(String, f64)> = weights
        .iter()
        .filter(|(k, _)| !seed_set.contains(k))
        .map(|(k, v)| (k.clone(), *v))
        .collect();
    expanded.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    SkgWalk { scores, seeds, expanded }
}

/// Applies the SKG boost to a list of lexical `SearchHit`s in place: existing
/// hits get `score *= 1 + beta*skg`, and strongly-related sections not already
/// present are appended (scored `beta*skg`, so they rank below real lexical
/// matches). Re-sorts by score descending. Mirrors how `blend_hybrid_scores`
/// folds in semantic-only candidates.
pub fn apply_skg_boost(
    hits: &mut Vec<SearchHit>,
    skg_scores: &HashMap<usize, f64>,
    beta: f64,
) {
    if beta <= 0.0 || skg_scores.is_empty() {
        return;
    }
    let present: HashSet<usize> = hits.iter().map(|h| h.section_index).collect();
    for h in hits.iter_mut() {
        if let Some(s) = skg_scores.get(&h.section_index) {
            h.score *= 1.0 + beta * s;
        }
    }
    for (idx, s) in skg_scores {
        if *s >= SKG_EXPAND_MIN && !present.contains(idx) {
            hits.push(SearchHit { section_index: *idx, score: beta * s });
        }
    }
    hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bm25::{Bm25Index, Section};
    use crate::semantic_mesh::EntityGraph;

    fn sec(entities: &[&str]) -> Section {
        Section {
            title: "t".to_string(),
            body: "some body text".to_string(),
            line_number: 1,
            filename: None,
            entities: entities.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// Three sections wiring Mercédès to Edmond (co-occur in s0) and to Fernand
    /// (co-occur in s1); Fernand also co-occurs with Danglars (s2).
    fn fixture() -> (Bm25Index, EntityGraph) {
        let sections = vec![
            sec(&["Mercédès", "Edmond Dantès"]),
            sec(&["Mercédès", "Fernand"]),
            sec(&["Fernand", "Danglars"]),
        ];
        let bm25 = Bm25Index::build(sections, None);
        let graph = EntityGraph::build(&bm25.entity_posting_lists, &bm25.entity_kinds, &bm25.entity_labels, 0.0);
        (bm25, graph)
    }

    #[test]
    fn resolves_folded_single_and_multiword_entities() {
        let (bm25, _) = fixture();
        // "mercedes" (no accent) must resolve to the stored "mercédès" key.
        assert_eq!(resolve_query_entities(&bm25, "mercedes", 4), vec!["mercédès".to_string()]);
        // Multi-word, accent-folded.
        assert_eq!(resolve_query_entities(&bm25, "edmond dantes", 4), vec!["edmond dantès".to_string()]);
        // No entity words → no seeds.
        assert!(resolve_query_entities(&bm25, "the and of", 4).is_empty());
    }

    #[test]
    fn skg_walk_ranks_seed_sections_above_neighbor_only_sections() {
        let (bm25, graph) = fixture();
        let params = SkgBoostParams::default();
        let walk = compute_skg_scores(&bm25, &graph, "mercedes", &params);

        assert_eq!(walk.seeds, vec!["mercédès".to_string()]);
        // Sections containing the seed (s0, s1) outrank the neighbor-only s2.
        let s0 = walk.scores[&0];
        let s1 = walk.scores[&1];
        let s2 = walk.scores[&2];
        assert!(s0 > s1, "s0 {} should exceed s1 {}", s0, s1);
        assert!(s1 > s2, "s1 {} should exceed s2 {}", s1, s2);
        assert!(s2 > 0.0, "neighbor-only section should still be boosted");
        // Normalized to [0,1] with the top section at 1.0.
        assert!((s0 - 1.0).abs() < 1e-9);
    }

    #[test]
    fn beta_zero_yields_no_scores() {
        let (bm25, graph) = fixture();
        let params = SkgBoostParams { beta: 0.0, ..Default::default() };
        // compute still runs, but apply_skg_boost with beta=0 is a no-op.
        let walk = compute_skg_scores(&bm25, &graph, "mercedes", &params);
        let mut hits = vec![SearchHit { section_index: 0, score: 5.0 }];
        apply_skg_boost(&mut hits, &walk.scores, 0.0);
        assert_eq!(hits.len(), 1);
        assert!((hits[0].score - 5.0).abs() < 1e-9);
    }

    #[test]
    fn apply_boost_scales_hits_and_expands_strong_sections() {
        let mut hits = vec![SearchHit { section_index: 0, score: 2.0 }];
        let mut skg = std::collections::HashMap::new();
        skg.insert(0usize, 0.5); // existing hit gets boosted
        skg.insert(7usize, 0.9); // strong, not present → appended
        skg.insert(9usize, 0.2); // weak, below SKG_EXPAND_MIN → ignored
        apply_skg_boost(&mut hits, &skg, 0.4);
        // 2.0 * (1 + 0.4*0.5) = 2.4 for section 0
        let h0 = hits.iter().find(|h| h.section_index == 0).unwrap();
        assert!((h0.score - 2.4).abs() < 1e-9);
        assert!(hits.iter().any(|h| h.section_index == 7));
        assert!(!hits.iter().any(|h| h.section_index == 9));
    }
}
