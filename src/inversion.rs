use serde::{Deserialize, Serialize};
use crate::bm25::Bm25Index;
use crate::Tagger;
use crate::semantic_mesh::MarkovChain;

#[derive(Serialize, Debug)]
pub struct InvertRequest {
    pub embedding: Vec<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_length: Option<usize>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct InvertResponse {
    pub text: String,
    pub similarity: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InversionResult {
    pub reconstructed_text: String,
    pub similarity: f64,
    pub steered_text: Option<String>,
    pub extracted_tags: Vec<String>,
    pub attention_history: Option<Vec<(usize, std::collections::HashMap<String, f64>)>>,
}

/// A simple, reusable primitive to invert a 768-dimensional GTR-T5 vector
/// back to its original text representation via the remote shivvr.nuts.services API.
pub fn invert_vector(
    embedding: &[f64],
    max_length: Option<usize>,
    token: &str,
) -> Result<InvertResponse, String> {
    if embedding.len() != 768 {
        return Err(format!(
            "Invalid embedding dimension. Expected 768, got {}",
            embedding.len()
        ));
    }

    if token.trim().is_empty() {
        return Err("NUTS_SERVICES_TOKEN is empty or not set.".to_string());
    }

    let url = format!("{}/invert", crate::hybrid::get_shivvr_base_url());
    let auth_header = format!("Bearer {}", token);

    let payload = InvertRequest {
        embedding: embedding.to_vec(),
        max_length,
    };

    match ureq::post(&url)
        .set("Authorization", &auth_header)
        .set("Content-Type", "application/json")
        .send_json(&payload)
    {
        Ok(res) => {
            match res.into_json::<InvertResponse>() {
                Ok(resp) => Ok(resp),
                Err(e) => Err(format!("Failed to parse inversion response JSON: {}", e)),
            }
        }
        Err(ureq::Error::Status(code, res)) => {
            let body = res.into_string().unwrap_or_else(|_| "Unknown error body".to_string());
            Err(format!("Server returned error status {}: {}", code, body))
        }
        Err(e) => Err(crate::hybrid::format_shivvr_error(&url, format!("Network request failed: {}", e))),
    }
}

/// The core pipeline function that performs vector inversion and, if corpus details are provided,
/// steering FST theme extraction and Markov chain synthesis.
pub fn execute_steered_inversion(
    embedding: &[f64],
    max_length: Option<usize>,
    token: &str,
    index: Option<&Bm25Index>,
    tagger: Option<&Tagger>,
    override_steering_tags: Option<Vec<String>>,
) -> Result<InversionResult, String> {
    // 1. Invert vector to raw text
    let resp = invert_vector(embedding, max_length, token)?;

    let mut steered_text = None;
    let mut extracted_tags = Vec::new();
    let mut attention_history = None;

    // 2. Perform steered synthesis if both index and FST tagger are active
    if let Some(index) = index {
        if let Some(tagger) = tagger {
            // Extract unique active FST themes from reconstructed text or use override
            if let Some(tags) = override_steering_tags {
                extracted_tags = tags;
            } else {
                let tags = tagger.tag(&resp.text);
                for t in &tags {
                    extracted_tags.push(t.output.clone());
                }
            }
            extracted_tags.sort();
            extracted_tags.dedup();

            // Train Markov Chain on indexed document corpus sections
            let bodies: Vec<&str> = index.sections.iter().map(|s| s.body.as_str()).collect();
            if !bodies.is_empty() {
                let chain = MarkovChain::build(&bodies);
                let first_word = resp.text
                    .split_whitespace()
                    .next()
                    .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()));

                let (simulated, history) = chain.generate_steered(
                    first_word,
                    120,
                    Some(tagger),
                    &index.entity_posting_lists,
                    &index.posting_lists,
                    &extracted_tags,
                );
                steered_text = Some(simulated);
                if !history.is_empty() {
                    attention_history = Some(history);
                }
            }
        }
    }

    Ok(InversionResult {
        reconstructed_text: resp.text,
        similarity: resp.similarity,
        steered_text,
        extracted_tags,
        attention_history,
    })
}

