use serde::{Deserialize, Serialize};
use serde_json::json;
use std::net::{TcpListener, TcpStream};
use std::io::{Read, Write};

#[derive(Serialize)]
struct ChatPayload<'a> {
    model: &'a str,
    messages: Vec<Message<'a>>,
    tools: Vec<Tool>,
    stream: bool,
    options: Options,
}

#[derive(Serialize, Deserialize)]
struct Message<'a> {
    role: &'a str,
    content: &'a str,
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
    parameters: serde_json::Value,
}

#[derive(Serialize)]
struct Options {
    temperature: f64,
    num_ctx: usize,
}

#[derive(Deserialize, Debug)]
struct ChatResponse {
    message: MessageResponse,
}

#[derive(Deserialize, Debug)]
struct MessageResponse {
    content: String,
    #[serde(default)]
    tool_calls: Vec<ToolCall>,
}

#[derive(Deserialize, Debug)]
struct ToolCall {
    function: FunctionCall,
}

#[derive(Deserialize, Debug)]
struct FunctionCall {
    name: String,
    arguments: serde_json::Value,
}

/// Resolves the effective Ollama endpoint. When the caller passes the default
/// (or empty) URL, probes common local endpoints (Docker host bridge vs native
/// localhost) and caches the winner for the life of the process so repeated
/// calls don't re-probe.
pub fn resolve_ollama_url(ollama_url: &str) -> String {
    if !ollama_url.is_empty() && ollama_url != "http://localhost:11434" {
        return ollama_url.trim_end_matches('/').to_string();
    }
    static RESOLVED: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    RESOLVED
        .get_or_init(|| {
            let endpoints = [
                "http://host.docker.internal:11434",
                "http://localhost:11434",
                "http://172.17.0.1:11434",
            ];
            for ep in &endpoints {
                if let Ok(res) = ureq::get(&format!("{}/api/tags", ep))
                    .timeout(std::time::Duration::from_secs(2))
                    .call()
                {
                    if res.status() == 200 {
                        return ep.to_string();
                    }
                }
            }
            "http://localhost:11434".to_string()
        })
        .clone()
}

/// Calls local/remote Ollama chat endpoint to extract key concepts,
/// proper names, organizations, locations, and terms from a text chunk.
pub fn extract_entities(
    text: &str,
    ollama_url: &str,
    ollama_model: &str,
) -> Result<Vec<String>, String> {
    let url = format!("{}/api/chat", resolve_ollama_url(ollama_url));

    let payload = ChatPayload {
        model: ollama_model,
        messages: vec![
            Message {
                role: "system",
                content: "You are a helpful assistant. You must extract all key concepts, proper names, organizations, locations, and terms from the text and record them by calling the 'extract_entities' tool. Always call the tool, do not write a conversational response.",
            },
            Message {
                role: "user",
                content: text,
            },
        ],
        tools: vec![Tool {
            tool_type: "function".to_string(),
            function: Function {
                name: "extract_entities".to_string(),
                description: "Call this tool to record the list of extracted entities from the text.".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "entities": {
                            "type": "array",
                            "items": {
                                "type": "string"
                            },
                            "description": "List of key concepts, proper names, organizations, locations, and terms extracted from the text."
                        }
                    },
                    "required": ["entities"]
                }),
            },
        }],
        stream: false,
        options: Options {
            temperature: 0.0,
            num_ctx: 16384,
        },
    };

    let response = ureq::post(&url)
        .set("Content-Type", "application/json")
        .timeout(std::time::Duration::from_secs(300))
        .send_json(&payload)
        .map_err(|e| format!("Ollama API request failed: {}", e))?;

    if response.status() != 200 {
        let status = response.status();
        let err_body = response.into_string().unwrap_or_else(|_| "Unknown error".to_string());
        return Err(format!("Ollama returned HTTP status {}: {}", status, err_body));
    }

    let chat_res: ChatResponse = response
        .into_json()
        .map_err(|e| format!("Failed to parse Ollama chat response JSON: {}", e))?;

    let mut entities = Vec::new();

    // 1. Process tool calls if present
    for call in &chat_res.message.tool_calls {
        if call.function.name == "extract_entities" {
            let args = &call.function.arguments;
            let parsed_args = if let serde_json::Value::String(ref s) = args {
                serde_json::from_str::<serde_json::Value>(s).unwrap_or(serde_json::Value::Null)
            } else {
                args.clone()
            };

            if let serde_json::Value::Object(map) = parsed_args {
                if let Some(serde_json::Value::Array(arr)) = map.get("entities") {
                    for val in arr {
                        if let Some(s) = val.as_str() {
                            entities.push(s.trim().to_string());
                        }
                    }
                }
            }
        }
    }

    // 2. Fallback: Parse raw text response if no tool calls were triggered
    if entities.is_empty() {
        let content = chat_res.message.content.trim();
        if !content.is_empty() {
            // Clean markdown block wrappers if present
            let cleaned_storage;
            let mut cleaned_content = content;
            if cleaned_content.starts_with("```") {
                let lines: Vec<&str> = cleaned_content.lines().collect();
                if lines.len() >= 2 && lines.last() == Some(&"```") {
                    cleaned_storage = lines[1..lines.len() - 1].join("\n");
                    cleaned_content = cleaned_storage.trim();
                }
            }
            if let Ok(serde_json::Value::Array(arr)) = serde_json::from_str(cleaned_content) {
                for val in arr {
                    if let Some(s) = val.as_str() {
                        entities.push(s.trim().to_string());
                    }
                }
            }
        }
    }

    entities.retain(|s| !s.is_empty());
    Ok(entities)
}

// --- MCP Server / HTTP Transport implementation ---

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

fn run_lume_cli(args: Vec<String>) -> Result<String, String> {
    let current_exe = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("lume"));
    let output = std::process::Command::new(current_exe)
        .args(&args)
        .output()
        .map_err(|e| format!("Failed to execute lume binary: {}", e))?;
    
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    
    if output.status.success() {
        Ok(stdout)
    } else {
        Err(format!("Lume CLI failed (exit code {}):\nstdout: {}\nstderr: {}", 
            output.status.code().map_or("unknown".to_string(), |c| c.to_string()),
            stdout, stderr))
    }
}

