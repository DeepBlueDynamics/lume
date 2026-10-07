//! Chat SQL tool-calling loop for lume chat / agent.
#![cfg(feature = "ti")]

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;

#[derive(Serialize, Deserialize, Debug, Clone)]
struct AgentMessage {
    role: String,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<AgentToolCall>>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct AgentToolCall {
    function: AgentFunctionCall,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct AgentFunctionCall {
    name: String,
    arguments: Value,
}

#[derive(Serialize, Clone)]
struct Tool {
    #[serde(rename = "type")]
    tool_type: String,
    function: Function,
}

#[derive(Serialize, Clone)]
struct Function {
    name: String,
    description: String,
    parameters: Value,
}

#[derive(Serialize, Clone)]
struct Options {
    temperature: f32,
    num_ctx: u32,
}

#[derive(Serialize)]
struct AgentChatPayload {
    model: String,
    messages: Vec<AgentMessage>,
    tools: Vec<Tool>,
    stream: bool,
    options: Options,
}

#[derive(Serialize, Debug, Clone)]
pub struct ToolCallRecord {
    pub name: String,
    pub args: Value,
    pub rows: Option<usize>,
    pub truncated: Option<bool>,
    pub error: Option<String>,
}

#[derive(Serialize, Debug)]
pub struct AgentJsonOutput {
    pub answer: String,
    pub sql: Vec<String>,
    pub tool_calls: Vec<ToolCallRecord>,
}

/// Pick the Ollama endpoint for this question from a comma-separated list, in order:
/// the first reachable one that already has `model`, else the first reachable one.
/// On a boat this is "the Pi's own Ollama, then the laptop's over the LAN".
fn select_ollama_endpoint(raw: &str, model: &str) -> Result<String, String> {
    let mut ordered: Vec<String> = Vec::new();
    for url in raw.split(',').map(resolve_ollama_url) {
        if !ordered.contains(&url) {
            ordered.push(url);
        }
    }
    if ordered.len() == 1 {
        return Ok(ordered[0].clone());
    }
    let mut first_reachable = None;
    for base in &ordered {
        let request = with_ollama_auth(
            ureq::get(&format!("{base}/api/tags")).timeout(std::time::Duration::from_secs(2)),
            base,
        );
        let Ok(response) = request.call() else {
            continue;
        };
        let names: Vec<String> = response
            .into_json::<serde_json::Value>()
            .ok()
            .and_then(|v| v.get("models").and_then(|m| m.as_array()).cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|m| m.get("name").and_then(|n| n.as_str()).map(str::to_string))
            .collect();
        if names
            .iter()
            .any(|n| n == model || n.trim_end_matches(":latest") == model)
        {
            return Ok(base.clone());
        }
        first_reachable.get_or_insert_with(|| base.clone());
    }
    first_reachable.ok_or_else(|| {
        format!(
            "Ollama is unreachable at {}. Check that Ollama is running and accessible.",
            ordered.join(", ")
        )
    })
}

/// `OLLAMA_API_KEY` is sent only to ollama.com (direct cloud use). A local Ollama that
/// ran `ollama signin` serves `:cloud` models itself and needs no key from Lume.
fn with_ollama_auth(request: ureq::Request, base: &str) -> ureq::Request {
    match std::env::var("OLLAMA_API_KEY") {
        Ok(key) if is_ollama_cloud(base) && !key.trim().is_empty() => {
            request.set("Authorization", &format!("Bearer {}", key.trim()))
        }
        _ => request,
    }
}

fn is_ollama_cloud(base: &str) -> bool {
    base.split("://")
        .nth(1)
        .and_then(|rest| rest.split(['/', ':']).next())
        .is_some_and(|host| host == "ollama.com" || host.ends_with(".ollama.com"))
}

fn resolve_ollama_url(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        "http://localhost:11434".to_string()
    } else if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") {
        format!("http://{}", trimmed)
    } else {
        trimmed.to_string()
    }
}

