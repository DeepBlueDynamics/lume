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
    events: bool,
) -> Result<(), String> {
    let decide = crate::decide::current_decide_config();
    run_chat_loop_with_decide(
        question,
        ollama_url,
        ollama_model,
        db_dir,
        verbose,
        ti_store,
        docs_index,
        json_output,
        &decide,
        events,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn run_chat_loop_with_decide(
    question: &str,
    ollama_url: &str,
    ollama_model: &str,
    db_dir: &str,
    verbose: bool,
    ti_store: Option<&str>,
    docs_index: Option<&str>,
    json_output: bool,
    decide: &crate::decide::DecideConfig,
    events: bool,
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

    let system_prompt = build_system_prompt_with_examples(
        ti_store,
        docs_index,
        db_dir,
        question,
        decide.no_examples,
    );

    if verbose && !json_output {
        println!("[Agent] System prompt:\n{}", system_prompt);
    }

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
    let mut last_successful_sql: Option<String> = None;
    let mut last_successful_rows: Vec<Value> = Vec::new();
    let mut last_successful_truncated = false;
    let mut last_sql_error: Option<String> = None;

    let max_turns = 10;
    for turn in 1..=max_turns {
        if events {
            emit_event(&json!({
                "event": "thinking",
                "turn": turn,
            }));
        }
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

        let mut assistant_msg = chat_res.message;
        if assistant_msg
            .tool_calls
            .as_ref()
            .is_none_or(|tc| tc.is_empty())
        {
            if let Some(synthesized) =
                synthesize_unexecuted_sql_tool_call(&assistant_msg.content, &executed_sql, ti_store)
            {
                assistant_msg.tool_calls = Some(synthesized);
            }
        }

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
                        decide,
                        question,
                        last_successful_sql.as_deref(),
                        &last_successful_rows,
                        last_successful_truncated,
                        last_sql_error.as_deref(),
                        ollama_model,
                        ti_store,
                        docs_index,
                        db_dir,
                    );
                }
            } else {
                for call in tool_calls {
                    let tool_name = &call.function.name;
                    let tool_args = &call.function.arguments;
                    if events {
                        emit_event(&json!({
                            "event": "tool_call",
                            "turn": turn,
                            "name": tool_name,
                            "args": tool_args,
                        }));
                    }
                    if !json_output {
                        println!(
                            "[Agent] Executing tool '{}' with arguments: {}",
                            tool_name, tool_args
                        );
                    }

                    let current_sql = tool_args
                        .get("sql")
                        .and_then(|v| v.as_str())
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string);
                    if let Some(ref trimmed) = current_sql {
                        if !executed_sql.contains(trimmed) {
                            executed_sql.push(trimmed.clone());
                        }
                    }

                    let start = std::time::Instant::now();
                    let tool_result =
                        execute_chat_tool(tool_name, tool_args.clone(), ti_store, docs_index);
                    let elapsed_ms = start.elapsed().as_millis() as u64;
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
                                if let Some(ref sql_str) = current_sql {
                                    last_successful_sql = Some(sql_str.clone());
                                    last_successful_rows = parsed
                                        .get("rows")
                                        .and_then(|v| v.as_array())
                                        .cloned()
                                        .unwrap_or_default();
                                    last_successful_truncated = truncated.unwrap_or(false);
                                }
                            } else if let Some(ref sql_str) = current_sql {
                                last_successful_sql = Some(sql_str.clone());
                                last_successful_rows.clear();
                                last_successful_truncated = false;
                            }

                            if events {
                                emit_event(&json!({
                                    "event": "tool_result",
                                    "turn": turn,
                                    "name": tool_name,
                                    "rows": rows,
                                    "elapsed_ms": elapsed_ms,
                                    "error": Value::Null,
                                }));
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
                            if current_sql.is_some() {
                                last_sql_error = Some(err.clone());
                            }
                            if events {
                                emit_event(&json!({
                                    "event": "tool_result",
                                    "turn": turn,
                                    "name": tool_name,
                                    "rows": Value::Null,
                                    "elapsed_ms": elapsed_ms,
                                    "error": err.clone(),
                                }));
                            }

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
                decide,
                question,
                last_successful_sql.as_deref(),
                &last_successful_rows,
                last_successful_truncated,
                last_sql_error.as_deref(),
                ollama_model,
                ti_store,
                docs_index,
                db_dir,
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

fn synthesize_unexecuted_sql_tool_call(
    content: &str,
    executed_sql: &[String],
    ti_store: Option<&str>,
) -> Option<Vec<AgentToolCall>> {
    for part in content.split("```sql").skip(1) {
        if let Some(end) = part.find("```") {
            let sql_snippet = part[..end].trim().trim_end_matches(';').trim();
            if !sql_snippet.is_empty()
                && !executed_sql
                    .iter()
                    .any(|s| s.trim_end_matches(';').trim() == sql_snippet)
            {
                let tool_name = if ti_store.is_some() {
                    "ti_query"
                } else {
                    "lume_sql"
                };
                return Some(vec![AgentToolCall {
                    function: AgentFunctionCall {
                        name: tool_name.to_string(),
                        arguments: json!({ "sql": sql_snippet }),
                    },
                }]);
            }
        }
    }
    None
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
                    obj.insert("store".to_string(), json!(store));
                    obj.remove("width_seconds");
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

#[allow(clippy::too_many_arguments)]
fn finish_agent(
    content: String,
    mut executed_sql: Vec<String>,
    tool_calls: Vec<ToolCallRecord>,
    json_output: bool,
    decide: &crate::decide::DecideConfig,
    question: &str,
    last_successful_sql: Option<&str>,
    last_successful_rows: &[Value],
    last_successful_truncated: bool,
    last_sql_error: Option<&str>,
    ollama_model: &str,
    ti_store: Option<&str>,
    docs_index: Option<&str>,
    db_dir: &str,
) -> Result<(), String> {
    for part in content.split("```sql").skip(1) {
        if let Some(end) = part.find("```") {
            let sql_snippet = part[..end].trim();
            if !sql_snippet.is_empty() && !executed_sql.contains(&sql_snippet.to_string()) {
                executed_sql.push(sql_snippet.to_string());
            }
        }
    }

    if json_output {
        let out = AgentJsonOutput {
            answer: content.clone(),
            sql: executed_sql.clone(),
            tool_calls,
        };
        println!("{}", serde_json::to_string(&out).unwrap_or_default());
    } else {
        println!("\n[Agent Final Answer]\n{}", content);
    }
    let _ = std::io::Write::flush(&mut std::io::stdout());

    crate::decide::finish_run_decide_and_log(
        decide,
        question,
        &executed_sql,
        last_successful_sql,
        last_successful_rows,
        last_successful_truncated,
        last_sql_error,
        &content,
        ollama_model,
        ti_store,
        docs_index,
        db_dir,
    );

    Ok(())
}

pub fn build_system_prompt_with_examples(
    ti_store: Option<&str>,
    docs_index: Option<&str>,
    db_dir: &str,
    question: &str,
    no_examples: bool,
) -> String {
    let mut p = build_system_prompt(ti_store, docs_index, db_dir);
    if !no_examples {
        if let Some(store_root) =
            crate::decide::resolve_run_log_store_dir(ti_store, docs_index, db_dir)
        {
            let examples = crate::decide::select_top_sql_examples(&store_root, question);
            if let Some(block) = crate::decide::format_examples_for_prompt(&examples) {
                p.push_str(&block);
            }
        }
    }
    p
}

pub fn build_system_prompt(
    ti_store: Option<&str>,
    docs_index: Option<&str>,
    db_dir: &str,
) -> String {
    if ti_store.is_some() {
        let mut p = String::from(
            "You are an expert Q&A and SQL analytics agent for Signal K marine telemetry and documentation.\n\
CRITICAL RULES:\n\
1. ALWAYS CALL ti_schema FIRST before writing or executing your first SQL query. Check the available tables, columns with their types, units, and time coverage.\n\
2. SQL CONVENTIONS:\n\
   - Dotted column names must be double-quoted, e.g. \"navigation.speedOverGround\", \"navigation.speedOverGround@max\", \"environment.wind.speedTrue\".\n\
   - Time buckets: use date_bin(INTERVAL '1 hour', ts) or date_bin(INTERVAL '10 minutes', ts) for time aggregations.\n\
   - Vessel filter: filter by vessel URN or MMSI where appropriate (e.g. vessel = '...').\n\
   - Documents & full-text search (examples show syntax only; never copy placeholder words):\n\
     * docs hold logs and notes; kinds and titles (e.g. radio.weather) are listed by ti_schema. They are values in docs.title/kind, not table names. For the latest entry: ORDER BY ts_start DESC.\n\
     * JSON fields in docs.body can be extracted with json_str(body, '<key>') and json_num(body, '<key>'), or read directly from body.\n\
     * The TI docs table (ti_sql) has docs(vessel, id, kind, title, body, ts_start, ts_end, score). Query by title from docs_kinds and order by ts_start DESC (do not use match() or LIKE on body when querying by a known docs title, because radio transcripts contain phonetic spelling errors):\n\
       Example: SELECT title, body, ts_start FROM docs WHERE title = '<title_from_docs_kinds>' ORDER BY ts_start DESC LIMIT 5;\n\
     * Full-text search: match(body, '<key words from the question>') is a Boolean filter for WHERE only when searching across unknown titles or library sections. Never compare it (> 0), select it as a number, or aggregate it (sum). Relevance is the score column; it is filled when the WHERE clause uses match(); use ORDER BY score DESC.\n\
       Example: SELECT vessel, kind, title, body, score FROM docs WHERE match(body, '<key words from the question>') ORDER BY score DESC LIMIT 10;\n\
     * The library index (lume_sql) has table sections(id, file, title, line, body, score). Group or identify documents by file (there is no doc column), and use line for position.\n\
       Example: SELECT file, title, line, score FROM sections WHERE match(body, '<key words from the question>') ORDER BY score DESC LIMIT 10;\n\
   - Geospatial & intervals: use intervals() for time ranges, and within_nm(lat, lon, target_lat, target_lon, nm) or in_bbox(lat, lon, min_lat, min_lon, max_lat, max_lon) for geographic queries.\n\
3. RETRY ON ERROR: If a SQL query fails with an error, analyze the error message, correct the SQL query, and retry (up to 3 times).\n\
4. FINAL ANSWER: Do not paste SQL queries into your final answer unless the user explicitly asks for them (the UI already displays executed queries in dedicated cards). Provide a direct, factual answer and cite the source file and line for document references.\n\
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
    }
}

fn emit_event(val: &Value) {
    if let Ok(line) = serde_json::to_string(val) {
        eprintln!("{}", line);
    }
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

    #[test]
    fn system_prompt_includes_document_match_semantics_and_examples() {
        let prompt = build_system_prompt(Some("/path/to/ti"), Some("/path/to/docs"), ".lume-index");
        assert!(prompt.contains(
            "docs hold logs and notes; kinds and titles (e.g. radio.weather) are listed by ti_schema. They are values in docs.title/kind, not table names. For the latest entry: ORDER BY ts_start DESC."
        ));
        assert!(prompt.contains(
            "match(body, '<key words from the question>') is a Boolean filter for WHERE only"
        ));
        assert!(prompt.contains("Never compare it (> 0)"));
        assert!(prompt.contains("Relevance is the score column"));
        assert!(prompt.contains("ORDER BY score DESC"));
        assert!(prompt.contains("sections(id, file, title, line, body, score)"));
        assert!(prompt.contains("docs(vessel, id, kind, title, body, ts_start, ts_end, score)"));
        assert!(prompt.contains("SELECT title, body, ts_start FROM docs WHERE title = '<title_from_docs_kinds>' ORDER BY ts_start DESC LIMIT 5;"));
        assert!(prompt.contains("SELECT file, title, line, score FROM sections WHERE match(body, '<key words from the question>') ORDER BY score DESC LIMIT 10;"));
        assert!(prompt.contains("SELECT vessel, kind, title, body, score FROM docs WHERE match(body, '<key words from the question>') ORDER BY score DESC LIMIT 10;"));
        assert!(!prompt.contains("generator maintenance"));
        assert!(!prompt.contains("engine repair"));
        assert!(prompt.contains(
            "Do not paste SQL queries into your final answer unless the user explicitly asks"
        ));
        assert!(prompt.contains("cite the source file and line"));
    }

    #[test]
    fn fenced_sql_synthesis_runs_at_most_once_and_skips_already_executed_sql() {
        // (a) Executes at most once per turn even when multiple ```sql blocks appear.
        let two_blocks = "Let's run:\n```sql\nSELECT 1;\n```\nand then:\n```sql\nSELECT 2;\n```";
        let calls = synthesize_unexecuted_sql_tool_call(two_blocks, &[], Some("/tmp/ti")).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "ti_query");
        assert_eq!(calls[0].function.arguments["sql"], "SELECT 1");

        // (b) SQL already executed isn't re-run; advances to the next unexecuted block.
        let executed = vec!["SELECT 1".to_string()];
        let calls_after_first =
            synthesize_unexecuted_sql_tool_call(two_blocks, &executed, None).unwrap();
        assert_eq!(calls_after_first.len(), 1);
        assert_eq!(calls_after_first[0].function.name, "lume_sql");
        assert_eq!(calls_after_first[0].function.arguments["sql"], "SELECT 2");

        // (c) A final answer that only quotes already-executed SQL returns None so the loop ends.
        let final_answer = "The temperature was 89°F.\n```sql\nSELECT title, body FROM docs WHERE title = 'radio.weather' ORDER BY ts_start DESC LIMIT 1;\n```";
        let prev = vec![
            "SELECT title, body FROM docs WHERE title = 'radio.weather' ORDER BY ts_start DESC LIMIT 1"
                .to_string(),
        ];
        assert!(
            synthesize_unexecuted_sql_tool_call(final_answer, &prev, Some("/tmp/ti")).is_none()
        );
    }
}