fn execute_tool_by_name(name: &str, args: serde_json::Value, default_db: &str) -> Result<String, String> {
    #[cfg(feature = "ti")]
    if matches!(name, "ti_query" | "ti_schema" | "ti_explain" | "ti_status" | "ti_resolve") {
        return crate::ti_mcp::call(name, args);
    }
    #[cfg(feature = "ti")]
    if name == "lume_sql" { return crate::sql::call(args, default_db); }
    match name {
        "lume_index" => {
            let db = args.get("db").and_then(|v| v.as_str()).unwrap_or(default_db);
            let force = args.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
            let semantic = args.get("semantic").and_then(|v| v.as_bool()).unwrap_or(false);
            let ollama_entities = args.get("ollama_entities").and_then(|v| v.as_bool()).unwrap_or(false);
            let ollama_model = args.get("ollama_model").and_then(|v| v.as_str());
            let ollama_url = args.get("ollama_url").and_then(|v| v.as_str());
            let shivvr_url = args.get("shivvr_url").and_then(|v| v.as_str());
            let tag_dict = args.get("tag_dict").and_then(|v| v.as_str());
            let dir = args.get("dir").and_then(|v| v.as_str());

            let mut cli_args = Vec::new();
            cli_args.push("index".to_string());

            let db_path = std::path::Path::new(db);
            let state_json_path = db_path.join("state.json");
            let is_update = (state_json_path.exists()
                || crate::index_binary::snapshot::present(db_path))
                && !force;

            if is_update {
                cli_args.push("update".to_string());
            }

            if force {
                cli_args.push("-f".to_string());
            }
            if semantic {
                cli_args.push("-s".to_string());
            }
            if ollama_entities {
                cli_args.push("-o".to_string());
            }
            cli_args.push("--db".to_string());
            cli_args.push(db.to_string());

            if let Some(m) = ollama_model {
                cli_args.push("--ollama-model".to_string());
                cli_args.push(m.to_string());
            }
            if let Some(u) = ollama_url {
                cli_args.push("--ollama-url".to_string());
                cli_args.push(u.to_string());
            }
            if let Some(s) = shivvr_url {
                cli_args.push("--shivvr-url".to_string());
                cli_args.push(s.to_string());
            }
            if let Some(td) = tag_dict {
                cli_args.push("--tag-dict".to_string());
                cli_args.push(td.to_string());
            }

            if !is_update {
                if let Some(d) = dir {
                    cli_args.push(d.to_string());
                } else {
                    return Err("Parameter 'dir' is required for initial indexing.".to_string());
                }
            }
            run_lume_cli(cli_args)
        }
        "lume_search" => {
            let query = args.get("query").and_then(|v| v.as_str()).ok_or_else(|| "Parameter 'query' is required.".to_string())?;
            let db = args.get("db").and_then(|v| v.as_str()).unwrap_or(default_db);
            let spell_check = args.get("spell_check").and_then(|v| v.as_bool()).unwrap_or(false);
            let limit = args.get("limit").and_then(|v| v.as_i64()).map(|v| v.max(0) as usize).unwrap_or(10);
            let alpha = args.get("alpha").and_then(|v| v.as_f64()).map(|v| v as f32).unwrap_or(0.5);
            let graph = args.get("graph").and_then(|v| v.as_f64()).unwrap_or(0.4);
            let shivvr_url = args.get("shivvr_url").and_then(|v| v.as_str()).map(|s| s.to_string());

            let index = crate::resident_index::open(db)?;
            let mut facet_requests = Vec::new();
            if let Some(facets_arr) = args.get("facets").and_then(|v| v.as_array()) {
                for item in facets_arr {
                    if let Some(s) = item.as_str() {
                        facet_requests.push(crate::meta::parse_facet_request(s)?);
                    }
                }
            }
            if let Some(facet_q_obj) = args.get("facet_queries").and_then(|v| v.as_object()) {
                for (name, q_val) in facet_q_obj {
                    if let Some(q_str) = q_val.as_str() {
                        facet_requests.push(crate::meta::FacetRequest::Query {
                            name: name.clone(),
                            query: q_str.to_string(),
                        });
                    }
                }
            }
            let mut opts = crate::search::SearchOptions {
                limit,
                spell_check,
                alpha,
                graph_beta: graph,
                shivvr_url,
                bm25_params: crate::bm25::Bm25Params::from_env(),
                facets: facet_requests,
                ..Default::default()
            };
            if alpha <= 0.0 {
                opts.mode = crate::search::SearchMode::LexicalOnly;
            }
            let results = crate::search::search(&index, query, &opts)?;
            let (stdout, _stderr) = crate::search::format_cli_output(&results, &index, db);
            Ok(stdout)
        }
        "lume_generate" => {
            let seed_word = args.get("seed_word").and_then(|v| v.as_str());
            let db = args.get("db").and_then(|v| v.as_str()).unwrap_or(default_db);
            let limit = args.get("limit").and_then(|v| v.as_i64());
            let steer = args.get("steer").and_then(|v| v.as_array());
            let attempts = args.get("attempts").and_then(|v| v.as_i64());
            let threshold = args.get("threshold").and_then(|v| v.as_f64());
            let shivvr_url = args.get("shivvr_url").and_then(|v| v.as_str());

            let mut cli_args = vec!["generate".to_string()];
            cli_args.push("--db".to_string());
            cli_args.push(db.to_string());

            if let Some(lim) = limit {
                cli_args.push("-l".to_string());
                cli_args.push(lim.to_string());
            }
            if let Some(st) = steer {
                let tags: Vec<String> = st.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect();
                if !tags.is_empty() {
                    cli_args.push("--steer".to_string());
                    cli_args.push(tags.join(","));
                }
            }
            if let Some(att) = attempts {
                cli_args.push("--attempts".to_string());
                cli_args.push(att.to_string());
            }
            if let Some(th) = threshold {
                cli_args.push("--threshold".to_string());
                cli_args.push(th.to_string());
            }
            if let Some(s) = shivvr_url {
                cli_args.push("--shivvr-url".to_string());
                cli_args.push(s.to_string());
            }
            if let Some(seed) = seed_word {
                cli_args.push(seed.to_string());
            }
            run_lume_cli(cli_args)
        }
        "lume_not_found" => {
            let reason = args.get("reason").and_then(|v| v.as_str()).unwrap_or("");
            let attempted = args.get("attempted_queries")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", "))
                .unwrap_or_default();
            Ok(format!(
                "Guidance: The previous search queries did not yield the answer. \
                Reason provided: '{}'. \
                Attempted queries: [{}]. \
                Please try a different search query focusing on specific exact phrases or key words from the question \
                (e.g., search for exact phrases in quotes like \"fancies himself\" or related words like \"captain\"). \
                Do not repeat the same queries.",
                reason, attempted
            ))
        }
        _ => Err(format!("Unknown tool: {}", name))
    }
}

#[cfg(feature = "ti")]
type TiState = Option<std::sync::Arc<crate::ti_http::TiServer>>;
#[cfg(not(feature = "ti"))]
type TiState = ();

