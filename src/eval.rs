//! Retrieval evaluation harness.
//!
//! Turns a Q&A file (as produced by `lib/lume_extractor.py qna`) into ranked-
//! retrieval relevance judgments and the standard IR metrics — Hit@k, MRR,
//! nDCG@k. There are no human relevance labels, so relevance is judged by
//! **answer-token containment**: a retrieved section is relevant if it contains
//! enough of the answer's content tokens. This is the same proxy RAG pipelines
//! use to ask "did retrieval surface the passage that answers the question?",
//! and it needs no alignment between the extractor's chunking and the index's
//! sectioning.
//!
//! This module is pure (parsing + judging + metrics) so it is unit-testable;
//! the index loading and search execution live in `main.rs::handle_eval`.

use std::collections::HashSet;

use crate::bm25::filter_query_stopwords;
use crate::tokenize;

/// One question/answer pair. `chunk_index` from the source file is retained for
/// provenance but not used for judging (we judge by answer containment, not by
/// chunk-id alignment, which would assume identical chunking).
#[derive(Debug, Clone)]
pub struct QnaItem {
    pub question: String,
    pub answer: String,
}

/// Parses a Q&A JSON array of `{question, answer, ...}` objects. Input is read
/// as raw bytes and lossily decoded so cp1252/latin-1 files (smart quotes, etc.)
/// don't abort the run — matching Lume's UTF-8-tolerant ingest.
pub fn parse_qna(bytes: &[u8]) -> Result<Vec<QnaItem>, String> {
    let text = String::from_utf8_lossy(bytes);
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("Q&A file is not valid JSON: {}", e))?;
    let arr = value
        .as_array()
        .ok_or_else(|| "Q&A file must be a JSON array of {question, answer} objects".to_string())?;
    let mut items = Vec::with_capacity(arr.len());
    for entry in arr {
        let question = entry.get("question").and_then(|v| v.as_str()).unwrap_or("").trim();
        let answer = entry.get("answer").and_then(|v| v.as_str()).unwrap_or("").trim();
        if question.is_empty() || answer.is_empty() {
            continue;
        }
        items.push(QnaItem { question: question.to_string(), answer: answer.to_string() });
    }
    if items.is_empty() {
        return Err("Q&A file contained no usable {question, answer} pairs".to_string());
    }
    Ok(items)
}

/// The content tokens of `text`: folded by the index tokenizer, with query
/// stopwords removed and one-character tokens dropped (punctuation, stray
/// digits). These are the tokens that carry the answer's meaning.
fn content_tokens(text: &str) -> Vec<Vec<u8>> {
    filter_query_stopwords(tokenize(text))
        .into_iter()
        .map(|t| t.bytes)
        .filter(|b| b.len() > 1)
        .collect()
}

/// Fraction of the answer's content tokens that appear in `section_body`, in
/// `[0,1]`. Returns `None` when the answer has no content tokens to match on
/// (e.g. a one-word stopword answer), so the caller can exclude it rather than
/// score it as a trivial hit or miss.
pub fn answer_recall(answer: &str, section_body: &str) -> Option<f64> {
    let needles = content_tokens(answer);
    if needles.is_empty() {
        return None;
    }
    let haystack: HashSet<Vec<u8>> = content_tokens(section_body).into_iter().collect();
    let present = needles.iter().filter(|t| haystack.contains(*t)).count();
    Some(present as f64 / needles.len() as f64)
}

/// Whether a section counts as relevant: at least `threshold` of the answer's
/// content tokens are present. Returns `None` for unjudgeable answers.
pub fn is_relevant(answer: &str, section_body: &str, threshold: f64) -> Option<bool> {
    answer_recall(answer, section_body).map(|r| r >= threshold)
}

/// Discounted cumulative gain over a ranked list of binary relevances, top `k`.
/// Position `i` (0-based) contributes `1 / log2(i + 2)` when relevant.
pub fn dcg_at_k(rels: &[bool], k: usize) -> f64 {
    rels.iter()
        .take(k)
        .enumerate()
        .filter(|(_, &r)| r)
        .map(|(i, _)| 1.0 / ((i + 2) as f64).log2())
        .sum()
}

