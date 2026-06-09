use std::collections::HashMap;
use std::time::SystemTime;
use crate::fast_retrieval::MiniRoaring;

// ─── SimpleRng (Zero-Dependency Xorshift RNG) ──────────────────────────

#[derive(Debug, Clone)]
pub struct SimpleRng {
    state: u64,
}

impl Default for SimpleRng {
    fn default() -> Self {
        Self::new()
    }
}

impl SimpleRng {
    pub fn new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let mut state = (nanos & 0xFFFFFFFFFFFFFFFF) as u64;
        if state == 0 {
            state = 0xACE1; // Fallback seed
        }
        Self { state }
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    pub fn next_range(&mut self, min: usize, max: usize) -> usize {
        if min >= max {
            return min;
        }
        let range = (max - min) as u64;
        let val = self.next_u64() % range;
        min + val as usize
    }
}

// ─── Word & Punctuation Tokenizer ────────────────────────────────────────

/// Tokenizes raw text, separating alphanumeric words (including internal 
/// apostrophes and hyphens like "d'if" or "twenty-four") from punctuation.
pub fn parse_words_and_punctuation(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        
        if c.is_alphanumeric() {
            let mut word = String::new();
            while i < chars.len() {
                let next_c = chars[i];
                // Treat apostrophes/hyphens inside alphanumeric bounds as part of the word
                if next_c.is_alphanumeric() || next_c == '\'' || next_c == '’' || next_c == '-' {
                    word.push(next_c);
                    i += 1;
                } else {
                    break;
                }
            }
            if !word.is_empty() {
                tokens.push(word);
            }
        } else {
            // Treat punctuation and special characters as single-character tokens
            tokens.push(c.to_string());
            i += 1;
        }
    }
    tokens
}

// ─── Whitespace & Punctuation Spacing Reconstruction ────────────────────

/// Reconstructs spacing between tokens to form natural, human-readable paragraphs.
pub fn reconstruct_spaces(tokens: &[String]) -> String {
    let mut result = String::new();
    let mut skip_next_space = false;

    for (idx, token) in tokens.iter().enumerate() {
        if idx == 0 {
            result.push_str(token);
        } else {
            let first_char = token.chars().next().unwrap_or(' ');
            
            // Check if this token is a punctuation that shouldn't have a space before it
            let is_close_punc = first_char == '.' || first_char == ',' || first_char == '!' || 
                                first_char == '?' || first_char == ';' || first_char == ':' || 
                                first_char == ')' || first_char == ']' || first_char == '}' || 
                                first_char == '’' || token == "”" || token == "—" || token == "-";
            
            if is_close_punc || skip_next_space {
                result.push_str(token);
                skip_next_space = false;
            } else {
                result.push(' ');
                result.push_str(token);
            }
        }
        
        // If this token is an opening punctuation, skip the space for the next token
        if token == "“" || token == "(" || token == "[" {
            skip_next_space = true;
        }
    }
    result
}

// ─── Generative Markov Style Model (Option C) ───────────────────────────

#[derive(Debug, Clone)]
pub struct MarkovChain {
    pub transitions: HashMap<(String, String), Vec<String>>,
    pub start_words: Vec<(String, String)>,
}

impl MarkovChain {
    /// Builds a trigram Markov Chain model over text sections.
    pub fn build<S: AsRef<str>>(bodies: &[S]) -> Self {
        let mut transitions = HashMap::new();
        let mut start_words = Vec::new();

        for body_ref in bodies {
            let tokens = parse_words_and_punctuation(body_ref.as_ref());
            if tokens.len() < 3 {
                continue;
            }

            // Check if the very start is a good sentence starter
            if is_good_sentence_start(&tokens[0]) {
                start_words.push((tokens[0].clone(), tokens[1].clone()));
            }

            for i in 0..(tokens.len() - 2) {
                let key = (tokens[i].clone(), tokens[i + 1].clone());
                let next_word = tokens[i + 2].clone();
                transitions.entry(key).or_insert_with(Vec::new).push(next_word);

                // Collect sentence starters after punctuation
                if (tokens[i] == "." || tokens[i] == "!" || tokens[i] == "?") && is_good_sentence_start(&tokens[i + 1]) {
                    start_words.push((tokens[i + 1].clone(), tokens[i + 2].clone()));
                }
            }
        }

        Self { transitions, start_words }
    }