fn handle_mcp_request(req_val: serde_json::Value, _ti: &TiState) -> serde_json::Value {
    let id = req_val.get("id").cloned().unwrap_or(serde_json::Value::Null);
    let method = match req_val.get("method").and_then(|m| m.as_str()) {
        Some(m) => m,
        None => {
            return json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32600, "message": "Invalid Request: missing method" }
            });
        }
    };

    match method {
        "initialize" => {
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {
                        "tools": {}
                    },
                    "serverInfo": {
                        "name": "lume-mcp",
                        "version": "0.10.0"
                    }
                }
            })
        }
        "tools/list" => {
            let response = json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "tools": [
                        {
                            "name": "lume_index",
                            "description": "Index a directory of text, code, and PDF files. Supports incremental updates. Automatically updates index if target db already has state.json.",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "dir": { "type": "string", "description": "Directory to index (required for initial indexing)" },
                                    "db": { "type": "string", "description": "Path to store the persisted index metadata [default: .lume-index]" },
                                    "semantic": { "type": "boolean", "description": "Enable dense semantic vector search (requires NUTS token)" },
                                    "ollama_entities": { "type": "boolean", "description": "Enable AI entity extraction via local Gemma on Ollama" },
                                    "ollama_model": { "type": "string", "description": "Local Ollama model to use for entity extraction" },
                                    "ollama_url": { "type": "string", "description": "Ollama API endpoint" },
                                    "force": { "type": "boolean", "description": "Force re-indexing of all files" },
                                    "tag_dict": { "type": "string", "description": "Path to FST phrase dictionary CSV" }
                                }
                            }
                        },
                        {
                            "name": "lume_search",
                            "description": "Query the persisted index using lexical, semantic, or hybrid search.",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "query": { "type": "string", "description": "Search query string. Supports term exclusions (-term, NOT term) and metadata field filters (e.g. field:value, -field:value, field:>=2020, field:2000..2020, field:a,b)" },
                                    "db": { "type": "string", "description": "Path to the persisted index metadata [default: .lume-index]" },
                                    "spell_check": { "type": "boolean", "description": "Enable spelling correction on search query" },
                                    "limit": { "type": "integer", "description": "Max number of search hits [default: 10]" },
                                    "alpha": { "type": "number", "description": "Hybrid blending weight: 0.0 (BM25 only) to 1.0 (semantic only) [default: 0.5]" },
                                    "graph": { "type": "number", "description": "SKG entity-graph boost weight; 0 disables [default: 0.4]" },
                                    "facets": {
                                        "type": "array",
                                        "items": { "type": "string" },
                                        "description": "Field or range facet requests (e.g. ['tags', 'year:range(2000,2030,5)'])"
                                    },
                                    "facet_queries": {
                                        "type": "object",
                                        "additionalProperties": { "type": "string" },
                                        "description": "Named query facet requests (e.g. {'cancer': 'cancer'})"
                                    }
                                },
                                "required": ["query"]
                            }
                        },
                        {
                            "name": "lume_generate",
                            "description": "Generate style-faithful text from the indexed corpus.",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "seed_word": { "type": "string", "description": "Seed word or phrase" },
                                    "db": { "type": "string", "description": "Path to the persisted index metadata [default: .lume-index]" },
                                    "limit": { "type": "integer", "description": "Max number of tokens/words to generate [default: 100]" },
                                    "steer": { "type": "array", "items": { "type": "string" }, "description": "Tags to steer the generation" },
                                    "attempts": { "type": "integer", "description": "Number of attempts for steered/inverted generation [default: 6]" },
                                    "threshold": { "type": "number", "description": "Quality threshold for GTR match [default: 0.75]" }
                                }
                            }
                        },
                        {
                            "name": "lume_not_found",
                            "description": "Call this tool if you have executed search queries but the retrieved passages do not contain the answer to the user's question. Explain what you were looking for and what you searched. The system will respond with guidance.",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "reason": { "type": "string", "description": "Why the current retrieved snippets are insufficient or what is missing." },
                                    "attempted_queries": { "type": "array", "items": { "type": "string" }, "description": "The search queries you have already tried." }
                                },
                                "required": ["reason"]
                            }
                        }
                    ]
                }
            });
            #[cfg(feature = "ti")]
            let response = {
                let mut response = response;
                if let Some(tools) = response["result"]["tools"].as_array_mut() {
                    tools.extend(crate::ti_mcp::definitions(_ti.as_ref().and_then(|server| server.mcp_width())));
                    tools.push(crate::sql::definition());
                }
                response
            };
            response
        }
        "tools/call" => {
            let params = match req_val.get("params") {
                Some(p) => p,
                None => {
                    return json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": -32602, "message": "Invalid params" }
                    });
                }
            };
            let name = match params.get("name").and_then(|n| n.as_str()) {
                Some(n) => n,
                None => {
                    return json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": -32602, "message": "Missing parameter 'name'" }
                    });
                }
            };
            let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
            
            #[cfg(feature = "ti")]
            let result = if let Some(server) = _ti.as_ref().filter(|_| matches!(name,"ti_query"|"ti_schema"|"ti_explain"|"ti_status"|"ti_resolve")) {
                server.mcp(name,&arguments)
            } else { execute_tool_by_name(name,arguments,".lume-index") };
            #[cfg(not(feature = "ti"))]
            let result = execute_tool_by_name(name,arguments,".lume-index");
            match result {
                Ok(out) => {
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "content": [
                                {
                                    "type": "text",
                                    "text": out
                                }
                            ]
                        }
                    })
                }
                Err(err) => {
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "content": [
                                {
                                    "type": "text",
                                    "text": format!("Error: {}", err)
                                }
                            ],
                            "isError": true
                        }
                    })
                }
            }
        }
        _ => {
            if id.is_null() {
                json!(null)
            } else {
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32601, "message": format!("Method '{}' not found", method) }
                })
            }
        }
    }
}

const HTTP_IO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const MAX_MCP_BODY_BYTES: usize = 8 * 1024 * 1024;

fn http_error(stream: &mut TcpStream, status: &str) -> std::io::Result<()> {
    stream.write_all(
        format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes(),
    )?;
    linger_close(stream);
    Ok(())
}

/// Early rejects leave the request body unread. Closing with unread bytes makes the OS
/// (always on Windows) send RST, which can discard this response before the client reads
/// it. Half-close, then discard what the client already sent, bounded in bytes and time.
fn linger_close(stream: &mut TcpStream) {
    use std::io::Read;
    let _ = stream.flush();
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
    let mut buf = [0u8; 8192];
    let mut drained = 0usize;
    while drained < 1024 * 1024 {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() || stream.set_read_timeout(Some(left)).is_err() {
            break;
        }
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => drained += n,
        }
    }
}

fn handle_connection(
    stream: TcpStream,
    ti: &TiState,
    auth: Option<&crate::http_auth::HttpBearer>,
) -> std::io::Result<()> {
    handle_connection_with_auth(stream, ti, HTTP_IO_TIMEOUT, auth)
}

#[cfg(test)]
fn handle_connection_with_timeout(
    stream: TcpStream,
    ti: &TiState,
    timeout: std::time::Duration,
) -> std::io::Result<()> {
    handle_connection_with_auth(stream, ti, timeout, None)
}