impl InversionResult {
    /// Formats the result as a beautiful, premium Markdown document for MCP consumers.
    pub fn to_markdown(&self, steer_target_path: Option<&str>) -> String {
        let mut markdown = String::new();
        markdown.push_str("# 🔄 Shivvr Neural Vector Inversion Results\n\n");
        markdown.push_str("Successfully reconstructed high-dimensional GTR-T5 vector back to text.\n\n");
        
        markdown.push_str("## 📄 Reconstructed Text\n");
        markdown.push_str(&format!("> \"{}\"\n\n", self.reconstructed_text));

        let similarity = self.similarity;
        let (conf_color, conf_label) = if similarity >= 0.95 {
            ("🟢 **Green**", "Highly Faithful (Lossless)")
        } else if similarity >= 0.75 {
            ("🔵 **Blue**", "Faithful (Slightly Lossy/Paraphrased)")
        } else if similarity >= 0.50 {
            ("🟡 **Yellow**", "Lossy / Concept-Related")
        } else {
            ("🔴 **Red**", "Low Confidence / Possible Hallucination")
        };

        markdown.push_str("## 📊 Fidelity Statistics\n");
        markdown.push_str(&format!("- **Cosine Similarity**: `{:.4}`\n", similarity));
        markdown.push_str(&format!("- **Confidence Level**: {} — {}\n\n", conf_color, conf_label));

        if let Some(target_path) = steer_target_path {
            markdown.push_str(&format!("## 🧠 Steered Synthesis (Target: `{}`)\n\n", target_path));
            markdown.push_str(&format!("Active FST themes identified: {:?}\n\n", self.extracted_tags));

            if let Some(ref simulated) = self.steered_text {
                markdown.push_str("### 📝 Synthesized Local Text\n");
                markdown.push_str(&format!("> \"{}\"\n\n", simulated));

                if let Some(ref history) = self.attention_history {
                    markdown.push_str("### 👁️ FST Attention Feedback Traces\n");
                    let mut last_printed_token = 0;
                    for (token_idx, register) in history {
                        if *token_idx == 0 || token_idx - last_printed_token >= 8 {
                            let mut trace_strs = Vec::new();
                            for (tag, weight) in register {
                                trace_strs.push(format!("`{}` ({:.2})", tag, weight));
                            }
                            markdown.push_str(&format!("- **Token #{:3}**: {}\n", token_idx, trace_strs.join(", ")));
                            last_printed_token = *token_idx;
                        }
                    }
                }
            } else {
                markdown.push_str("_No text found in target document to train local Markov chain._\n");
            }
        }

        markdown
    }

    /// Outputs a stunning, premium terminal view of the vector inversion and steering flow.
    pub fn print_cli(
        &self,
        steer_target_path: Option<&str>,
        elapsed: std::time::Duration,
        index_elapsed: Option<std::time::Duration>,
    ) {
        println!("\x1B[32mSuccessfully inverted vector in {:.2?}!\x1B[0m", elapsed);
        println!();
        println!("────────────────────────────────────────────────────────────────────────");
        println!("\x1B[1;33mRECONSTRUCTED TEXT:\x1B[0m");
        println!("  \"{}\"", self.reconstructed_text);
        println!();

        let similarity = self.similarity;
        let (conf_color, conf_label) = if similarity >= 0.95 {
            ("\x1B[1;32m", "Highly Faithful (Lossless)")
        } else if similarity >= 0.75 {
            ("\x1B[1;36m", "Faithful (Slightly Lossy/Paraphrased)")
        } else if similarity >= 0.50 {
            ("\x1B[1;33m", "Lossy / Concept-Related")
        } else {
            ("\x1B[1;31m", "Low Confidence / Possible Hallucination")
        };

        println!("\x1B[1;33mFIDELITY METRICS:\x1B[0m");
        println!("  Cosine Similarity: \x1B[1m{:.4}\x1B[0m", similarity);
        println!("  Confidence Level:  {}{}\x1B[0m", conf_color, conf_label);
        println!("────────────────────────────────────────────────────────────────────────");
        println!();

        if let Some(target_path) = steer_target_path {
            println!("\x1B[34mTriggering Steered Synthesis using reconstructed concepts over: {}...\x1B[0m", target_path);
            if let Some(idx_el) = index_elapsed {
                println!("\x1B[32mIndexed local corpus in {:.2?}\x1B[0m", idx_el);
            }
            println!("\x1B[1;32mFST Tagging identified active themes in reconstructed text:\x1B[0m {:?}", self.extracted_tags);

            if let Some(ref simulated) = self.steered_text {
                println!();
                println!("────────────────────────────────────────────────────────────────────────");
                println!("\x1B[1;35mSTEERED STOCHASTIC SYNTHESIS:\x1B[0m");
                println!("  \"{}\"", simulated);
                println!("────────────────────────────────────────────────────────────────────────");
                println!();
            }
        }
    }
}
