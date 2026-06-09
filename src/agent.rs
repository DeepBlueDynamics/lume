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

/// Calls local/remote Ollama chat endpoint to extract key concepts,
/// proper names, organizations, locations, and terms from a text chunk.
pub fn extract_entities(
    text: &str,
    ollama_url: &str,
    ollama_model: &str,
) -> Result<Vec<String>, String> {
    let url = format!("{}/api/chat", ollama_url.trim_end_matches('/'));

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
            let is_update = state_json_path.exists() && !force;

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
            let limit = args.get("limit").and_then(|v| v.as_i64());
            let alpha = args.get("alpha").and_then(|v| v.as_f64());
            let graph = args.get("graph").and_then(|v| v.as_f64());
            let shivvr_url = args.get("shivvr_url").and_then(|v| v.as_str());

            let mut cli_args = vec!["search".to_string()];
            if spell_check {
                cli_args.push("-c".to_string());
            }
            cli_args.push("--db".to_string());
            cli_args.push(db.to_string());

            if let Some(lim) = limit {
                cli_args.push("-l".to_string());
                cli_args.push(lim.to_string());
            }
            if let Some(alp) = alpha {
                cli_args.push("-a".to_string());
                cli_args.push(alp.to_string());
            }
            if let Some(g) = graph {
                cli_args.push("-g".to_string());
                cli_args.push(g.to_string());
            }
            if let Some(s) = shivvr_url {
                cli_args.push("--shivvr-url".to_string());
                cli_args.push(s.to_string());
            }
            cli_args.push(query.to_string());
            run_lume_cli(cli_args)
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
        _ => Err(format!("Unknown tool: {}", name))
    }
}