fn handle_connection_with_auth(
    mut stream: TcpStream,
    _ti: &TiState,
    timeout: std::time::Duration,
    auth: Option<&crate::http_auth::HttpBearer>,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let mut buffer = [0; 8192];
    let mut bytes_read = 0;
    let header_end = loop {
        if bytes_read == buffer.len() {
            return http_error(&mut stream, "400 Bad Request");
        }
        let n = match stream.read(&mut buffer[bytes_read..]) {
            Ok(0) => return http_error(&mut stream, "400 Bad Request"),
            Ok(n) => n,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                return http_error(&mut stream, "400 Bad Request")
            }
            Err(error) => return Err(error),
        };
        bytes_read += n;
        if let Some(end) = find_subsequence(&buffer[..bytes_read], b"\r\n\r\n") {
            break end;
        }
    };
    let req_str = match std::str::from_utf8(&buffer[..header_end]) {
        Ok(headers) => headers,
        Err(_) => return http_error(&mut stream, "400 Bad Request"),
    };
    let mut lines = req_str.lines();
    let req_line = match lines.next() {
        Some(l) => l,
        None => return Ok(()),
    };
    let parts: Vec<&str> = req_line.split_whitespace().collect();
    if parts.len() != 3 || !parts[2].starts_with("HTTP/1.") {
        return http_error(&mut stream, "400 Bad Request");
    }
    let method = parts[0];
    let path = parts[1];

    if method == "GET" && path.split('?').next() == Some("/health")
        && auth.is_none_or(|auth| auth.public_health())
    {
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")?;
        return Ok(());
    }

    if let Some(auth) = auth {
        #[cfg(feature = "ti")]
        let route_token = _ti.as_ref().and_then(|server| server.route_http_token(path));
        #[cfg(not(feature = "ti"))]
        let route_token = None;
        let clean_path = path.split('?').next().unwrap_or(path);
        let write = clean_path.starts_with("/v1/")
            || (clean_path.starts_with("/ti/shards/") && method != "GET" && method != "HEAD");
        if !auth.accepts_scope(req_str, route_token, if write { "write" } else { "read" }) {
            return http_error(&mut stream, "401 Unauthorized");
        }
    }

    #[cfg(feature = "ti")]
    if _ti.as_ref().is_some_and(|server| server.otlp_only())
        && !(method == "POST" && matches!(path.split('?').next(), Some("/v1/metrics" | "/v1/logs")))
    {
        stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")?;
        return Ok(());
    }

    #[cfg(feature = "ti")]
    if path.starts_with("/ti/") || path.starts_with("/v1/") {
        return crate::ti_http::handle_authenticated(
            &mut stream,
            _ti.as_deref(),
            method,
            path,
            req_str,
            &buffer[header_end + 4..bytes_read],
            auth.is_some(),
        );
    }
    #[cfg(feature = "ti")]
    let cors=if _ti.is_some(){""}else{"Access-Control-Allow-Origin: *\r\n"};
    #[cfg(not(feature = "ti"))]
    let cors="Access-Control-Allow-Origin: *\r\n";
    if method == "OPTIONS" {
        let response = format!("HTTP/1.1 200 OK\r\n\
                        {cors}\
                        Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
                        Access-Control-Allow-Headers: *\r\n\
                        Content-Length: 0\r\n\r\n");
        stream.write_all(response.as_bytes())?;
        stream.flush()?;
        return Ok(());
    }

    if method == "GET" && (path == "/sse" || path.starts_with("/sse?")) {
        let response = format!("HTTP/1.1 200 OK\r\n\
                        Content-Type: text/event-stream\r\n\
                        Cache-Control: no-cache\r\n\
                        Connection: keep-alive\r\n\
                        {cors}\r\n\
                        event: endpoint\r\n\
                        data: /message\r\n\r\n");
        stream.write_all(response.as_bytes())?;
        stream.flush()?;

        // Keep SSE connection open
        loop {
            let mut dummy = [0; 1];
            match stream.read(&mut dummy) {
                Ok(0) => break, // Connection closed by client
                Ok(_) => {},    // Keep alive
                Err(_) => break,
            }
        }
        return Ok(());
    }

    if method == "POST" && (path == "/message" || path.starts_with("/message?") || path == "/mcp" || path.starts_with("/mcp?")) {
        let mut length = None;
        for line in req_str.lines().skip(1) {
            let Some((name, value)) = line.split_once(':') else {
                return http_error(&mut stream, "400 Bad Request");
            };
            if name.eq_ignore_ascii_case("transfer-encoding") {
                return http_error(&mut stream, "400 Bad Request");
            }
            if name.eq_ignore_ascii_case("content-length") {
                if length.is_some() {
                    return http_error(&mut stream, "400 Bad Request");
                }
                length = match value.trim().parse::<usize>() {
                    Ok(value) => Some(value),
                    Err(_) => return http_error(&mut stream, "400 Bad Request"),
                };
            }
        }
        let content_length = length.unwrap_or(0);
        if content_length > MAX_MCP_BODY_BYTES {
            return http_error(&mut stream, "413 Payload Too Large");
        }
        let initial = &buffer[header_end + 4..bytes_read];
        let mut body_bytes = initial[..initial.len().min(content_length)].to_vec();
        while body_bytes.len() < content_length {
            let mut chunk = [0; 8192];
            let remaining = (content_length - body_bytes.len()).min(chunk.len());
            match stream.read(&mut chunk[..remaining]) {
                Ok(0) => return http_error(&mut stream, "400 Bad Request"),
                Ok(n) => body_bytes.extend_from_slice(&chunk[..n]),
                Err(error) if matches!(error.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock) =>
                    return http_error(&mut stream, "400 Bad Request"),
                Err(error) => return Err(error),
            }
        }
        let body_str = String::from_utf8_lossy(&body_bytes);

        let rpc_req: serde_json::Value = match serde_json::from_str(&body_str) {
            Ok(val) => val,
            Err(e) => {
                let err_resp = format!(
                    "HTTP/1.1 400 Bad Request\r\n\
                     Content-Type: application/json\r\n\
                     Connection: close\r\n\
                     {cors}\r\n{}",
                    json!({
                        "jsonrpc": "2.0",
                        "error": { "code": -32700, "message": format!("Parse error: {}", e) },
                        "id": null
                    })
                );
                stream.write_all(err_resp.as_bytes())?;
                stream.flush()?;
                return Ok(());
            }
        };

        // Standalone TI MCP calls must not reintroduce browser access to TI data.
        #[cfg(feature = "ti")]
        let cors=if rpc_req["params"]["name"].as_str().is_some_and(|name|matches!(name,"ti_query"|"ti_schema"|"ti_explain"|"ti_status"|"ti_resolve")){""}else{cors};
        // MCP transport needs read; mutation tools additionally need write.
        // Check after bounded parsing, before any tool dispatch.
        if rpc_req["method"].as_str() == Some("tools/call")
            && rpc_req["params"]["name"].as_str() == Some("lume_index")
            && auth.is_some_and(|auth| !auth.accepts_scope(req_str, None, "write"))
        {
            return http_error(&mut stream, "401 Unauthorized");
        }
        let response_json = handle_mcp_request(rpc_req, _ti);
        if response_json.is_null() {
            let response = format!("HTTP/1.1 204 No Content\r\n\
                            Connection: close\r\n\
                            Content-Length: 0\r\n\
                            {cors}\r\n");
            stream.write_all(response.as_bytes())?;
        } else {
            let resp_str = serde_json::to_string(&response_json).unwrap_or_default();
            let response = format!(
                "HTTP/1.1 200 OK\r\n\
                 Content-Type: application/json\r\n\
                 Connection: close\r\n\
                 {cors}\
                 Access-Control-Allow-Headers: *\r\n\
                 Access-Control-Allow-Methods: *\r\n\
                 Content-Length: {}\r\n\r\n{}",
                resp_str.len(),
                resp_str
            );
            stream.write_all(response.as_bytes())?;
        }
        stream.flush()?;
        return Ok(());
    }

    // Default 404 response for other paths
    let not_found = format!("HTTP/1.1 404 Not Found\r\n\
                     {cors}\
                     Content-Length: 0\r\n\r\n");
    stream.write_all(not_found.as_bytes())?;
    stream.flush()?;
    Ok(())
}

/// Cap on simultaneously-handled connections; excess requests get an
/// immediate 503 instead of an unbounded thread spawn.
const MAX_CONCURRENT_CONNECTIONS: usize = 64;

