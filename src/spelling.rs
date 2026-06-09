use std::collections::HashMap;
use crate::fast_retrieval::MiniRoaring;
use serde::{Serialize, Deserialize};

/// Generates character trigrams for a given word.
/// Pads the word with boundary characters `_` to emphasize prefix and suffix matches
/// (e.g., "this" -> "_th", "thi", "his", "is_").
pub fn trigrams(word: &str) -> Vec<String> {
    let mut padded = String::with_capacity(word.len() + 2);
    padded.push('_');
    padded.push_str(word);
    padded.push('_');
    
    let chars: Vec<char> = padded.chars().collect();
    if chars.len() < 3 {
        return vec![word.to_string()];
    }
    
    let mut tg = Vec::with_capacity(chars.len() - 2);
    for i in 0..chars.len() - 2 {
        let mut s = String::with_capacity(3);
        s.push(chars[i]);
        s.push(chars[i + 1]);
        s.push(chars[i + 2]);
        tg.push(s);
    }
    tg
}

pub fn levenshtein_distance(a: &str, b: &str) -> usize {
    let len_a = a.chars().count();
    let len_b = b.chars().count();
    if len_a == 0 { return len_b; }
    if len_b == 0 { return len_a; }
    
    let mut dp = vec![0; len_b + 1];
    for j in 0..=len_b {
        dp[j] = j;
    }
    
    for (i, ca) in a.chars().enumerate() {
        let mut prev = dp[0];
        dp[0] = i + 1;
        for (j, cb) in b.chars().enumerate() {
            let temp = dp[j + 1];
            if ca == cb {
                dp[j + 1] = prev;
            } else {
                dp[j + 1] = std::cmp::min(
                    prev + 1, // replacement
                    std::cmp::min(
                        dp[j + 1] + 1, // deletion
                        dp[j] + 1      // insertion
                    )
                );
            }
            prev = temp;
        }
    }
    dp[len_b]
}

/// A dedicated spelling corrector backed by an inverted trigram index using roaring bitmaps
/// and field-independent BM25 scoring.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpellIndex {
    pub unique_words: Vec<String>,
    pub vocab_set: std::collections::HashSet<String>,
    pub trigram_postings: HashMap<String, MiniRoaring>,
    pub word_lens: Vec<usize>,
    pub trigram_dfs: HashMap<String, usize>,
    pub avg_word_len: f64,
    pub num_words: usize,
}

impl SpellIndex {
    /// Builds a spelling index from the static tagger phrases and BM25 index corpus terms.
    pub fn build(phrases: &[String], corpus_terms: &[Vec<u8>]) -> Self {
        let mut word_set = std::collections::HashSet::new();
        
        // 1. Process tagger phrases: tokenize into simple alphabetic/alphanumeric words
        for p in phrases {
            let tokens = crate::tokenize(p);
            for tok in tokens {
                if let Ok(w) = String::from_utf8(tok.bytes) {
                    let w_trimmed = w.trim().to_lowercase();
                    if !w_trimmed.is_empty() && w_trimmed.chars().any(|c| c.is_alphabetic()) {
                        word_set.insert(w_trimmed);
                    }
                }
            }
        }
        
        // 2. Process BM25 corpus terms: already tokenized bytes
        for term_bytes in corpus_terms {
            if let Ok(w) = String::from_utf8(term_bytes.clone()) {
                let w_trimmed = w.trim().to_lowercase();
                if !w_trimmed.is_empty() && w_trimmed.chars().any(|c| c.is_alphabetic()) {
                    word_set.insert(w_trimmed);
                }
            }
        }
        
        let unique_words: Vec<String> = word_set.into_iter().collect();
        let num_words = unique_words.len();
        let vocab_set: std::collections::HashSet<String> = unique_words.iter().cloned().collect();
        
        let mut trigram_postings: HashMap<String, MiniRoaring> = HashMap::new();
        let mut word_lens = Vec::with_capacity(num_words);
        let mut trigram_dfs = HashMap::new();
        let mut total_trigram_count = 0;
        
        for (idx, word) in unique_words.iter().enumerate() {
            let trigs = trigrams(word);
            word_lens.push(trigs.len());
            total_trigram_count += trigs.len();
            
            // Deduplicate trigrams in a single word for proper DF calculation
            let mut unique_trigs = trigs.clone();
            unique_trigs.sort();
            unique_trigs.dedup();
            
            for trig in unique_trigs {
                *trigram_dfs.entry(trig.clone()).or_insert(0) += 1;
            }
            
            for trig in trigs {
                trigram_postings
                    .entry(trig)
                    .or_default()
                    .insert(idx as u32);
            }
        }
        
        let avg_word_len = if num_words > 0 {
            total_trigram_count as f64 / num_words as f64
        } else {
            0.0
        };
        
        Self {
            unique_words,
            vocab_set,
            trigram_postings,
            word_lens,
            trigram_dfs,
            avg_word_len,
            num_words,
        }
    }