#[allow(clippy::too_many_arguments)]
pub fn run_chat_loop(
    question: &str,
    ollama_url: &str,
    ollama_model: &str,
    db_dir: &str,
    verbose: bool,
    ti_store: Option<&str>,
    docs_index: Option<&str>,
    json_output: bool,
) -> Result<(), String> {
    let base = match select_ollama_endpoint(ollama_url, ollama_model) {
        Ok(base) => base,
        Err(message) if json_output => {
            let out = AgentJsonOutput {
                answer: message,
                sql: Vec::new(),
                tool_calls: Vec::new(),
            };
            println!("{}", serde_json::to_string(&out).unwrap_or_default());
            return Ok(());
        }
        Err(message) => return Err(message),
    };
    let url = format!("{base}/api/chat");

    let system_prompt = if ti_store.is_some() {
        let mut p = String::from(
            "You are an expert Q&A and SQL analytics agent for Signal K marine telemetry and documentation.\n\
CRITICAL RULES:\n\
1. ALWAYS CALL ti_schema FIRST before writing or executing your first SQL query. Check the available tables, columns with their types, units, and time coverage.\n\
2. SQL CONVENTIONS:\n\
   - Dotted column names must be double-quoted, e.g. \"navigation.speedOverGround\", \"navigation.speedOverGround@max\", \"environment.wind.speedTrue\".\n\
   - Time buckets: use date_bin(INTERVAL '1 hour', ts) or date_bin(INTERVAL '10 minutes', ts) for time aggregations.\n\
   - Vessel filter: filter by vessel URN or MMSI where appropriate (e.g. vessel = '...').\n\
   - Documents & full-text: use match(body, 'words') for document searches.\n\
   - Geospatial & intervals: use intervals() for time ranges, and within_nm(lat, lon, target_lat, target_lon, nm) or in_bbox(lat, lon, min_lat, min_lon, max_lat, max_lon) for geographic queries.\n\
3. RETRY ON ERROR: If a SQL query fails with an error, analyze the error message, correct the SQL query, and retry (up to 3 times).\n\
4. FINAL ANSWER: In your final answer, include every SQL statement you ran inside a fenced code block (```sql ... ```) so the user can inspect and reuse it.\n\
5. Keep your final answer concise, factual, and directly supported by query results."
        );
        if let Some(store) = ti_store {
            p.push_str(&format!(
                "\nThe target TI store is located at: '{}'.",
                store
            ));
        }
        if let Some(docs) = docs_index {
            p.push_str(&format!("\nThe documentation index is located at: '{}'. You can use lume_sql to query document sections and entities.", docs));
        }
        p
    } else {
        format!(
            "You are an expert Q&A agent. Your goal is to answer the user's question using the Lume search tool. The target index is at '{}'.",
            db_dir
        )
    };

    let mut messages = vec![
        AgentMessage {
            role: "system".to_string(),
            content: system_prompt,
            tool_calls: None,
        },
        AgentMessage {
            role: "user".to_string(),
            content: question.to_string(),
            tool_calls: None,
        },
    ];

    let mut tools = Vec::new();

    if ti_store.is_some() {
        // Shared MCP definitions carry the data-model guide; chat adds its schema-first rule.
        for def in crate::ti_mcp::definitions(None) {
            let name = def["name"].as_str().unwrap_or("");
            if matches!(name, "ti_schema" | "ti_query" | "ti_explain") {
                let base = def["description"].as_str().unwrap_or("");
                let desc = if name == "ti_schema" {
                    format!(
                        "{base} Always call this first before writing or executing any SQL query."
                    )
                } else {
                    base.to_string()
                };
                tools.push(Tool {
                    tool_type: "function".to_string(),
                    function: Function {
                        name: name.to_string(),
                        description: desc.to_string(),
                        parameters: def["inputSchema"].clone(),
                    },
                });
            }
        }
    }

    if docs_index.is_some() {
        let def = crate::sql::definition();
        tools.push(Tool {
            tool_type: "function".to_string(),
            function: Function {
                name: def["name"].as_str().unwrap_or("lume_sql").to_string(),
                description: def["description"].as_str().unwrap_or("").to_string(),
                parameters: def["inputSchema"].clone(),
            },
        });
    }

    if !json_output {
        println!("[Agent] Starting task: {}", question);
    }

    let mut executed_sql: Vec<String> = Vec::new();
    let mut recorded_tool_calls: Vec<ToolCallRecord> = Vec::new();
    let mut sql_error_counts: HashMap<String, usize> = HashMap::new();

    let max_turns = 10;
    for turn in 1..=max_turns {
        let payload = AgentChatPayload {
            model: ollama_model.to_string(),
            messages: messages.clone(),
            tools: tools.clone(),
            stream: false,
            options: Options {
                temperature: 0.0,
                num_ctx: 16384,
            },
        };

        let response = match with_ollama_auth(ureq::post(&url), &base)
            .set("Content-Type", "application/json")
            .timeout(std::time::Duration::from_secs(300))
            .send_json(&payload)
        {
            Ok(resp) => resp,
            Err(e) => {
                let err_msg = format!("Ollama API request failed: {}", e);
                if json_output {
                    let out = AgentJsonOutput {
                        answer: format!(
                            "Ollama is unreachable at {}. Check that Ollama is running and accessible.",
                            base
                        ),
                        sql: executed_sql,
                        tool_calls: recorded_tool_calls,
                    };
                    println!("{}", serde_json::to_string(&out).unwrap_or_default());
                    return Ok(());
                } else {
                    return Err(err_msg);
                }
            }
        };

        if response.status() != 200 {
            let status = response.status();
            let err_body = response
                .into_string()
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(format!(
                "Ollama returned HTTP status {}: {}",
                status, err_body
            ));
        }

        #[derive(Deserialize, Debug)]
        struct AgentChatResponse {
            message: AgentMessage,
        }

        let chat_res: AgentChatResponse = response
            .into_json()
            .map_err(|e| format!("Failed to parse Ollama response JSON: {}", e))?;

        let assistant_msg = chat_res.message;

        if verbose && !json_output {
            println!(
                "[Agent] Turn {}: Model returned message:\n{}",
                turn,
                serde_json::to_string_pretty(&assistant_msg).unwrap_or_default()
            );
        } else if !json_output {
            let tool_desc = if let Some(ref tc) = assistant_msg.tool_calls {
                let names: Vec<String> = tc.iter().map(|c| c.function.name.clone()).collect();
                format!("{:?}", names)
            } else {
                "None".to_string()
            };
            println!(
                "[Agent] Turn {}: Model returned tool_calls={}",
                turn, tool_desc
            );
        }

        messages.push(assistant_msg.clone());

        if let Some(ref tool_calls) = assistant_msg.tool_calls {
            if tool_calls.is_empty() {
                if !assistant_msg.content.trim().is_empty() {
                    return finish_agent(
                        assistant_msg.content,
                        executed_sql,
                        recorded_tool_calls,
                        json_output,
                    );
                }
            } else {
                for call in tool_calls {
                    let tool_name = &call.function.name;
                    let tool_args = &call.function.arguments;
                    if !json_output {
                        println!(
                            "[Agent] Executing tool '{}' with arguments: {}",
                            tool_name, tool_args
                        );
                    }

                    if let Some(s) = tool_args.get("sql").and_then(|v| v.as_str()) {
                        let trimmed = s.trim();
                        if !trimmed.is_empty() && !executed_sql.contains(&trimmed.to_string()) {
                            executed_sql.push(trimmed.to_string());
                        }
                    }

                    let tool_result =
                        execute_chat_tool(tool_name, tool_args.clone(), ti_store, docs_index);
                    match tool_result {
                        Ok(output) => {
                            let mut rows = None;
                            let mut truncated = None;
                            if let Ok(parsed) = serde_json::from_str::<Value>(&output) {
                                rows = parsed
                                    .get("row_count")
                                    .and_then(|v| v.as_u64())
                                    .map(|n| n as usize)
                                    .or_else(|| {
                                        parsed
                                            .get("rows")
                                            .and_then(|v| v.as_array())
                                            .map(|a| a.len())
                                    });
                                truncated = parsed.get("truncated").and_then(|v| v.as_bool());
                            }

                            recorded_tool_calls.push(ToolCallRecord {
                                name: tool_name.clone(),
                                args: tool_args.clone(),
                                rows,
                                truncated,
                                error: None,
                            });

                            if verbose && !json_output {
                                println!("[Agent] Tool returned output:\n{}", output);
                            } else if !json_output {
                                println!("[Agent] Tool returned output (length: {})", output.len());
                            }

                            messages.push(AgentMessage {
                                role: "tool".to_string(),
                                content: output,
                                tool_calls: None,
                            });
                        }
                        Err(err) => {
                            recorded_tool_calls.push(ToolCallRecord {
                                name: tool_name.clone(),
                                args: tool_args.clone(),
                                rows: None,
                                truncated: None,
                                error: Some(err.clone()),
                            });

                            let count = sql_error_counts.entry(tool_name.clone()).or_insert(0);
                            *count += 1;

                            let content = if *count <= 3 {
                                format!(
                                    "SQL error: {}. Please analyze this error, correct the SQL query, and retry.",
                                    err
                                )
                            } else {
                                format!(
                                    "SQL error: {}. Maximum retry attempts (3) exceeded for this query.",
                                    err
                                )
                            };

                            if verbose && !json_output {
                                println!("[Agent] Tool error:\n{}", content);
                            } else if !json_output {
                                println!("[Agent] Tool error: {}", err);
                            }

                            messages.push(AgentMessage {
                                role: "tool".to_string(),
                                content,
                                tool_calls: None,
                            });
                        }
                    }
                }
            }
        } else if !assistant_msg.content.trim().is_empty() {
            return finish_agent(
                assistant_msg.content,
                executed_sql,
                recorded_tool_calls,
                json_output,
            );
        }
    }

    if json_output {
        let out = AgentJsonOutput {
            answer: format!(
                "Agent exceeded maximum turns ({}) without finding a final answer.",
                max_turns
            ),
            sql: executed_sql,
            tool_calls: recorded_tool_calls,
        };
        println!("{}", serde_json::to_string(&out).unwrap_or_default());
        Ok(())
    } else {
        Err(format!(
            "Agent exceeded maximum turns ({}) without finding a final answer.",
            max_turns
        ))
    }
}