    /// Generates styled text starting with an optional seed word.
    pub fn generate(&self, seed_word: Option<&str>, max_tokens: usize) -> String {
        let mut rng = SimpleRng::new();
        let mut tokens = Vec::new();
        let mut visited_trigrams = std::collections::HashSet::new();

        // 1. Choose starting pair
        let mut current_pair = None;
        if let Some(seed) = seed_word {
            let mut candidates = Vec::new();
            let seed_lower = seed.to_lowercase();
            for key in self.transitions.keys() {
                if key.0.to_lowercase() == seed_lower {
                    candidates.push(key.clone());
                }
            }
            if !candidates.is_empty() {
                let idx = rng.next_range(0, candidates.len());
                current_pair = Some(candidates[idx].clone());
            }
        }

        // Fallback to random sentence starters
        if current_pair.is_none() && !self.start_words.is_empty() {
            let idx = rng.next_range(0, self.start_words.len());
            current_pair = Some(self.start_words[idx].clone());
        }

        let (mut w1, mut w2) = match current_pair {
            Some(pair) => pair,
            None => return String::from("No styled text generated (empty index)."),
        };

        tokens.push(w1.clone());
        tokens.push(w2.clone());

        let mut token_count = 2;
        while token_count < max_tokens {
            let key = (w1.clone(), w2.clone());
            if let Some(next_words) = self.transitions.get(&key) {
                if next_words.is_empty() {
                    break;
                }

                // Deduplicate transitions to avoid infinite loops and repeating clauses
                let mut candidates_w3 = Vec::new();
                for w in next_words {
                    let trigram = (w1.clone(), w2.clone(), w.clone());
                    if !visited_trigrams.contains(&trigram) {
                        candidates_w3.push(w.clone());
                    }
                }

                let w3 = if !candidates_w3.is_empty() {
                    let next_idx = rng.next_range(0, candidates_w3.len());
                    candidates_w3[next_idx].clone()
                } else {
                    // If stuck in a loop where all choices would repeat a trigram,
                    // jump to a new start pair to maintain natural diversity.
                    if !self.start_words.is_empty() {
                        let idx = rng.next_range(0, self.start_words.len());
                        let (start_w1, start_w2) = self.start_words[idx].clone();
                        w1 = start_w1;
                        w2 = start_w2;
                        tokens.push(w1.clone());
                        tokens.push(w2.clone());
                        token_count += 2;
                        continue;
                    } else {
                        break;
                    }
                };

                visited_trigrams.insert((w1.clone(), w2.clone(), w3.clone()));
                tokens.push(w3.clone());

                w1 = w2;
                w2 = w3.clone();
                token_count += 1;

                // End at natural sentence boundary if we have enough text
                if token_count > 80 && (w3 == "." || w3 == "!" || w3 == "?") {
                    break;
                }
            } else {
                // Handle transition dead ends by jumping to another suffix if possible
                let mut candidates = Vec::new();
                for k in self.transitions.keys() {
                    if k.0 == w2 {
                        candidates.push(k.clone());
                    }
                }
                if !candidates.is_empty() {
                    let idx = rng.next_range(0, candidates.len());
                    w1 = candidates[idx].0.clone();
                    w2 = candidates[idx].1.clone();
                    // Push the new word since we are jumping and it hasn't been pushed!
                    tokens.push(w2.clone());
                    token_count += 1;
                } else {
                    break;
                }
            }
        }

        reconstruct_spaces(&tokens)
    }