pub fn serve(port: u16) -> Result<(), String> {
    serve_on(port,"127.0.0.1")
}
pub fn serve_on(port: u16, bind: &str) -> Result<(), String> {
    serve_on_with_http_auth(port, bind, None)
}
pub fn serve_on_with_http_auth(
    port: u16,
    bind: &str,
    auth: Option<crate::http_auth::HttpBearer>,
) -> Result<(), String> {
    let bind = bind
        .parse::<std::net::IpAddr>()
        .map_err(|e| format!("Invalid bind address: {e}"))?;
    #[cfg(feature = "ti")]
    let ti = None;
    #[cfg(not(feature = "ti"))]
    let ti = ();
    serve_configured(port, ti, bind, auth)
}
#[cfg(feature = "ti")]
pub fn serve_with_ti(port:u16,root:&std::path::Path)->Result<(),String>{
    serve_with_ti_on(port,root,"127.0.0.1")
}
#[cfg(feature = "ti")]
pub fn serve_with_ti_on(port:u16,root:&std::path::Path,bind:&str)->Result<(),String>{
    serve_with_ti_pg_on(port,root,bind,None)
}
#[cfg(feature = "ti")]
pub fn serve_with_ti_pg_on(port:u16,root:&std::path::Path,bind:&str,pg:Option<u16>)->Result<(),String>{
    serve_with_ti_pg_config(port, root, bind, pg, None, None)
}
#[cfg(feature = "ti")]
pub fn serve_with_ti_pg_config(
    port: u16, root: &std::path::Path, bind: &str, pg: Option<u16>,
    pg_bind: Option<&str>, pg_auth_config: Option<&std::path::Path>,
) -> Result<(), String> {
    serve_with_ti_pg_docs_config(port, root, bind, pg, pg_bind, pg_auth_config, None)
}
#[cfg(feature = "ti")]
#[allow(clippy::too_many_arguments)]
pub fn serve_with_ti_pg_tls_config(
    port: u16,
    root: &std::path::Path,
    bind: &str,
    pg: Option<u16>,
    pg_bind: Option<&str>,
    pg_auth_config: Option<&std::path::Path>,
    docs_index: Option<&std::path::Path>,
    pg_options: &crate::ti_pg::PgOptions,
) -> Result<(), String> {
    serve_with_ti_pg_tls_http_config(
        port,
        root,
        bind,
        pg,
        pg_bind,
        pg_auth_config,
        docs_index,
        pg_options,
        None,
    )
}
#[cfg(feature = "ti")]
#[allow(clippy::too_many_arguments)]
pub fn serve_with_ti_pg_tls_http_config(
    port: u16,
    root: &std::path::Path,
    bind: &str,
    pg: Option<u16>,
    pg_bind: Option<&str>,
    pg_auth_config: Option<&std::path::Path>,
    docs_index: Option<&std::path::Path>,
    pg_options: &crate::ti_pg::PgOptions,
    auth: Option<crate::http_auth::HttpBearer>,
) -> Result<(), String> {
    let mut ti = crate::ti_http::TiServer::open(root)?;
    if let Some(path) = docs_index {
        ti = ti.with_docs_index(path)?;
    }
    if let Some(path) = pg_auth_config {
        ti = ti.with_pg_auth_config(path)?;
    }
    serve_with_ti_server_pg_http_options(
        port,
        std::sync::Arc::new(ti),
        bind,
        pg,
        pg_bind,
        pg_options,
        auth,
    )
}
#[cfg(feature = "ti")]
pub fn serve_with_ti_pg_docs_config(
    port: u16, root: &std::path::Path, bind: &str, pg: Option<u16>,
    pg_bind: Option<&str>, pg_auth_config: Option<&std::path::Path>,
    docs_index: Option<&std::path::Path>,
) -> Result<(), String> {
    serve_with_ti_pg_tls_config(port, root, bind, pg, pg_bind, pg_auth_config, docs_index, &crate::ti_pg::PgOptions::default())
}
#[cfg(feature = "ti")]
pub fn serve_with_ti_server(
    port: u16,
    ti: std::sync::Arc<crate::ti_http::TiServer>,
    bind: &str,
    pg: Option<u16>,
) -> Result<(), String> {
    serve_with_ti_server_pg_bind(port, ti, bind, pg, None)
}
#[cfg(feature = "ti")]
pub fn serve_with_ti_server_pg_bind(
    port: u16,
    ti: std::sync::Arc<crate::ti_http::TiServer>,
    bind: &str,
    pg: Option<u16>,
    pg_bind: Option<&str>,
) -> Result<(), String> {
    serve_with_ti_server_pg_options(port, ti, bind, pg, pg_bind, &crate::ti_pg::PgOptions::default())
}
#[cfg(feature = "ti")]
pub fn serve_with_ti_server_pg_options(
    port: u16,
    ti: std::sync::Arc<crate::ti_http::TiServer>,
    bind: &str,
    pg: Option<u16>,
    pg_bind: Option<&str>,
    pg_options: &crate::ti_pg::PgOptions,
) -> Result<(), String> {
    serve_with_ti_server_pg_http_options(port, ti, bind, pg, pg_bind, pg_options, None)
}
#[cfg(feature = "ti")]
pub fn serve_with_ti_server_pg_http_options(
    port: u16,
    ti: std::sync::Arc<crate::ti_http::TiServer>,
    bind: &str,
    pg: Option<u16>,
    pg_bind: Option<&str>,
    pg_options: &crate::ti_pg::PgOptions,
    auth: Option<crate::http_auth::HttpBearer>,
) -> Result<(), String> {
    let http_bind = bind
        .parse::<std::net::IpAddr>()
        .map_err(|e| format!("Invalid bind address: {e}"))?;
    let pg_bind = pg_bind
        .unwrap_or(bind)
        .parse::<std::net::IpAddr>()
        .map_err(|e| format!("Invalid pg bind address: {e}"))?;
    crate::http_auth::validate_bind(bind, auth.is_some()
        || (ti.otlp_only() && ti.route_http_token("/v1/logs").is_some()))?;
    let _pg = pg
        .map(|port| {
            crate::ti_pg::start_with_options(
                ti.clone(),
                std::net::SocketAddr::new(pg_bind, port),
                pg_options,
            )
        })
        .transpose()?;
    serve_configured(port, Some(ti), http_bind, auth)
}
struct ActiveConnection(std::sync::Arc<std::sync::atomic::AtomicUsize>);

impl Drop for ActiveConnection {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

fn serve_configured(
    port: u16,
    _ti: TiState,
    bind: std::net::IpAddr,
    auth: Option<crate::http_auth::HttpBearer>,
) -> Result<(), String> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    #[cfg(feature = "ti")]
    let otlp_only = _ti.as_ref().is_some_and(|server| server.otlp_only());
    #[cfg(feature = "ti")]
    let route_authenticated = otlp_only
        && _ti.as_ref().is_some_and(|server| server.route_http_token("/v1/logs").is_some());
    #[cfg(not(feature = "ti"))]
    let route_authenticated = false;
    crate::http_auth::validate_bind(&bind.to_string(), auth.is_some() || route_authenticated)?;
    let address = std::net::SocketAddr::new(bind, port);
    let listener =
        TcpListener::bind(address).map_err(|e| format!("Failed to bind to {address}: {e}"))?;
    println!(
        "Lume MCP HTTP server listening on http://{}",
        listener.local_addr().map_err(|e| e.to_string())?
    );

    let active = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                stream
                    .set_read_timeout(Some(HTTP_IO_TIMEOUT))
                    .map_err(|e| e.to_string())?;
                stream
                    .set_write_timeout(Some(HTTP_IO_TIMEOUT))
                    .map_err(|e| e.to_string())?;
                if active.load(Ordering::Acquire) >= MAX_CONCURRENT_CONNECTIONS {
                    let busy = "HTTP/1.1 503 Service Unavailable\r\n\
                                Retry-After: 1\r\n\
                                Content-Length: 0\r\n\r\n";
                    let _ = stream.write_all(busy.as_bytes());
                    continue;
                }
                active.fetch_add(1, Ordering::AcqRel);
                let slot = ActiveConnection(Arc::clone(&active));
                #[cfg(feature = "ti")]
                let ti = _ti.clone();
                #[cfg(not(feature = "ti"))]
                let ti = ();
                let auth = auth.clone();
                std::thread::spawn(move || {
                    let _slot = slot;
                    if let Err(e) = handle_connection(stream, &ti, auth.as_ref()) {
                        eprintln!("Error handling connection: {}", e);
                    }
                });
            }
            Err(e) => {
                eprintln!("Failed to accept incoming connection: {}", e);
            }
        }
    }
    Ok(())
}

// --- Autonomous Tool-Calling Agent Loop ---

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
    arguments: serde_json::Value,
}

#[derive(Serialize)]
struct AgentChatPayload {
    model: String,
    messages: Vec<AgentMessage>,
    tools: Vec<Tool>,
    stream: bool,
    options: Options,
}

/// Inputs for [`run_agent_loop`]. One struct keeps the CLI entry under clippy's argument limit.
pub struct AgentLoopArgs<'a> {
    pub question: &'a str,
    pub ollama_url: &'a str,
    pub ollama_model: &'a str,
    pub db_dir: &'a str,
    pub verbose: bool,
    pub ti_store: Option<&'a str>,
    pub docs_index: Option<&'a str>,
    pub json_output: bool,
    pub events: bool,
}