fn execute_chat_tool(
    name: &str,
    mut args: Value,
    ti_store: Option<&str>,
    docs_index: Option<&str>,
) -> Result<String, String> {
    match name {
        "ti_schema" | "ti_query" | "ti_explain" => {
            if let Some(store) = ti_store {
                if let Some(obj) = args.as_object_mut() {
                    if !obj.contains_key("store") {
                        obj.insert("store".to_string(), json!(store));
                    }
                }
            }
            crate::ti_mcp::call(name, args)
        }
        "lume_sql" => {
            let db = docs_index.unwrap_or(".lume-index");
            crate::sql::call(args, db)
        }
        _ => Err(format!("Unknown tool: {}", name)),
    }
}

fn finish_agent(
    content: String,
    mut executed_sql: Vec<String>,
    tool_calls: Vec<ToolCallRecord>,
    json_output: bool,
) -> Result<(), String> {
    for part in content.split("```sql") {
        if let Some(end) = part.find("```") {
            let sql_snippet = part[..end].trim();
            if !sql_snippet.is_empty() && !executed_sql.contains(&sql_snippet.to_string()) {
                executed_sql.push(sql_snippet.to_string());
            }
        }
    }

    if json_output {
        let out = AgentJsonOutput {
            answer: content,
            sql: executed_sql,
            tool_calls,
        };
        println!("{}", serde_json::to_string(&out).unwrap_or_default());
    } else {
        println!("\n[Agent Final Answer]\n{}", content);
    }
    Ok(())
}

#[cfg(test)]
mod endpoint_tests {
    use super::*;

    #[test]
    fn a_single_endpoint_is_used_without_probing() {
        assert_eq!(
            select_ollama_endpoint("127.0.0.1:11434/", "m").unwrap(),
            "http://127.0.0.1:11434"
        );
    }

    #[test]
    fn unreachable_endpoints_are_all_named() {
        // Port 9 (discard) on loopback refuses connections immediately.
        let error =
            select_ollama_endpoint("http://127.0.0.1:9, http://127.0.0.2:9", "m").unwrap_err();
        assert!(
            error.contains("http://127.0.0.1:9, http://127.0.0.2:9"),
            "{error}"
        );
    }

    #[test]
    fn the_api_key_goes_only_to_ollama_com() {
        assert!(is_ollama_cloud("https://ollama.com"));
        assert!(is_ollama_cloud("https://api.ollama.com/v1"));
        assert!(!is_ollama_cloud("http://127.0.0.1:11434"));
        assert!(!is_ollama_cloud("https://ollama.com.evil.example"));
    }
}