    /// Generates steered text utilizing an FST tagger and co-occurrence posting lists for local attention feedback.
    #[allow(clippy::type_complexity)]
    pub fn generate_steered(
        &self,
        seed_word: Option<&str>,
        max_tokens: usize,
        tagger: Option<&crate::Tagger>,
        entity_posting_lists: &HashMap<String, MiniRoaring>,
        token_posting_lists: &HashMap<Vec<u8>, MiniRoaring>,
        injected_tags: &[String],
    ) -> (String, Vec<(usize, HashMap<String, f64>)>) {
        let mut rng = SimpleRng::new();
        let mut tokens = Vec::new();
        let mut visited_trigrams = std::collections::HashSet::new();
        let mut attention_register: HashMap<String, f64> = HashMap::new();
        let mut attention_history = Vec::new();

        // 0. Pre-inject requested tags into attention register
        for tag in injected_tags {
            attention_register.insert(tag.to_uppercase(), 1.0);
        }

        // 1. Choose starting pair
        let mut current_pair = None;
        if let Some(seed) = seed_word {
            let mut candidates = Vec::new();
            let seed_lower = seed.to_lowercase();
            for key in self.transitions.keys() {
                if key.0.to_lowercase() == seed_lower {
                    candidates.push(key.clone());
                }
            }
            if !candidates.is_empty() {
                let idx = rng.next_range(0, candidates.len());
                current_pair = Some(candidates[idx].clone());
            }
        }

        // Fallback to random sentence starters
        if current_pair.is_none() && !self.start_words.is_empty() {
            let idx = rng.next_range(0, self.start_words.len());
            current_pair = Some(self.start_words[idx].clone());
        }

        let (mut w1, mut w2) = match current_pair {
            Some(pair) => pair,
            None => return (String::from("No styled text generated (empty index)."), Vec::new()),
        };

        tokens.push(w1.clone());
        tokens.push(w2.clone());

        // Tag initial words
        if let Some(t) = tagger {
            let initial_text = reconstruct_spaces(&tokens);
            let matched = t.tag(&initial_text);
            for tag in matched {
                attention_register.insert(tag.output.clone(), 1.0);
            }
        }

        let mut token_count = 2;
        while token_count < max_tokens {
            let key = (w1.clone(), w2.clone());
            if let Some(next_words) = self.transitions.get(&key) {
                if next_words.is_empty() {
                    break;
                }

                // Collect transitions with their frequencies to preserve original probability distribution
                let mut candidate_freqs = HashMap::new();
                for w in next_words {
                    let trigram = (w1.clone(), w2.clone(), w.clone());
                    if !visited_trigrams.contains(&trigram) {
                        *candidate_freqs.entry(w.clone()).or_insert(0) += 1;
                    }
                }

                let w3 = if !candidate_freqs.is_empty() {
                    let candidates_w3: Vec<String> = candidate_freqs.keys().cloned().collect();
                    // Compute dynamic attention weights for candidates
                    let mut candidate_weights = Vec::with_capacity(candidates_w3.len());
                    for c in &candidates_w3 {
                        let freq = candidate_freqs.get(c).copied().unwrap_or(1);
                        let mut weight = freq as f64; // Start with the natural frequency as base weight
                        
                        let c_upper = c.to_uppercase();

                        // Tokenize candidate to get its folded bytes key in the index
                        let c_toks = crate::tokenize(c);
                        let c_bytes = c_toks.first().map(|t| t.bytes.clone());
                        
                        for (active_tag, active_weight) in &attention_register {
                            let tag_upper = active_tag.to_uppercase();
                            
                            // Try to resolve the active tag posting list from FST entities, fallback to general tokens
                            let list_t = entity_posting_lists.get(&tag_upper)
                                .or_else(|| {
                                    let tag_toks = crate::tokenize(active_tag);
                                    tag_toks.first().and_then(|t| token_posting_lists.get(&t.bytes))
                                });
                            
                            if let Some(lt) = list_t {
                                // 1. Co-occurrence graph Jaccard boost using actual text token posting lists!
                                if let Some(ref cb) = c_bytes {
                                    if let Some(list_c) = token_posting_lists.get(cb) {
                                        let jaccard = list_c.jaccard_similarity(lt);
                                        if jaccard > 0.0 {
                                            weight += 80.0 * jaccard * active_weight;
                                        }
                                    }
                                }
                                
                                // Also check entity-to-entity posting lists for co-occurrence if candidate is an FST entity!
                                if let Some(list_c) = entity_posting_lists.get(&c_upper) {
                                    let jaccard = list_c.jaccard_similarity(lt);
                                    if jaccard > 0.0 {
                                        weight += 80.0 * jaccard * active_weight;
                                    }
                                }
                            }
                            
                            // 2. Direct name/substring matching boost
                            if tag_upper.contains(&c_upper) || c_upper.contains(&tag_upper) {
                                weight += 30.0 * active_weight;
                            }
                        }
                        candidate_weights.push(weight);
                    }

                    // Weighted random choice
                    let total_weight: f64 = candidate_weights.iter().sum();
                    if total_weight > 0.0 {
                        let mut roll = (rng.next_range(0, 100000) as f64 / 100000.0) * total_weight;
                        let mut chosen_idx = 0;
                        for (idx, _) in candidate_weights.iter().enumerate() {
                            roll -= candidate_weights[idx];
                            if roll <= 0.0 {
                                chosen_idx = idx;
                                break;
                            }
                        }
                        candidates_w3[chosen_idx].clone()
                    } else {
                        let next_idx = rng.next_range(0, candidates_w3.len());
                        candidates_w3[next_idx].clone()
                    }
                } else {
                    // No unvisited transitions remaining from this state. Terminate generation cleanly.
                    break;
                };

                visited_trigrams.insert((w1.clone(), w2.clone(), w3.clone()));
                tokens.push(w3.clone());

                // Feed newly generated token back to FST Tagger to detect active entities
                if let Some(t) = tagger {
                    let recent_len = tokens.len().min(5);
                    let recent_tokens = &tokens[tokens.len() - recent_len..];
                    let recent_text = reconstruct_spaces(recent_tokens);
                    let matched_tags = t.tag(&recent_text);
                    for tag in matched_tags {
                        attention_register.insert(tag.output.clone(), 1.0);
                    }
                }

                // Decay attention weights
                for (tag, weight) in attention_register.iter_mut() {
                    let is_injected = injected_tags.iter().any(|it| it.to_uppercase() == tag.to_uppercase());
                    *weight *= 0.85;
                    // If it is an injected tag, keep it at at least 0.3 to sustain thematic focus!
                    if is_injected && *weight < 0.3 {
                        *weight = 0.3;
                    }
                }
                attention_register.retain(|_, &mut w| w > 0.05);

                // Record attention trace history for visualization
                if !attention_register.is_empty() {
                    attention_history.push((tokens.len(), attention_register.clone()));
                }

                w1 = w2;
                w2 = w3.clone();
                token_count += 1;

                // End at natural sentence boundary if we have enough text
                if token_count > 80 && (w3 == "." || w3 == "!" || w3 == "?") {
                    break;
                }
            } else {
                // Handle transition dead ends by jumping to another suffix if possible
                let mut candidates = Vec::new();
                for k in self.transitions.keys() {
                    if k.0 == w2 {
                        candidates.push(k.clone());
                    }
                }
                if !candidates.is_empty() {
                    let idx = rng.next_range(0, candidates.len());
                    w1 = candidates[idx].0.clone();
                    w2 = candidates[idx].1.clone();
                    tokens.push(w2.clone());
                    token_count += 1;
                } else {
                    break;
                }
            }
        }

        (reconstruct_spaces(&tokens), attention_history)
    }
}