pub fn run_agent_loop(args: AgentLoopArgs<'_>) -> Result<(), String> {
    let AgentLoopArgs {
        question,
        ollama_url,
        ollama_model,
        db_dir,
        verbose,
        #[cfg(feature = "ti")]
        ti_store,
        #[cfg(feature = "ti")]
        docs_index,
        #[cfg(feature = "ti")]
        json_output,
        #[cfg(feature = "ti")]
        events,
        ..
    } = args;
    #[cfg(feature = "ti")]
    if ti_store.is_some() || docs_index.is_some() || json_output || events {
        return crate::chat_sql::run_chat_loop(
            question,
            ollama_url,
            ollama_model,
            db_dir,
            verbose,
            ti_store,
            docs_index,
            json_output,
            events,
        );
    }
    let url = format!("{}/api/chat", resolve_ollama_url(ollama_url));

    let mut messages = vec![
        AgentMessage {
            role: "system".to_string(),
            content: format!("You are an expert Q&A agent. Your goal is to answer the user's question using the Lume search tool. \
CRITICAL RULES: \
1. DO NOT HALLUCINATE OR GUESS. Every fact in your answer must be directly supported by the retrieved search snippets. If the snippets do not contain the exact answer, do not invent one or try to stretch irrelevant snippets to fit. \
2. VERIFY SEMANTICS. Check if the retrieved text actually addresses the question. For example, if asked what Danglars says Dantès fancies himself to be, verify if the snippet shows Danglars talking about Dantès. \
3. TRY MULTIPLE SEARCHES. If your first search query doesn't yield snippets containing the direct answer, you MUST try alternative search queries. Try: \
   - Specific keywords/phrases from the question (e.g., exact match quotes like \"fancies himself\" or \"fancy himself\"). \
   - Synonyms, nouns, or specific verbs. \
   - Broadening/narrowing the query (e.g. searching for just \"fancies himself\"). \
4. ALWAYS SEARCH FIRST. You do not have any pre-existing knowledge of the document. You MUST begin the conversation by calling the lume_search tool with a query based on the user's question. Do not attempt to answer or say you do not have the information before performing at least one search. \
5. If you have searched multiple times with different queries and still cannot find the answer, call the lume_not_found tool to report the failure. Do not write a plain text response stating you cannot find the answer before doing this. \
6. The target search index is located at: '{}'. If you call lume_search, you should query this index. \
Keep your final answer concise and factual.", db_dir),
            tool_calls: None,
        },
        AgentMessage {
            role: "user".to_string(),
            content: question.to_string(),
            tool_calls: None,
        },
    ];

    let tools = vec![
        Tool {
            tool_type: "function".to_string(),
            function: Function {
                name: "lume_index".to_string(),
                description: "Index a directory of text, code, and PDF files. Supports incremental updates. Automatically updates index if target db already has state.json.".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "dir": { "type": "string", "description": "Directory to index (required for initial indexing)" },
                        "db": { "type": "string", "description": "Path to store the persisted index metadata [default: .lume-index]" },
                        "semantic": { "type": "boolean", "description": "Enable dense semantic vector search (requires NUTS token)" },
                        "ollama_entities": { "type": "boolean", "description": "Enable AI entity extraction via local Gemma on Ollama" },
                        "ollama_model": { "type": "string", "description": "Local Ollama model to use for entity extraction" },
                        "ollama_url": { "type": "string", "description": "Ollama API endpoint" },
                        "shivvr_url": { "type": "string", "description": "Shivvr API endpoint URL [default: http://localhost:8085]" },
                        "force": { "type": "boolean", "description": "Force re-indexing of all files" },
                        "tag_dict": { "type": "string", "description": "Path to FST phrase dictionary CSV" }
                    }
                }),
            },
        },
        Tool {
            tool_type: "function".to_string(),
            function: Function {
                name: "lume_search".to_string(),
                description: "Query the persisted index using lexical, semantic, or hybrid search.".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Search query string" },
                        "db": { "type": "string", "description": "Path to the persisted index metadata [default: .lume-index]" },
                        "spell_check": { "type": "boolean", "description": "Enable spelling correction on search query" },
                        "limit": { "type": "integer", "description": "Max number of search hits [default: 10]" },
                        "alpha": { "type": "number", "description": "Hybrid blending weight: 0.0 (BM25 only) to 1.0 (semantic only) [default: 0.5]" },
                        "graph": { "type": "number", "description": "SKG entity-graph boost weight; 0 disables [default: 0.4]" },
                        "shivvr_url": { "type": "string", "description": "Shivvr API endpoint URL [default: http://localhost:8085]" }
                    },
                    "required": ["query"]
                }),
            },
        },
        Tool {
            tool_type: "function".to_string(),
            function: Function {
                name: "lume_generate".to_string(),
                description: "Generate style-faithful text from the indexed corpus.".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "seed_word": { "type": "string", "description": "Seed word or phrase" },
                        "db": { "type": "string", "description": "Path to the persisted index metadata [default: .lume-index]" },
                        "limit": { "type": "integer", "description": "Max number of tokens/words to generate [default: 100]" },
                        "steer": { "type": "array", "items": { "type": "string" }, "description": "Tags to steer the generation" },
                        "attempts": { "type": "integer", "description": "Number of attempts for steered/inverted generation [default: 6]" },
                        "threshold": { "type": "number", "description": "Quality threshold for GTR match [default: 0.75]" },
                        "shivvr_url": { "type": "string", "description": "Shivvr API endpoint URL [default: http://localhost:8085]" }
                      }
                }),
            },
        },
        Tool {
            tool_type: "function".to_string(),
            function: Function {
                name: "lume_not_found".to_string(),
                description: "Call this tool if you have executed search queries but the retrieved passages do not contain the answer to the user's question. Explain what you were looking for and what you searched. The system will respond with guidance on what queries or terms to try next. Only output a final answer saying the text doesn't contain the information after calling this tool and receiving its guidance.".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "reason": { "type": "string", "description": "Why the current retrieved snippets are insufficient or what is missing." },
                        "attempted_queries": { "type": "array", "items": { "type": "string" }, "description": "The search queries you have already tried." }
                    },
                    "required": ["reason"]
                }),
            },
        },
    ];

    println!("[Agent] Starting task: {}", question);

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

        let response = ureq::post(&url)
            .set("Content-Type", "application/json")
            .timeout(std::time::Duration::from_secs(300))
            .send_json(&payload)
            .map_err(|e| format!("Ollama API request failed: {}", e))?;

        if response.status() != 200 {
            let status = response.status();
            let err_body = response.into_string().unwrap_or_else(|_| "Unknown error".to_string());
            return Err(format!("Ollama returned HTTP status {}: {}", status, err_body));
        }

        #[derive(Deserialize, Debug)]
        struct AgentChatResponse {
            message: AgentMessage,
        }

        let chat_res: AgentChatResponse = response
            .into_json()
            .map_err(|e| format!("Failed to parse Ollama response JSON: {}", e))?;

        let assistant_msg = chat_res.message;
        
        if verbose {
            println!("[Agent] Turn {}: Model returned message:\n{}", turn, serde_json::to_string_pretty(&assistant_msg).unwrap_or_default());
        } else {
            let tool_desc = if let Some(ref tc) = assistant_msg.tool_calls {
                let names: Vec<String> = tc.iter().map(|c| c.function.name.clone()).collect();
                format!("{:?}", names)
            } else {
                "None".to_string()
            };
            println!("[Agent] Turn {}: Model returned tool_calls={}", turn, tool_desc);
        }

        // Keep track of this assistant message in the conversation history
        messages.push(assistant_msg.clone());

        if let Some(ref tool_calls) = assistant_msg.tool_calls {
            if tool_calls.is_empty() {
                // No tool calls but text content was returned
                if !assistant_msg.content.trim().is_empty() {
                    println!("\n[Agent Final Answer]\n{}", assistant_msg.content);
                    return Ok(());
                }
            } else {
                for call in tool_calls {
                    let tool_name = &call.function.name;
                    let tool_args = &call.function.arguments;
                    println!("[Agent] Executing tool '{}' with arguments: {}", tool_name, tool_args);
                    
                    let result_str = match execute_tool_by_name(tool_name, tool_args.clone(), db_dir) {
                        Ok(output) => output,
                        Err(err) => format!("Error executing tool: {}", err),
                    };

                    if verbose {
                        println!("[Agent] Tool returned output:\n{}", result_str);
                    } else {
                        println!("[Agent] Tool returned output (length: {})", result_str.len());
                    }

                    messages.push(AgentMessage {
                        role: "tool".to_string(),
                        content: result_str,
                        tool_calls: None,
                    });
                }
            }
        } else {
            // No tool calls, just text
            if !assistant_msg.content.trim().is_empty() {
                println!("\n[Agent Final Answer]\n{}", assistant_msg.content);
                return Ok(());
            }
        }
    }

    Err(format!("Agent exceeded maximum turns ({}) without finding a final answer.", max_turns))
}

