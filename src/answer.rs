//! Agentic question-answering over the retrieved field (`lume answer`).
//!
//! Drives a plan → retrieve → evaluate → refine → answer loop with a local
//! Ollama model (default gpt-4o-mini). The orchestration + retrieval + the
//! relaxation animation live in `main.rs::handle_answer`; this module holds the
//! model I/O (plan / evaluate / synthesize) and the citation parsing, so they
//! stay testable and the loop reads cleanly.

use serde_json::{json, Value};

/// Non-streaming Ollama `/api/chat` call. Returns the assistant message content.
pub fn ollama_chat(url: &str, model: &str, system: &str, user: &str, temperature: f64) -> Result<String, String> {
    let endpoint = format!("{}/api/chat", url.trim_end_matches('/'));
    let payload = json!({
        "model": model,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user },
        ],
        "stream": false,
        "options": { "temperature": temperature, "num_ctx": 16384 },
    });
    let resp = ureq::post(&endpoint)
        .set("Content-Type", "application/json")
        .timeout(std::time::Duration::from_secs(300))
        .send_json(&payload)
        .map_err(|e| format!("Ollama request failed: {}", e))?;
    let v: Value = resp.into_json().map_err(|e| format!("Ollama response parse failed: {}", e))?;
    v.get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "Ollama response had no message.content".to_string())
}

/// Extracts the first balanced `[...]` or `{...}` span from model text (which may
/// wrap JSON in prose or ``` fences).
fn first_json_span(s: &str, open: char, close: char) -> Option<&str> {
    let start = s.find(open)?;
    let mut depth = 0i32;
    for (i, c) in s[start..].char_indices() {
        if c == open { depth += 1; }
        else if c == close { depth -= 1; if depth == 0 { return Some(&s[start..start + i + c.len_utf8()]); } }
    }
    None
}

/// Plans 1–3 search queries for the question.
pub fn plan_queries(url: &str, model: &str, question: &str) -> Result<Vec<String>, String> {
    let sys = "You are a search query planner for a full-text search engine over a long narrative \
document. Given a question, output a JSON array of 2 to 4 SHORT keyword queries (3-6 words each, \
NOT full sentences) that would retrieve the passage answering it. The text usually describes what \
happens WITHOUT repeating the question's proper nouns, so DIVERSIFY the queries: \
(a) one using the key names/entities from the question; \
(b) one or two using SYNONYMS and the EVENT or ACTION itself, phrased the way the narrative would \
(e.g. for a death: 'died of starvation grief', 'passed away in poverty'); \
(c) one naming a likely SECONDARY character, place, or concrete detail involved in the scene. \
Do NOT repeat the same proper noun in every query. Output ONLY the JSON array, nothing else.";
    let out = ollama_chat(url, model, sys, question, 0.1)?;
    let arr = first_json_span(&out, '[', ']').ok_or_else(|| format!("planner gave no JSON array: {}", out.trim()))?;
    let v: Value = serde_json::from_str(arr).map_err(|e| format!("planner JSON invalid: {}", e))?;
    let queries: Vec<String> = v.as_array().map(|a| {
        a.iter().filter_map(|x| x.as_str().map(|s| s.trim().to_string())).filter(|s| !s.is_empty()).collect()
    }).unwrap_or_default();
    if queries.is_empty() { Err("planner produced no queries".to_string()) } else { Ok(queries) }
}

/// The evaluator's verdict on whether the retrieved passages can answer the
/// question, plus any new queries to try if not.
pub struct Verdict {
    pub sufficient: bool,
    pub queries: Vec<String>,
    pub note: String,
}

/// Judges whether `passages` answer `question`; if not, proposes new queries.
pub fn evaluate(url: &str, model: &str, question: &str, passages: &str) -> Result<Verdict, String> {
    let sys = "You judge whether retrieved passages can answer a question. Respond with ONLY a \
JSON object: {\"sufficient\": true|false, \"queries\": [..], \"note\": \"..\"}. If the passages \
clearly contain the answer, set sufficient=true and queries=[]. If not, set sufficient=false and \
give 2-3 NEW short keyword queries that approach from a DIFFERENT angle than what evidently failed: \
use synonyms, the event/action as the narrative would phrase it, or a secondary character, place, \
or concrete detail — rather than rephrasing the question's proper nouns again.";
    let user = format!("QUESTION: {}\n\nPASSAGES:\n{}", question, passages);
    let out = ollama_chat(url, model, sys, &user, 0.1)?;
    let obj = first_json_span(&out, '{', '}').ok_or_else(|| format!("evaluator gave no JSON object: {}", out.trim()))?;
    let v: Value = serde_json::from_str(obj).map_err(|e| format!("evaluator JSON invalid: {}", e))?;
    let sufficient = v.get("sufficient").and_then(|x| x.as_bool()).unwrap_or(true);
    let queries = v.get("queries").and_then(|x| x.as_array()).map(|a| {
        a.iter().filter_map(|x| x.as_str().map(|s| s.trim().to_string())).filter(|s| !s.is_empty()).collect()
    }).unwrap_or_default();
    let note = v.get("note").and_then(|x| x.as_str()).unwrap_or("").to_string();
    Ok(Verdict { sufficient, queries, note })
}

/// Synthesizes a cited answer from numbered passages.
pub fn synthesize(url: &str, model: &str, question: &str, numbered_passages: &str) -> Result<String, String> {
    let sys = "Answer the question using ONLY the numbered passages provided. Cite every passage \
you draw on inline with its number in square brackets, e.g. [2]. Be concise and factual. If the \
passages do not contain the answer, say so plainly.";
    let user = format!("QUESTION: {}\n\nPASSAGES:\n{}\n\nAnswer with inline [n] citations:", question, numbered_passages);
    ollama_chat(url, model, sys, &user, 0.3)
}

/// Parses the `[n]` markers actually used in an answer, returning the distinct
/// 1-based passage numbers in first-appearance order.
pub fn parse_citations(answer: &str, max_n: usize) -> Vec<usize> {
    let mut out = Vec::new();
    let bytes = answer.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'[' {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_digit() { j += 1; }
            if j > i + 1 && j < bytes.len() && bytes[j] == b']' {
                if let Ok(n) = answer[i + 1..j].parse::<usize>() {
                    if n >= 1 && n <= max_n && !out.contains(&n) { out.push(n); }
                }
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_json_spans_from_prose() {
        assert_eq!(first_json_span("sure: [\"a\", \"b\"] ok", '[', ']'), Some("[\"a\", \"b\"]"));
        assert_eq!(first_json_span("```json\n{\"x\":1}\n```", '{', '}'), Some("{\"x\":1}"));
        assert_eq!(first_json_span("none here", '[', ']'), None);
    }

    #[test]
    fn parses_distinct_in_range_citations() {
        assert_eq!(parse_citations("From [2] and [5], also [2] again.", 6), vec![2, 5]);
        assert_eq!(parse_citations("[9] is out of range", 6), Vec::<usize>::new());
        assert_eq!(parse_citations("no cites", 6), Vec::<usize>::new());
    }
}