fn is_good_sentence_start(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let first_char = s.chars().next().unwrap();
    first_char.is_uppercase() || first_char == '“' || first_char == '"'
}

// ─── Semantic Entity Graph Model (Option A) ─────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EntityNode {
    pub id: String,
    pub label: String,
    pub kind: String,
    pub frequency: usize,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EntityEdge {
    pub source: String,
    pub target: String,
    pub similarity: f64,
    pub intersection: usize,
    pub union_size: usize,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EntityGraph {
    pub nodes: Vec<EntityNode>,
    pub edges: Vec<EntityEdge>,
}

impl EntityGraph {
    /// Computes co-occurrence graph from BM25Index matched entity posting lists
    pub fn build(
        posting_lists: &HashMap<String, MiniRoaring>,
        kinds: &HashMap<String, String>,
        labels: &HashMap<String, String>,
        min_similarity: f64,
    ) -> Self {
        let mut nodes = Vec::new();
        let mut edges = Vec::new();

        // 1. Gather all nodes
        let mut entity_keys: Vec<&String> = posting_lists.keys().collect();
        // Sort keys for deterministic output ordering
        entity_keys.sort();

        for &key in &entity_keys {
            let frequency = posting_lists[key].len();
            if frequency > 0 {
                nodes.push(EntityNode {
                    id: key.clone(),
                    label: labels.get(key).cloned().unwrap_or_else(|| key.clone()),
                    kind: kinds.get(key).cloned().unwrap_or_else(|| "unknown".to_string()),
                    frequency,
                });
            }
        }

        // 2. Compute pairwise Jaccard similarities
        for i in 0..entity_keys.len() {
            for j in (i + 1)..entity_keys.len() {
                let key_a = entity_keys[i];
                let key_b = entity_keys[j];
                
                let list_a = &posting_lists[key_a];
                let list_b = &posting_lists[key_b];

                let similarity = list_a.jaccard_similarity(list_b);
                if similarity >= min_similarity && similarity > 0.0 {
                    let intersection = list_a.intersect(list_b).len();
                    let union_size = list_a.union(list_b).len();

                    edges.push(EntityEdge {
                        source: key_a.clone(),
                        target: key_b.clone(),
                        similarity,
                        intersection,
                        union_size,
                    });
                }
            }
        }

        // Sort edges by similarity descending
        edges.sort_by(|a, b| b.similarity.partial_cmp(&a.similarity).unwrap_or(std::cmp::Ordering::Equal));

        Self { nodes, edges }
    }

    /// Serializes the graph manually to a clean JSON string without serde
    pub fn to_json(&self) -> String {
        let mut json = String::new();
        json.push_str("{\n  \"entities\": [\n");
        for (i, node) in self.nodes.iter().enumerate() {
            json.push_str("    {\n");
            json.push_str(&format!("      \"id\": \"{}\",\n", escape_json(&node.id)));
            json.push_str(&format!("      \"label\": \"{}\",\n", escape_json(&node.label)));
            json.push_str(&format!("      \"kind\": \"{}\",\n", escape_json(&node.kind)));
            json.push_str(&format!("      \"frequency\": {}\n", node.frequency));
            json.push_str(if i == self.nodes.len() - 1 { "    }\n" } else { "    },\n" });
        }
        json.push_str("  ],\n  \"links\": [\n");
        for (i, edge) in self.edges.iter().enumerate() {
            json.push_str("    {\n");
            json.push_str(&format!("      \"source\": \"{}\",\n", escape_json(&edge.source)));
            json.push_str(&format!("      \"target\": \"{}\",\n", escape_json(&edge.target)));
            json.push_str(&format!("      \"similarity\": {:.4},\n", edge.similarity));
            json.push_str(&format!("      \"intersection\": {},\n", edge.intersection));
            json.push_str(&format!("      \"union\": {}\n", edge.union_size));
            json.push_str(if i == self.edges.len() - 1 { "    }\n" } else { "    },\n" });
        }
        json.push_str("  ]\n}");
        json
    }

    /// Prints a beautiful ASCII-art relationship table to the terminal
    pub fn print_ascii_table(&self) {
        println!("\x1B[1;36m");
        println!("  ┌──────────────────────────────────────────────────────────────────────────────┐");
        println!("  │                    SEMANTIC ENTITY CO-OCCURRENCE MESH                       │");
        println!("  ├──────────────────────────────┬──────────────────────────────┬────────┬───────┤");
        println!("  │ Entity A                     │ Entity B                     │Jaccard │Shared │");
        println!("  ├──────────────────────────────┼──────────────────────────────┼────────┼───────┤");
        
        if self.edges.is_empty() {
            println!("  │ (No co-occurrence relationships found above threshold)                       │");
        } else {
            // Show top 25 strongest relationships to keep terminal clean and beautiful
            for edge in self.edges.iter().take(25) {
                let name_a = truncate_or_pad(&edge.source, 28);
                let name_b = truncate_or_pad(&edge.target, 28);
                println!(
                    "  │ \x1B[1;33m{}\x1B[0;36m │ \x1B[1;33m{}\x1B[0;36m │ \x1B[1;32m{:.4}\x1B[0;36m │ \x1B[35m{:3}/{:<3}\x1B[0;36m │",
                    name_a, name_b, edge.similarity, edge.intersection, edge.union_size
                );
            }
        }
        
        println!("  └──────────────────────────────┴──────────────────────────────┴────────┴───────┘");
        println!("\x1B[0m");
    }
}

fn escape_json(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn truncate_or_pad(s: &str, width: usize) -> String {
    if s.len() > width {
        format!("{}...", &s[..width - 3])
    } else {
        format!("{:<width$}", s, width = width)
    }
}