pub fn summarize_document(
    db_dir: &str,
    ollama_url: &str,
    ollama_model: &str,
    target_file: Option<&str>,
    num_queries: usize,
    hits_per_query: usize,
    verbose: bool,
) -> Result<(), String> {
    let root = std::path::Path::new(db_dir);
    let typed_state = if crate::index_binary::snapshot::present(root) {
        crate::index_binary::snapshot::restore_state(root)?
    } else {
        crate::index_binary::snapshot::settings(root)?
    };
    let state = serde_json::to_value(typed_state).map_err(|e| e.to_string())?;

    let cached_files = state.get("cached_files")
        .and_then(|v| v.as_object())
        .ok_or("No cached files found in state.json")?;

    let selected_file = match target_file {
        Some(f) => {
            if !cached_files.contains_key(f) {
                return Err(format!("File '{}' not found in Lume index cached files.", f));
            }
            f.to_string()
        }
        None => {
            let mut best_file = String::new();
            let mut max_sections = 0;
            for (fname, val) in cached_files {
                if let Some(arr) = val.as_array() {
                    if arr.len() >= 2 {
                        if let Some(sections) = arr[1].as_array() {
                            if sections.len() > max_sections {
                                max_sections = sections.len();
                                best_file = fname.clone();
                            }
                        }
                    }
                }
            }
            if best_file.is_empty() {
                cached_files.keys().next().ok_or("No cached files in index")?.clone()
            } else {
                best_file
            }
        }
    };

    println!("[🧠] Target Document: {}", selected_file);
    let semantic_enabled = state.get("semantic_enabled").and_then(|v| v.as_bool()).unwrap_or(false);

    // Read the entity graph to find key concepts/entities
    let mut top_entities = Vec::new();
    let graph_root = if crate::index_binary::snapshot::present(root) {
        crate::index_binary::generation::generation_directory(
            root,
            &crate::index_binary::generation::read_manifest(root)?,
        )?
    } else {
        root.to_path_buf()
    };
    let graph_path = graph_root.join("entity_graph.json");
    if graph_path.exists() {
        if let Ok(graph_content) = std::fs::read_to_string(&graph_path) {
            if let Ok(graph_val) = serde_json::from_str::<serde_json::Value>(&graph_content) {
                if let Some(nodes) = graph_val.get("nodes").and_then(|n| n.as_array()) {
                    let mut sorted_nodes = nodes.clone();
                    // Sort by frequency descending
                    sorted_nodes.sort_by(|a, b| {
                        let freq_a = a.get("frequency").and_then(|v| v.as_u64()).unwrap_or(0);
                        let freq_b = b.get("frequency").and_then(|v| v.as_u64()).unwrap_or(0);
                        freq_b.cmp(&freq_a)
                    });
                    
                    for node in sorted_nodes.iter().take(12) {
                        if let Some(label) = node.get("label").and_then(|v| v.as_str()) {
                            top_entities.push(label.to_string());
                        }
                    }
                }
            }
        }
    }

    if !top_entities.is_empty() {
        println!("[🧠] Central entities identified in Knowledge Graph: {}", top_entities.join(", "));
    }

    let resolved_url = resolve_ollama_url(ollama_url);

    println!("[🧠] Ollama Endpoint: {}", resolved_url);
    println!("[🧠] Ollama Model: {}", ollama_model);

    // 1. Generate Search Plan
    println!("[🧠] Planning search queries to explore the document...");
    let filename = std::path::Path::new(&selected_file)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(&selected_file);

    let graph_guide = if !top_entities.is_empty() {
        format!(
            "\nThe Knowledge Graph of the document identifies the following central entities and key concepts as highly important:\n\
            {}\n\
            Make sure your planned search queries specifically target these entities/concepts to extract the most relevant passages.",
            top_entities.join(", ")
        )
    } else {
        "".to_string()
    };

    let prompt = format!(
        "You are an agentic search planner. Your task is to generate exactly {} distinct search queries to discover the structure, main themes, key arguments, and conclusions of the document named '{}'.\n\n\
        Rules:\n\
        1. Each query should focus on a different aspect of the document (e.g., table of contents/preface, core thesis/introduction, main theoretical chapters, final summary/conclusions).\n\
        2. The queries should be designed to return the most informative passage hits when run against a search engine.{}\n\
        3. The response MUST be a valid JSON array of strings:\n\
        [\n\
          \"query 1\",\n\
          \"query 2\",\n\
          ...\n\
        ]\n\
        Do not return any conversational text, only the JSON array.",
        num_queries, filename, graph_guide
    );

    let payload = serde_json::json!({
        "model": ollama_model,
        "messages": [
            {
                "role": "system",
                "content": "You are a search query planner outputting strictly structured JSON. You must return only a JSON array of strings."
            },
            {
                "role": "user",
                "content": prompt
            }
        ],
        "format": "json",
        "stream": false,
        "options": {
            "temperature": 0.2,
            "num_ctx": 4096
        }
    });

    let url = format!("{}/api/chat", resolved_url.trim_end_matches('/'));
    let response = ureq::post(&url)
        .set("Content-Type", "application/json")
        .timeout(std::time::Duration::from_secs(60))
        .send_json(&payload)
        .map_err(|e| format!("Failed to call Ollama planner: {}", e))?;

    let res_val: serde_json::Value = response.into_json()
        .map_err(|e| format!("Failed to parse planner JSON response: {}", e))?;
    let content = res_val["message"]["content"].as_str().ok_or("No message content in planner response")?.trim();

    let clean_content = extract_json_block(content);
    let queries: Vec<String> = serde_json::from_str(&clean_content)
        .map_err(|e| format!("Failed to parse query plan JSON: {}. Raw content was:\n{}", e, content))?;

    for (idx, q) in queries.iter().enumerate() {
        println!("  Query {}: \"{}\"", idx + 1, q);
    }

    // 2. Execute searches and gather unique contexts
    println!("\n[🔍] Executing searches against the Lume index...");
    let mut unique_snippets = std::collections::HashSet::new();

    for q in &queries {
        let mut cli_args = vec![
            "search".to_string(),
            "--db".to_string(),
            db_dir.to_string(),
            "-l".to_string(),
            hits_per_query.to_string(),
        ];
        if semantic_enabled {
            cli_args.push("-a".to_string());
            cli_args.push("0.5".to_string());
        }
        cli_args.push(q.clone());

        let output = run_lume_cli(cli_args)?;
        
        let mut current_snippet = Vec::new();
        let mut collecting = false;
        for line in output.lines() {
            if line.starts_with('[') && line.contains("Score:") {
                if collecting && !current_snippet.is_empty() {
                    unique_snippets.insert(current_snippet.join("\n").trim().to_string());
                    current_snippet.clear();
                }
                collecting = true;
            } else if collecting {
                current_snippet.push(line);
            }
        }
        if collecting && !current_snippet.is_empty() {
            unique_snippets.insert(current_snippet.join("\n").trim().to_string());
        }
        
        if verbose {
            println!("  Ran query: \"{}\" (Retrieved snippets)", q);
        }
    }

    println!("\n[📊] Gathered {} unique passage snippets.", unique_snippets.len());

    if unique_snippets.is_empty() {
        println!("\n# Executive Summary: {}\n", filename);
        println!("I do not have enough information / retrieved passages to summarize this document.");
        return Ok(());
    }

    // 3. Synthesize summary
    println!("[🧠] Synthesizing comprehensive summary...");
    let context_text = unique_snippets.into_iter().collect::<Vec<String>>().join("\n\n---\n\n");

    let synth_prompt = format!(
        "You are a senior document analyst. Below is a collection of retrieved text passages from the document '{}'.\n\
        Use these passages to synthesize a comprehensive, high-quality, structured summary of the entire document.\n\n\
        Retrieved Passages:\n\
        \"\"\"\n\
        {}\n\
        \"\"\"\n\n\
        Your summary should include:\n\
        1. **Document Overview**: A high-level description of what the document is about.\n\
        2. **Key Themes and Arguments**: Detailed bullet points explaining the core concepts, theories, or topics discussed.\n\
        3. **Structure & Organization**: An outline of how the document is structured (if a table of contents or chapter names were retrieved).\n\
        4. **Conclusions**: The main takeaways or final thoughts of the document.\n\n\
        Write a professional, detailed, and cohesive summary. Do not refer to the fact that you read 'snippets' or 'passages'; write the summary as if you have read the complete document.",
        filename, context_text
    );

    let payload = serde_json::json!({
        "model": ollama_model,
        "messages": [
            {
                "role": "system",
                "content": "You are a professional summarization assistant. You must write a cohesive, comprehensive summary based only on the provided context."
            },
            {
                "role": "user",
                "content": synth_prompt
            }
        ],
        "stream": false,
        "options": {
            "temperature": 0.3,
            "num_ctx": 16384
        }
    });

    let response = ureq::post(&url)
        .set("Content-Type", "application/json")
        .timeout(std::time::Duration::from_secs(240))
        .send_json(&payload)
        .map_err(|e| format!("Failed to call Ollama synthesizer: {}", e))?;

    let res_val: serde_json::Value = response.into_json()
        .map_err(|e| format!("Failed to parse synthesizer JSON response: {}", e))?;
    let summary = res_val["message"]["content"].as_str().ok_or("No message content in synthesizer response")?.trim();

    println!("\n# Executive Summary: {}\n", filename);
    println!("{}", summary);

    Ok(())
}