    /// Evaluates a query term and returns up to `max_suggestions` ordered candidates
    /// with their final alignment scores.
    pub fn correct_word(&self, word: &str, max_suggestions: usize) -> Vec<(String, f64)> {
        let query_word = word.trim().to_lowercase();
        if query_word.is_empty() || self.num_words == 0 {
            return Vec::new();
        }
        
        let q_trigs = trigrams(&query_word);
        if q_trigs.is_empty() {
            return Vec::new();
        }
        
        // 1. Gather all candidates using union of roaring posting lists
        let mut candidate_set = MiniRoaring::new();
        let mut first = true;
        for trig in &q_trigs {
            if let Some(list) = self.trigram_postings.get(trig) {
                if first {
                    candidate_set = list.clone();
                    first = false;
                } else {
                    candidate_set = candidate_set.union(list);
                }
            }
        }
        
        let candidate_ids = candidate_set.iter();
        if candidate_ids.is_empty() {
            return Vec::new();
        }
        
        // 2. Score candidates using BM25 scoring over sub-word trigram tokens
        let k1 = 1.2;
        let b = 0.75;
        let mut scored_candidates = Vec::new();
        
        for doc_id in candidate_ids {
            let doc_idx = doc_id as usize;
            let candidate_word = &self.unique_words[doc_idx];
            let cand_trigs = trigrams(candidate_word);
            
            let mut tf_map = HashMap::new();
            for trig in &cand_trigs {
                *tf_map.entry(trig.clone()).or_insert(0) += 1;
            }
            
            let mut score = 0.0;
            let doc_len = self.word_lens[doc_idx] as f64;
            let len_norm = 1.0 - b + b * (doc_len / self.avg_word_len);
            
            for trig in &q_trigs {
                if let Some(&tf) = tf_map.get(trig) {
                    let tf = tf as f64;
                    let df = self.trigram_dfs.get(trig).copied().unwrap_or(0);
                    
                    let idf = ((self.num_words as f64 - df as f64 + 0.5) / (df as f64 + 0.5) + 1.0).ln();
                    let idf = idf.max(0.0);
                    
                    score += idf * (tf * (k1 + 1.0)) / (tf + k1 * len_norm);
                }
            }
            
            if score > 0.0 {
                scored_candidates.push((candidate_word.clone(), score));
            }
        }
        
        // Sort BM25 score descending
        scored_candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        
        // 3. Keep the top 20 candidate matches and apply DP Levenshtein distance check <= 2
        let top_n = scored_candidates.into_iter().take(20);
        let mut results = Vec::new();
        
        for (cand, bm25_score) in top_n {
            let dist = levenshtein_distance(&query_word, &cand);
            if dist <= 2 {
                // Apply a score penalty based on the edit distance
                let dist_multiplier = match dist {
                    0 => 1.0,
                    1 => 0.8,
                    2 => 0.5,
                    _ => 0.0,
                };
                let final_score = bm25_score * dist_multiplier;
                results.push((cand, final_score));
            }
        }
        
        // Re-sort results by final score descending
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(max_suggestions);
        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trigram_slices() {
        let trigs = trigrams("cat");
        // "_cat_" -> "_ca", "cat", "at_"
        assert_eq!(trigs, vec!["_ca", "cat", "at_"]);

        let short = trigrams("a");
        assert_eq!(short, vec!["_a_"]);
    }

    #[test]
    fn test_levenshtein_distance() {
        assert_eq!(levenshtein_distance("htis", "this"), 2); // Transposition = 2 edits (substitution of h and t)
        assert_eq!(levenshtein_distance("lucene", "lucne"), 1); // Deletion of o
        assert_eq!(levenshtein_distance("solr", "solar"), 1); // Insertion of a
        assert_eq!(levenshtein_distance("hello", "hello"), 0); // Exact match
    }

    #[test]
    fn test_spelling_corrector() {
        let phrases = vec![
            "Monte Cristo".to_string(),
            "Apache Lucene".to_string(),
            "Elasticsearch".to_string(),
            "Antigravity".to_string(),
        ];
        let corpus_terms = vec![
            b"this".to_vec(),
            b"that".to_vec(),
            b"apple".to_vec(),
        ];
        
        let index = SpellIndex::build(&phrases, &corpus_terms);
        assert!(index.num_words > 0);
        
        // Query misspelled terms
        let corrections = index.correct_word("htis", 1);
        assert_eq!(corrections.len(), 1);
        assert_eq!(corrections[0].0, "this");
        
        let corrections = index.correct_word("lucne", 1);
        assert_eq!(corrections.len(), 1);
        assert_eq!(corrections[0].0, "lucene");

        let corrections = index.correct_word("aplle", 1);
        assert_eq!(corrections.len(), 1);
        assert_eq!(corrections[0].0, "apple");
    }
}