fn handle_mcp_request(req_val: serde_json::Value) -> serde_json::Value {
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
            json!({
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
                                    "query": { "type": "string", "description": "Search query string" },
                                    "db": { "type": "string", "description": "Path to the persisted index metadata [default: .lume-index]" },
                                    "spell_check": { "type": "boolean", "description": "Enable spelling correction on search query" },
                                    "limit": { "type": "integer", "description": "Max number of search hits [default: 10]" },
                                    "alpha": { "type": "number", "description": "Hybrid blending weight: 0.0 (BM25 only) to 1.0 (semantic only) [default: 0.5]" },
                                    "graph": { "type": "number", "description": "SKG entity-graph boost weight; 0 disables [default: 0.4]" }
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
                        }
                    ]
                }
            })
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
            
            match execute_tool_by_name(name, arguments, ".lume-index") {
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

fn handle_connection(mut stream: TcpStream) -> std::io::Result<()> {
    let mut buffer = [0; 8192];
    let mut bytes_read = 0;
    loop {
        let n = stream.read(&mut buffer[bytes_read..])?;
        if n == 0 {
            return Ok(());
        }
        bytes_read += n;
        if find_subsequence(&buffer[..bytes_read], b"\r\n\r\n").is_some() {
            break;
        }
        if bytes_read >= buffer.len() {
            break;
        }
    }

    let req_str = String::from_utf8_lossy(&buffer[..bytes_read]);
    let mut lines = req_str.lines();
    let req_line = match lines.next() {
        Some(l) => l,
        None => return Ok(()),
    };
    let parts: Vec<&str> = req_line.split_whitespace().collect();
    if parts.len() < 2 {
        return Ok(());
    }
    let method = parts[0];
    let path = parts[1];

    if method == "OPTIONS" {
        let response = "HTTP/1.1 200 OK\r\n\
                        Access-Control-Allow-Origin: *\r\n\
                        Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
                        Access-Control-Allow-Headers: *\r\n\
                        Content-Length: 0\r\n\r\n";
        stream.write_all(response.as_bytes())?;
        stream.flush()?;
        return Ok(());
    }

    if method == "GET" && (path == "/sse" || path.starts_with("/sse?")) {
        let response = "HTTP/1.1 200 OK\r\n\
                        Content-Type: text/event-stream\r\n\
                        Cache-Control: no-cache\r\n\
                        Connection: keep-alive\r\n\
                        Access-Control-Allow-Origin: *\r\n\r\n\
                        event: endpoint\r\n\
                        data: /message\r\n\r\n";
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
        let mut content_length = 0;
        let header_body_sep = req_str.find("\r\n\r\n").unwrap_or(bytes_read);
        let headers_str = &req_str[..header_body_sep];

        for line in headers_str.lines() {
            if line.to_lowercase().starts_with("content-length:") {
                if let Some(val_str) = line.split(':').nth(1) {
                    if let Ok(len) = val_str.trim().parse::<usize>() {
                        content_length = len;
                    }
                }
            }
        }

        let header_end = header_body_sep + 4;
        let mut body_bytes = buffer[header_end..bytes_read].to_vec();
        while body_bytes.len() < content_length {
            let mut temp = vec![0; content_length - body_bytes.len()];
            let n = stream.read(&mut temp)?;
            if n == 0 {
                break;
            }
            body_bytes.extend_from_slice(&temp[..n]);
        }

        let body_str = if body_bytes.len() > content_length {
            String::from_utf8_lossy(&body_bytes[..content_length]).into_owned()
        } else {
            String::from_utf8_lossy(&body_bytes).into_owned()
        };

        let rpc_req: serde_json::Value = match serde_json::from_str(&body_str) {
            Ok(val) => val,
            Err(e) => {
                let err_resp = format!(
                    "HTTP/1.1 400 Bad Request\r\n\
                     Content-Type: application/json\r\n\
                     Access-Control-Allow-Origin: *\r\n\r\n{}",
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

        let response_json = handle_mcp_request(rpc_req);
        if response_json.is_null() {
            let response = "HTTP/1.1 204 No Content\r\n\
                            Access-Control-Allow-Origin: *\r\n\r\n";
            stream.write_all(response.as_bytes())?;
        } else {
            let resp_str = serde_json::to_string(&response_json).unwrap_or_default();
            let response = format!(
                "HTTP/1.1 200 OK\r\n\
                 Content-Type: application/json\r\n\
                 Access-Control-Allow-Origin: *\r\n\
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
    let not_found = "HTTP/1.1 404 Not Found\r\n\
                     Access-Control-Allow-Origin: *\r\n\
                     Content-Length: 0\r\n\r\n";
    stream.write_all(not_found.as_bytes())?;
    stream.flush()?;
    Ok(())
}

pub fn serve(port: u16) -> Result<(), String> {
    let listener = TcpListener::bind(format!("0.0.0.0:{}", port))
        .map_err(|e| format!("Failed to bind to port {}: {}", port, e))?;
    println!("Lume MCP HTTP server listening on http://0.0.0.0:{}", port);

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                std::thread::spawn(move || {
                    if let Err(e) = handle_connection(stream) {
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

pub fn run_agent_loop(
    question: &str,
    ollama_url: &str,
    ollama_model: &str,
    db_dir: &str,
    verbose: bool,
) -> Result<(), String> {
    let url = format!("{}/api/chat", ollama_url.trim_end_matches('/'));

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
5. If you have searched multiple times with different queries and still cannot find the answer, state that you cannot find the answer in the text. Never make up an answer. \
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
                    if is_negative_response(&assistant_msg.content) && turn < max_turns {
                        println!("[Agent Guidance] Negative/uncertain answer detected. Requesting alternative search query.");
                        messages.push(AgentMessage {
                            role: "user".to_string(),
                            content: "The previous search did not yield the answer. Please try a different search query focusing on specific exact phrases or key words from the question (e.g. search for exact phrases in quotes like \"fancies himself\" or related words like \"captain\").".to_string(),
                            tool_calls: None,
                        });
                        continue;
                    }
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
                if is_negative_response(&assistant_msg.content) && turn < max_turns {
                    println!("[Agent Guidance] Negative/uncertain answer detected. Requesting alternative search query.");
                    messages.push(AgentMessage {
                        role: "user".to_string(),
                        content: "The previous search did not yield the answer. Please try a different search query focusing on specific exact phrases or key words from the question (e.g. search for exact phrases in quotes like \"fancies himself\" or related words like \"captain\").".to_string(),
                        tool_calls: None,
                    });
                    continue;
                }
                println!("\n[Agent Final Answer]\n{}", assistant_msg.content);
                return Ok(());
            }
        }
    }

    Err(format!("Agent exceeded maximum turns ({}) without finding a final answer.", max_turns))
}

fn is_negative_response(content: &str) -> bool {
    let lower = content.to_lowercase();
    lower.contains("not contain")
        || lower.contains("not explicitly")
        || lower.contains("not mentioned")
        || lower.contains("cannot find")
        || lower.contains("no mention")
        || lower.contains("no information")
        || lower.contains("sorry")
        || lower.contains("don't find")
        || lower.contains("do not find")
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
    let state_path = std::path::Path::new(db_dir).join("state.json");
    if !state_path.exists() {
        return Err(format!("Lume index state file not found at {}. Index a directory first.", state_path.display()));
    }
    let file_content = std::fs::read_to_string(&state_path)
        .map_err(|e| format!("Failed to read state.json: {}", e))?;
    let state: serde_json::Value = serde_json::from_str(&file_content)
        .map_err(|e| format!("Failed to parse state.json: {}", e))?;

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
    let graph_path = std::path::Path::new(db_dir).join("entity_graph.json");
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

    let resolved_url = if ollama_url.is_empty() || ollama_url == "http://localhost:11434" {
        let endpoints = ["http://host.docker.internal:11434", "http://localhost:11434", "http://172.17.0.1:11434"];
        let mut found = "http://localhost:11434".to_string();
        for ep in &endpoints {
            if let Ok(res) = ureq::get(&format!("{}/api/tags", ep)).timeout(std::time::Duration::from_secs(2)).call() {
                if res.status() == 200 {
                    found = ep.to_string();
                    break;
                }
            }
        }
        found
    } else {
        ollama_url.to_string()
    };

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