/// Normalized DCG@k: actual DCG over the ideal DCG (the same relevances sorted
/// best-first). `0` when nothing relevant was retrieved. Bounded `[0,1]`.
pub fn ndcg_at_k(rels: &[bool], k: usize) -> f64 {
    let dcg = dcg_at_k(rels, k);
    let mut ideal = rels.to_vec();
    ideal.sort_by(|a, b| b.cmp(a)); // true (relevant) first
    let idcg = dcg_at_k(&ideal, k);
    if idcg == 0.0 { 0.0 } else { dcg / idcg }
}

/// Reciprocal rank: `1 / (rank of first relevant)`, `0` if none in the list.
pub fn reciprocal_rank(rels: &[bool]) -> f64 {
    rels.iter()
        .position(|&r| r)
        .map(|p| 1.0 / (p + 1) as f64)
        .unwrap_or(0.0)
}

/// Whether any of the top `k` is relevant.
pub fn hit_at_k(rels: &[bool], k: usize) -> bool {
    rels.iter().take(k).any(|&r| r)
}

/// Running aggregate of per-query metrics.
#[derive(Debug, Default, Clone)]
pub struct EvalAggregate {
    pub judged: usize,
    pub skipped: usize,
    pub hits: usize,
    pub mrr_sum: f64,
    pub ndcg_sum: f64,
    pub k: usize,
}

impl EvalAggregate {
    pub fn new(k: usize) -> Self {
        Self { k, ..Default::default() }
    }

    /// Records one question. `rels` is the ranked relevance list for its top
    /// results. Pass `None` to count the question as skipped (unjudgeable).
    pub fn record(&mut self, rels: Option<&[bool]>) {
        match rels {
            None => self.skipped += 1,
            Some(rels) => {
                self.judged += 1;
                if hit_at_k(rels, self.k) {
                    self.hits += 1;
                }
                self.mrr_sum += reciprocal_rank(rels);
                self.ndcg_sum += ndcg_at_k(rels, self.k);
            }
        }
    }

    pub fn hit_rate(&self) -> f64 {
        if self.judged == 0 { 0.0 } else { self.hits as f64 / self.judged as f64 }
    }
    pub fn mrr(&self) -> f64 {
        if self.judged == 0 { 0.0 } else { self.mrr_sum / self.judged as f64 }
    }
    pub fn ndcg(&self) -> f64 {
        if self.judged == 0 { 0.0 } else { self.ndcg_sum / self.judged as f64 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_skips_blank_pairs() {
        let json = br#"[
            {"question": "Q1", "answer": "A1", "chunk_index": 0},
            {"question": "", "answer": "x"},
            {"question": "Q2", "answer": "A2"}
        ]"#;
        let items = parse_qna(json).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].answer, "A1");
    }

    #[test]
    fn answer_recall_counts_content_token_overlap() {
        // All three content tokens present → 1.0.
        let r = answer_recall("February 24 1815", "signalled on february 24 1815 sharp").unwrap();
        assert!((r - 1.0).abs() < 1e-9);
        // Half present.
        let r = answer_recall("brain fever agony death", "he died of brain fever").unwrap();
        assert!((r - 0.5).abs() < 1e-9, "got {}", r);
        // An answer with no content tokens (single short token, dropped by the
        // len>1 filter) is unjudgeable.
        assert!(answer_recall("a", "anything").is_none());
    }

    #[test]
    fn ndcg_is_one_when_relevant_first_and_decays_with_rank() {
        assert!((ndcg_at_k(&[true, false, false], 10) - 1.0).abs() < 1e-9);
        let mid = ndcg_at_k(&[false, false, true], 10);
        assert!(mid > 0.0 && mid < 1.0, "got {}", mid);
        assert_eq!(ndcg_at_k(&[false, false, false], 10), 0.0);
    }

    #[test]
    fn reciprocal_rank_and_hit() {
        assert!((reciprocal_rank(&[false, true, false]) - 0.5).abs() < 1e-9);
        assert_eq!(reciprocal_rank(&[false, false]), 0.0);
        assert!(hit_at_k(&[false, true], 5));
        assert!(!hit_at_k(&[false, true], 1));
    }

    #[test]
    fn aggregate_tracks_judged_and_skipped() {
        let mut agg = EvalAggregate::new(5);
        agg.record(Some(&[true, false]));
        agg.record(Some(&[false, false]));
        agg.record(None);
        assert_eq!(agg.judged, 2);
        assert_eq!(agg.skipped, 1);
        assert_eq!(agg.hits, 1);
        assert!((agg.hit_rate() - 0.5).abs() < 1e-9);
        assert!((agg.mrr() - 0.5).abs() < 1e-9);
    }
}