fn extract_json_block(text: &str) -> String {
    let text = text.trim();
    let first_bracket = text.find('[');
    let last_bracket = text.rfind(']');
    if let (Some(fb), Some(lb)) = (first_bracket, last_bracket) {
        if lb > fb {
            return text[fb..=lb].to_string();
        }
    }
    let first_brace = text.find('{');
    let last_brace = text.rfind('}');
    if let (Some(fb), Some(lb)) = (first_brace, last_brace) {
        if lb > fb {
            return text[fb..=lb].to_string();
        }
    }
    text.to_string()
}



#[cfg(test)]
mod http_limits_tests {
    use super::*;
    use std::net::Shutdown;
    use std::time::{Duration, Instant};

    fn request(bytes: &[u8], close_write: bool) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handler = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            #[cfg(feature = "ti")]
            let ti: TiState = None;
            #[cfg(not(feature = "ti"))]
            let ti: TiState = ();
            handle_connection_with_timeout(stream, &ti, Duration::from_millis(100)).unwrap();
        });
        let mut client = TcpStream::connect(addr).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        client.write_all(bytes).unwrap();
        if close_write {
            client.shutdown(Shutdown::Write).unwrap();
        }
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        handler.join().unwrap();
        response
    }

    #[test]
    fn incomplete_or_invalid_headers_are_bad_requests() {
        assert!(
            request(b"POST /mcp HTTP/1.1\r\nContent-Length: 2\r\n", true)
                .starts_with("HTTP/1.1 400")
        );
        let full = format!("POST /mcp HTTP/1.1\r\nX: {}", "x".repeat(8192 - 23));
        assert!(request(full.as_bytes(), true).starts_with("HTTP/1.1 400"));
        assert!(request(b"POST /mcp HTTP/1.1\r\nX: \xff\r\n\r\n", true).starts_with("HTTP/1.1 400"));
    }

    #[test]
    fn mcp_rejects_truncated_oversize_or_ambiguous_bodies() {
        for body in [
            "Content-Length: 3\r\n\r\n{}",
            "Content-Length: 8388609\r\n\r\n",
            "Content-Length: 2\r\nContent-Length: 2\r\n\r\n{}",
            "Content-Length: invalid\r\n\r\n",
            "Transfer-Encoding: chunked\r\n\r\n0\r\n\r\n",
        ] {
            let expected = if body.contains("8388609") {
                "HTTP/1.1 413"
            } else {
                "HTTP/1.1 400"
            };
            assert!(
                request(format!("POST /mcp HTTP/1.1\r\n{body}").as_bytes(), true)
                    .starts_with(expected)
            );
        }
    }

    #[test]
    fn stalled_headers_and_bodies_time_out() {
        assert_eq!(HTTP_IO_TIMEOUT, Duration::from_secs(30));
        let start = Instant::now();
        assert!(request(b"POST /mcp HTTP/1.1\r\n", false).starts_with("HTTP/1.1 400"));
        assert!(
            request(b"POST /mcp HTTP/1.1\r\nContent-Length: 2\r\n\r\n", false)
                .starts_with("HTTP/1.1 400")
        );
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn panicking_handler_releases_admission_slot() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        let active = Arc::new(AtomicUsize::new(1));
        let slot = ActiveConnection(Arc::clone(&active));
        let handler = std::thread::spawn(move || {
            let _slot = slot;
            panic!("injected handler panic");
        });
        assert!(handler.join().is_err());
        assert_eq!(active.load(Ordering::Acquire), 0);
    }

    #[test]
    fn non_loopback_requires_global_auth_before_binding() {
        assert!(crate::http_auth::validate_bind("192.0.2.1", false).is_err());
        assert!(crate::http_auth::validate_bind("192.0.2.1", true).is_ok());
        assert!(crate::http_auth::validate_bind("127.0.0.1", false).is_ok());
        assert!(crate::http_auth::validate_bind("::1", false).is_ok());
    }

    #[test]
    fn mcp_notification_and_parse_error_advertise_close() {
        for (body, status) in [
            (r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#, "204"),
            ("invalid-json", "400"),
        ] {
            let response = request(
                format!("POST /mcp HTTP/1.1\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes(),
                true,
            );
            assert!(response.starts_with(&format!("HTTP/1.1 {status}")));
            assert!(response.contains("\r\nConnection: close\r\n"));
        }
    }

    #[test]
    fn mcp_lume_search_schema_includes_facets() {
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
        let response = request(
            format!(
                "POST /mcp HTTP/1.1\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
            true,
        );
        assert!(response.starts_with("HTTP/1.1 200"));
        assert!(response.contains(r#""facets""#));
        assert!(response.contains(r#""facet_queries""#));
    }

    #[test]
    fn valid_mcp_still_works() {
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
        let response = request(
            format!(
                "POST /mcp HTTP/1.1\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
            true,
        );
        assert!(response.starts_with("HTTP/1.1 200"));
        assert!(response.contains("tools"));
        assert!(response.contains("\r\nConnection: close\r\n"));
    }
}
