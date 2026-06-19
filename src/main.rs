use std::collections::HashMap;
use std::env;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, Instant};

use serde::{Deserialize, Serialize};

use lume::bm25::{Bm25Index, Section, SearchHit, Bm25Params, SearchVariant};
use lume::spelling::SpellIndex;
use lume::semantic_mesh::EntityGraph;
use lume::Tagger;
use lume::Entry;

#[derive(Serialize, Deserialize, Debug, Clone)]
struct IndexState {
    target_dir: String,
    db_dir: String,
    semantic_enabled: bool,
    ollama_entities: bool,
    ollama_model: String,
    ollama_url: String,
    tag_dict_path: Option<String>,
    semantic_session_id: Option<String>,
    cached_files: HashMap<String, (u64, Vec<Section>)>,
}

fn save_json<T: Serialize>(path: &Path, val: &T) -> Result<(), String> {
    let file = File::create(path).map_err(|e| format!("Failed to create file {}: {}", path.display(), e))?;
    let writer = io::BufWriter::new(file);
    serde_json::to_writer_pretty(writer, val).map_err(|e| format!("Failed to write JSON to {}: {}", path.display(), e))?;
    Ok(())
}

fn load_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let file = File::open(path).map_err(|e| format!("Failed to open file {}: {}", path.display(), e))?;
    let reader = io::BufReader::new(file);
    let val = serde_json::from_reader(reader).map_err(|e| format!("Failed to parse JSON from {}: {}", path.display(), e))?;
    Ok(val)
}

fn main() {
    let mut args: Vec<String> = env::args().collect();
    
    // Parse global --shivvr-url parameter first
    let mut shivvr_url = None;
    let mut idx = 1;
    while idx < args.len() {
        if args[idx] == "--shivvr-url" {
            if idx + 1 < args.len() {
                shivvr_url = Some(args[idx + 1].clone());
                args.remove(idx + 1);
                args.remove(idx);
            } else {
                eprintln!("Error: --shivvr-url requires an argument");
                std::process::exit(1);
            }
        } else {
            idx += 1;
        }
    }

    if let Some(url) = shivvr_url {
        std::env::set_var("SHIVVR_BASE_URL", url);
    } else if std::env::var("SHIVVR_BASE_URL").is_err() {
        std::env::set_var("SHIVVR_BASE_URL", "http://localhost:8085");
    }

    if args.len() < 2 {
        print_global_help();
        return;
    }

    let subcommand = args[1].trim().to_lowercase();
    match subcommand.as_str() {
        "index" => {
            if args.len() >= 3 && args[2] == "update" {
                if let Err(e) = handle_index_update(&args[3..]) {
                    eprintln!("Error: {}", e);
                    std::process::exit(1);
                }
            } else {
                if let Err(e) = handle_index_init(&args[2..]) {
                    eprintln!("Error: {}", e);
                    std::process::exit(1);
                }
            }
        }
        "search" => {
            if let Err(e) = handle_search(&args[2..]) {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
        "generate" => {
            if let Err(e) = handle_generate(&args[2..]) {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
        "eval" => {
            if let Err(e) = handle_eval(&args[2..]) {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
        "stream" => {
            if let Err(e) = handle_stream(&args[2..]) {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
        "answer" => {
            if let Err(e) = handle_answer(&args[2..]) {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
        "summarize" => {
            if let Err(e) = handle_summarize(&args[2..]) {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
        "crawl" => {
            if args.iter().any(|a| a == "-h" || a == "--help") {
                print_crawl_help();
                return;
            }
            lume::crawl::run(args[2..].to_vec());
        }
        "serve" | "--serve" => {
            if args.iter().any(|a| a == "-h" || a == "--help") {
                print_serve_help();
                return;
            }
            // 5863 = "LUME" on a phone keypad. Unassigned by IANA, and unlike
            // 8000 nothing else grabs it by default.
            let mut port = 5863u16;
            if let Some(pos) = args.iter().position(|a| a == "--port" || a == "-p") {
                if pos + 1 < args.len() {
                    if let Ok(p) = args[pos + 1].parse::<u16>() {
                        port = p;
                    }
                }
            }
            if let Err(e) = lume::agent::serve(port) {
                eprintln!("Error starting serve mode: {}", e);
                std::process::exit(1);
            }
        }
        "agent" | "chat" => {
            if args.iter().any(|a| a == "-h" || a == "--help") {
                print_agent_help();
                return;
            }
            let mut ollama_url = String::from("http://localhost:11434");
            let mut ollama_model = String::from("gemma4:31b-cloud");
            let mut verbose = false;
            let mut db_dir = String::from(".lume-index");
            let mut question_parts = Vec::new();
            
            let mut idx = 2;
            while idx < args.len() {
                let arg = &args[idx];
                if arg == "--ollama-url" && idx + 1 < args.len() {
                    ollama_url = args[idx + 1].clone();
                    idx += 2;
                } else if arg == "--ollama-model" && idx + 1 < args.len() {
                    ollama_model = args[idx + 1].clone();
                    idx += 2;
                } else if arg == "--db" && idx + 1 < args.len() {
                    db_dir = args[idx + 1].clone();
                    idx += 2;
                } else if arg == "-v" || arg == "-V" || arg == "--verbose" {
                    verbose = true;
                    idx += 1;
                } else {
                    question_parts.push(arg.clone());
                    idx += 1;
                }
            }
            
            let question = question_parts.join(" ");
            if question.trim().is_empty() {
                eprintln!("Error: Missing question. Usage: lume agent <QUESTION>");
                std::process::exit(1);
            }
            
            if let Err(e) = lume::agent::run_agent_loop(&question, &ollama_url, &ollama_model, &db_dir, verbose) {
                eprintln!("Error running agent: {}", e);
                std::process::exit(1);
            }
        }
        "help" | "-h" | "--help" => {
            print_global_help();
        }
        "version" | "-v" | "--version" => {
            println!("lume {}", env!("CARGO_PKG_VERSION"));
        }
        _ => {
            eprintln!("Unknown subcommand: {}", args[1]);
            print_global_help();
            std::process::exit(1);
        }
    }
}

fn print_global_help() {
    println!(r#"  _      _    _ __  __ ______
  | |    | |  | |  \/  |  ____|
  | |    | |  | | \  / | |__
  | |    | |  | | |\/| |  __|
  | |____| |__| | |  | | |____
  |______|\____/|_|  |_|______|  v{}

High-performance, stateful FST-backed tagger & BM25 hybrid search engine suite.

USAGE:
  lume [FLAGS] <SUBCOMMAND>

FLAGS:
  -h, --help           Prints help information
  -V, --version        Prints version information
  --shivvr-url <URL>   Shivvr endpoint URL [default: http://localhost:8085]

SUBCOMMANDS:
  index      Index a directory (supports code, text, and PDF files)
  search     Query the persisted index using lexical, semantic, or hybrid search
  generate   Generate style-faithful text from the indexed corpus
  serve      Start the MCP server over HTTP transport (alias: --serve)
  agent      Run an autonomous agent loop to answer a question (alias: chat)
  summarize  Agentic document summarizer via planning, search exploration, and synthesis
  crawl      Stealth crawl webpage content and save to personal search collection
  eval       Measure retrieval quality (Hit@k, MRR, nDCG@k) against a Q&A file
  stream     Stream the live phase/Weber search relaxation as NDJSON for the 3D visualizer
  answer     Agentic plan→retrieve→answer loop with citations, streamed for the visualizer
"#, env!("CARGO_PKG_VERSION"));
}

fn print_index_help() {
    println!(r#"lume-index
Index a directory of text, code, and PDF files. Supports incremental updates.

USAGE:
  lume index [FLAGS] [OPTIONS] <DIR>
  lume index update [FLAGS] [OPTIONS]

FLAGS:
  -h, --help               Prints help information
  -s, --semantic           Enable dense semantic vector search (requires NUTS token)
  -o, --ollama-entities    Enable AI entity extraction via local Gemma on Ollama
  -f, --force              Force re-indexing of all files (ignoring modification times)

OPTIONS:
  --db <PATH>            Path to store the persisted index metadata [default: .lume-index]
  --tag-dict <PATH>      Path to FST phrase dictionary CSV
  --ollama-model <NAME>  Local Ollama model to use for entity extraction [default: gpt-4o-mini:latest]
  --ollama-url <URL>     Ollama API endpoint [default: http://localhost:11434]
  --shivvr-url <URL>     Shivvr endpoint URL [default: http://localhost:8085]
  --chunks <RANGE>       Specific chunk(s) to run entity extraction on (e.g., "2", "1-5", "2-")

ENV:
  LUME_EXTRACT_WORKERS   Concurrent entity-extraction threads [default: 10]
  LUME_EXTRACTOR_PATH    Explicit path to lume_extractor.py (default: auto-detect next to the executable)

ARGS:
  <DIR>                  Directory to index (omitted when running 'update')
"#);
}

fn print_search_help() {
    println!(r#"lume-search
Query the index using lexical, semantic, or hybrid search.

USAGE:
  lume search [FLAGS] [OPTIONS] <QUERY>

FLAGS:
  -h, --help            Prints help information
  -c, --spell-check     Enable spelling correction on search query

OPTIONS:
  --db <PATH>           Path to the persisted index metadata [default: .lume-index]
  -l, --limit <LIMIT>   Max number of search hits [default: 10]
  -a, --alpha <VAL>     Hybrid blending weight: 0.0 (BM25 only) to 1.0 (semantic only) [default: 0.5]
  -g, --graph <VAL>     SKG entity-graph boost weight; 0 disables [default: 0.4, env GRAPH_ALPHA]
  --scoring <MODE>      SKG edge weighting: 'relatedness' (significance, default) or 'jaccard' (overlap)
  --shivvr-url <URL>    Shivvr endpoint URL [default: http://localhost:8085]

ENV:
  LUME_QUERY_INVERSION  Set to 1 to print the query's embedding inversion (debug; costs an extra round-trip)
  LUME_BLEND_NORM       Set to 1 for normalized blending (bm25/max + α·sem + β·skg)

ARGS:
  <QUERY>               Search query string
"#);
}

fn print_eval_help() {
    println!(r#"lume-eval
Measure retrieval quality against a Q&A file using the lexical BM25 + SKG-graph
pipeline. Relevance is judged by answer-token containment (no human labels
needed): a retrieved section counts as relevant when it contains at least
--threshold of the answer's content tokens.

USAGE:
  lume eval [FLAGS] [OPTIONS] <QNA_JSON>

FLAGS:
  -h, --help              Prints help information
  -c, --spell-check       Spell-correct each question before searching
  --compare               Run both scoring modes and print the Jaccard vs. relatedness delta

OPTIONS:
  --db <PATH>             Path to the persisted index metadata [default: .lume-index]
  -k, --limit <K>         Cut-off for Hit@k / nDCG@k and results retrieved [default: 10]
  -g, --graph <VAL>       SKG entity-graph boost weight; 0 disables the graph [default: 0.4]
  --scoring <MODE>        SKG edge weighting: 'relatedness' (default) or 'jaccard' (ignored with --compare)
  -t, --threshold <VAL>   Answer-token recall needed to count a section relevant [default: 0.5]
  -n, --max-questions <N> Evaluate only the first N questions (smoke test)

ARGS:
  <QNA_JSON>              Q&A file: a JSON array of {{question, answer}} objects
                         (as produced by 'python lib/lume_extractor.py qna')
"#);
}

fn handle_index_init(args: &[String]) -> Result<(), String> {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print_index_help();
        return Ok(());
    }

    let mut db_dir = String::from(".lume-index");
    let mut tag_dict_path: Option<String> = None;
    let mut semantic_enabled = false;
    let mut ollama_entities = false;
    let mut ollama_model = String::from("gpt-4o-mini:latest");
    let mut ollama_url = String::from("http://localhost:11434");
    let mut force = false;
    let mut dir_to_index: Option<String> = None;
    let mut chunk_range: Option<(usize, usize)> = None;

    let mut idx = 0;
    while idx < args.len() {
        let arg = &args[idx];
        if arg == "-s" || arg == "--semantic" {
            semantic_enabled = true;
            idx += 1;
        } else if arg == "-o" || arg == "--ollama-entities" {
            ollama_entities = true;
            idx += 1;
        } else if arg == "-f" || arg == "--force" {
            force = true;
            idx += 1;
        } else if arg == "--db" && idx + 1 < args.len() {
            db_dir = args[idx + 1].clone();
            idx += 2;
        } else if arg == "--tag-dict" && idx + 1 < args.len() {
            tag_dict_path = Some(args[idx + 1].clone());
            idx += 2;
        } else if arg == "--ollama-model" && idx + 1 < args.len() {
            ollama_model = args[idx + 1].clone();
            idx += 2;
        } else if arg == "--ollama-url" && idx + 1 < args.len() {
            ollama_url = args[idx + 1].clone();
            idx += 2;
        } else if arg == "--chunks" && idx + 1 < args.len() {
            let val = &args[idx + 1];
            if let Some(pos) = val.find('-') {
                let start_str = &val[..pos];
                let end_str = &val[pos+1..];
                let start = start_str.parse::<usize>().unwrap_or(1);
                let end = end_str.parse::<usize>().unwrap_or(usize::MAX);
                chunk_range = Some((start, end));
            } else if let Ok(n) = val.parse::<usize>() {
                chunk_range = Some((n, n));
            }
            idx += 2;
        } else if arg.starts_with('-') {
            return Err(format!("Unknown option: {}", arg));
        } else {
            if dir_to_index.is_some() {
                return Err(format!("Too many directories specified: {}", arg));
            }
            dir_to_index = Some(arg.clone());
            idx += 1;
        }
    }

    let dir = dir_to_index.ok_or_else(|| String::from("Missing directory to index"))?;

    let db_path = Path::new(&db_dir);
    let state_file_path = db_path.join("state.json");
    let mut cached_files = HashMap::new();
    if state_file_path.exists() {
        if let Ok(state) = load_json::<IndexState>(&state_file_path) {
            if state.target_dir == dir {
                cached_files = state.cached_files;
            }
        }
    }

    run_indexing(
        &dir,
        &db_dir,
        tag_dict_path,
        semantic_enabled,
        ollama_entities,
        ollama_model,
        ollama_url,
        force,
        cached_files,
        chunk_range,
    )?;

    Ok(())
}

fn handle_index_update(args: &[String]) -> Result<(), String> {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print_index_help();
        return Ok(());
    }

    let mut db_dir = String::from(".lume-index");
    let mut force = false;

    let mut chunk_range: Option<(usize, usize)> = None;

    let mut idx = 0;
    while idx < args.len() {
        let arg = &args[idx];
        if arg == "-f" || arg == "--force" {
            force = true;
            idx += 1;
        } else if arg == "--db" && idx + 1 < args.len() {
            db_dir = args[idx + 1].clone();
            idx += 2;
        } else if arg == "--chunks" && idx + 1 < args.len() {
            let val = &args[idx + 1];
            if let Some(pos) = val.find('-') {
                let start_str = &val[..pos];
                let end_str = &val[pos+1..];
                let start = start_str.parse::<usize>().unwrap_or(1);
                let end = end_str.parse::<usize>().unwrap_or(usize::MAX);
                chunk_range = Some((start, end));
            } else if let Ok(n) = val.parse::<usize>() {
                chunk_range = Some((n, n));
            }
            idx += 2;
        } else {
            idx += 1;
        }
    }

    let db_path = Path::new(&db_dir);
    let state_file_path = db_path.join("state.json");
    if !state_file_path.exists() {
        return Err(format!("Index state file not found at {}. Run 'lume index <DIR>' first.", state_file_path.display()));
    }

    let state: IndexState = load_json(&state_file_path)?;

    println!("Updating index for target directory: {}", state.target_dir);

    run_indexing(
        &state.target_dir,
        &state.db_dir,
        state.tag_dict_path,
        state.semantic_enabled,
        state.ollama_entities,
        state.ollama_model,
        state.ollama_url,
        force,
        state.cached_files,
        chunk_range,
    )?;

    Ok(())
}

/// Reads `.lumeignore` from the target directory root: one entry per line,
/// either a bare name (matches any dir/file with that name) or a path
/// relative to the target root. `#` lines and blanks are skipped.
fn load_lumeignore(root: &Path) -> Vec<String> {
    let Ok(content) = fs::read_to_string(root.join(".lumeignore")) else {
        return Vec::new();
    };
    content
        .lines()
        .map(|l| l.trim().trim_end_matches('/').replace('\\', "/"))
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect()
}

fn is_ignored(path: &Path, root: &Path, ignores: &[String]) -> bool {
    if ignores.is_empty() {
        return false;
    }
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let rel = path
        .strip_prefix(root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    ignores.iter().any(|pat| name == pat || rel == *pat)
}

fn scan_directory(
    dir: &Path,
    root: &Path,
    db_dir: &Path,
    ignores: &[String],
    files: &mut Vec<PathBuf>,
) -> io::Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name == ".git" || name == "target" || name == ".venv" || name == ".lume-index" || path == db_dir {
                continue;
            }
            if is_ignored(&path, root, ignores) {
                continue;
            }
            scan_directory(&path, root, db_dir, ignores, files)?;
        } else if path.is_file() {
            if is_ignored(&path, root, ignores) {
                continue;
            }
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                let ext_lower = ext.to_lowercase();
                if matches!(
                    ext_lower.as_str(),
                    "pdf" | "txt" | "md" | "rs" | "py" | "js" | "ts" | "go" | "c" | "cpp" | "h" | "java" | "sh" | "yml" | "yaml" | "toml" | "html" | "css" | "ini" | "cfg" | "conf"
                ) {
                    files.push(path);
                }
            }
        }
    }
    Ok(())
}

/// Locates lib/lume_extractor.py. The script ships with the lume source tree,
/// so a cwd-relative path only works when invoked from the repo root — resolve
/// against the executable's location instead (target/release/lume.exe →
/// ancestors → repo root), with LUME_EXTRACTOR_PATH as an explicit override
/// for installs that relocate the script.
fn find_extractor_script() -> PathBuf {
    if let Ok(p) = env::var("LUME_EXTRACTOR_PATH") {
        let p = PathBuf::from(p);
        if p.exists() {
            return p;
        }
        eprintln!("[⚠️] LUME_EXTRACTOR_PATH is set but {} does not exist; falling back to auto-detection", p.display());
    }
    if let Ok(exe) = env::current_exe() {
        for dir in exe.ancestors().skip(1) {
            let candidate = dir.join("lib").join("lume_extractor.py");
            if candidate.exists() {
                return candidate;
            }
        }
    }
    PathBuf::from("lib/lume_extractor.py")
}

fn run_extractor_pdf(pdf_path: &Path) -> Result<Vec<Section>, String> {
    let script = find_extractor_script();
    let output = Command::new("uv")
        .arg("run")
        .arg(&script)
        .arg("pdf")
        .arg(pdf_path)
        .output()
        .map_err(|e| format!("Failed to spawn `uv run {}`: {}", script.display(), e))?;

    if !output.status.success() {
        return Err(format!("Python extractor failed: {}", String::from_utf8_lossy(&output.stderr)));
    }

    let stdout_str = String::from_utf8_lossy(&output.stdout);
    let json_val: serde_json::Value = serde_json::from_str(&stdout_str)
        .map_err(|e| format!("Failed to parse extractor output: {}. Raw: {}", e, stdout_str))?;

    if !json_val["success"].as_bool().unwrap_or(false) {
        return Err(json_val["error"].as_str().unwrap_or("Unknown error").to_string());
    }

    let mut sections = Vec::new();
    let pages = json_val["pages"].as_array().ok_or("No pages in output")?;
    let filename_str = pdf_path.to_string_lossy().to_string();
    for page in pages {
        let page_num = page["page_number"].as_u64().unwrap_or(0);
        let text = page["text"].as_str().unwrap_or("").to_string();
        sections.push(Section {
            title: format!("Page {}", page_num),
            body: text,
            line_number: page_num as usize,
            filename: Some(filename_str.clone()),
            entities: Vec::new(),
        });
    }
    Ok(sections)
}

fn run_extractor_entities(
    text: &str,
    ollama_url: &str,
    ollama_model: &str,
) -> Result<Vec<String>, String> {
    lume::agent::extract_entities(text, ollama_url, ollama_model)
}

/// Target / hard-cap line counts for a markdown retrieval chunk. Gutenberg
/// prose wraps at ~11 words/line, so ~30 lines ≈ ~300 tokens — small enough for
/// sharp BM25 ranking, large enough to keep a passage coherent.
const CHUNK_TARGET_LINES: usize = 30;
const CHUNK_MAX_LINES: usize = 50;

/// Splits a chapter body into chunks of roughly `target` lines, extending each
/// chunk to the next blank-line (paragraph) boundary so paragraphs aren't cut
/// mid-thought, up to `hard_cap` lines. Returns (line_offset_within_body, text)
/// pairs; blank-only chunks are dropped.
fn split_body_into_chunks(body: &str, target: usize, hard_cap: usize) -> Vec<(usize, String)> {
    let lines: Vec<&str> = body.lines().collect();
    let mut chunks = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let mut end = (i + target).min(lines.len());
        let cap = (i + hard_cap).min(lines.len());
        while end < cap && !lines[end].trim().is_empty() {
            end += 1;
        }
        let text = lines[i..end].join("\n");
        if !text.trim().is_empty() {
            chunks.push((i, text));
        }
        i = end;
    }
    chunks
}

fn chunk_text_file(path: &Path, content: &str) -> Vec<Section> {
    let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let filename_str = path.to_string_lossy().to_string();

    if extension.to_lowercase() == "md" {
        let raw_sections = lume::bm25::parse_markdown(content);
        let mut sections = Vec::new();
        for sec in raw_sections {
            // Split each chapter into retrieval-sized windows (~TARGET_LINES,
            // snapped to the next paragraph boundary) instead of two giant
            // half-chapter blobs. Smaller sections give BM25 sharper length
            // normalization and let snippets show the matching passage.
            let pieces = split_body_into_chunks(&sec.body, CHUNK_TARGET_LINES, CHUNK_MAX_LINES);
            if pieces.len() <= 1 {
                let mut s = sec;
                s.filename = Some(filename_str.clone());
                sections.push(s);
            } else {
                let total = pieces.len();
                for (idx, (line_offset, body)) in pieces.into_iter().enumerate() {
                    sections.push(Section {
                        title: format!("{} (Part {}/{})", sec.title, idx + 1, total),
                        body,
                        line_number: sec.line_number + line_offset,
                        filename: Some(filename_str.clone()),
                        entities: Vec::new(),
                    });
                }
            }
        }
        sections
    } else {
        let lines: Vec<&str> = content.lines().collect();
        let chunk_size = 25;
        let mut sections = Vec::new();
        for (idx, chunk) in lines.chunks(chunk_size).enumerate() {
            let start_line = idx * chunk_size + 1;
            let end_line = start_line + chunk.len() - 1;
            let body = chunk.join("\n");
            sections.push(Section {
                title: format!("Lines {}-{}", start_line, end_line),
                body,
                line_number: start_line,
                filename: Some(filename_str.clone()),
                entities: Vec::new(),
            });
        }
        sections
    }
}

fn load_tagger_csv(path: &Path) -> io::Result<Tagger> {
    let kind = path.file_stem().and_then(|s| s.to_str()).unwrap_or("entity").to_string();
    let text = std::fs::read_to_string(path)?;
    let mut lines = text.lines();
    let header_line = match lines.next() {
        Some(h) => h,
        None => return Err(io::Error::new(io::ErrorKind::InvalidData, "Empty CSV file")),
    };
    let headers = lume::parse_csv_line(header_line);
    let action_col = headers
        .iter()
        .position(|h| h.trim().eq_ignore_ascii_case("action"));
    let is_regex_col = headers
        .iter()
        .position(|h| h.trim().eq_ignore_ascii_case("is_regex"));

    let mut entries = Vec::new();
    for (i, raw) in lines.enumerate() {
        let cells = lume::parse_csv_line(raw);
        let phrase = cells.first().map(|s| s.trim()).unwrap_or("");
        if phrase.is_empty() {
            continue;
        }
        let output_override = action_col
            .and_then(|idx| cells.get(idx))
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let is_regex_val = is_regex_col
            .and_then(|idx| cells.get(idx))
            .map(|s| s.trim().eq_ignore_ascii_case("true"))
            .unwrap_or(false);

        let mut entry = Entry::new(phrase, kind.clone(), format!("csv-{}", i))
            .with_regex(is_regex_val);
        if let Some(out) = output_override {
            entry = entry.with_output(out);
        }
        entries.push(entry);
    }
    Tagger::build(entries)
}

/// Reads a file as text, tolerating non-UTF-8 content. UTF-16 files (BOM) are
/// decoded properly; other encodings are decoded lossily; content that still
/// looks binary (>5% undecodable) yields Ok(None) so the caller can skip the
/// file instead of aborting the whole indexing run.
fn read_text_tolerant(path: &Path) -> Result<Option<String>, String> {
    // Accept the decoded text only if <5% of it is replacement chars / NULs;
    // otherwise the file is treated as binary.
    fn accept(s: String) -> Option<String> {
        let total = s.chars().count();
        let bad = s.chars().filter(|&c| c == '\u{FFFD}' || c == '\0').count();
        if total == 0 || bad * 20 > total {
            None
        } else if bad > 0 {
            Some(s.chars().filter(|&c| c != '\u{FFFD}' && c != '\0').collect())
        } else {
            Some(s)
        }
    }

    let bytes = fs::read(path).map_err(|e| format!("Failed to read file {}: {}", path.display(), e))?;
    if bytes.is_empty() {
        return Ok(Some(String::new()));
    }
    if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE {
        let utf16: Vec<u16> = bytes[2..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        return Ok(accept(String::from_utf16_lossy(&utf16)));
    }
    if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
        let utf16: Vec<u16> = bytes[2..].chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        return Ok(accept(String::from_utf16_lossy(&utf16)));
    }
    // BOM-less UTF-16: NUL bytes are valid UTF-8, so a UTF-16 file of mostly
    // ASCII text decodes "successfully" as UTF-8 with a NUL between every
    // character. If >25% of bytes are NUL, infer endianness from whether the
    // NULs sit at odd (LE) or even (BE) offsets and decode as UTF-16.
    let nul_total = bytes.iter().filter(|&&b| b == 0).count();
    if nul_total * 4 > bytes.len() {
        let odd_nuls = bytes.iter().skip(1).step_by(2).filter(|&&b| b == 0).count();
        let utf16: Vec<u16> = if odd_nuls * 2 >= nul_total {
            bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect()
        } else {
            bytes.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect()
        };
        return Ok(accept(String::from_utf16_lossy(&utf16)));
    }
    match String::from_utf8(bytes) {
        Ok(s) => Ok(accept(s)),
        Err(e) => Ok(accept(String::from_utf8_lossy(e.as_bytes()).into_owned())),
    }
}

/// Rebuilds the searchable index files (bm25.json, spelling.json,
/// entity_graph.json) from the currently cached sections and writes them to
/// the db dir. Called periodically during long `-o`/`-s` runs so the index is
/// searchable while indexing is still in progress, and once at the end.
/// Returns the number of sections written.
/// Flattens cached per-file sections into one Vec in path-sorted order.
/// Section ordering MUST be deterministic: semantic hits map back to sections
/// by index (`source` = section idx), so the ingest pass and every bm25
/// rebuild have to agree on the order. HashMap iteration does not.
fn collect_all_sections(cached_files: &HashMap<String, (u64, Vec<Section>)>) -> Vec<Section> {
    let mut paths: Vec<&String> = cached_files.keys().collect();
    paths.sort();
    let mut all_sections = Vec::new();
    for path in paths {
        all_sections.extend(cached_files[path].1.clone());
    }
    all_sections
}

fn flush_searchable_indexes(
    cached_files: &HashMap<String, (u64, Vec<Section>)>,
    tagger: Option<&Tagger>,
    tagger_phrases: &[String],
    db_path: &Path,
) -> Result<usize, String> {
    let all_sections = collect_all_sections(cached_files);
    if all_sections.is_empty() {
        return Ok(0);
    }
    let count = all_sections.len();
    let bm25 = Bm25Index::build(all_sections, tagger);
    let corpus_terms: Vec<Vec<u8>> = bm25.posting_lists.keys().cloned().collect();
    let spelling = SpellIndex::build(tagger_phrases, &corpus_terms);
    let entity_graph = EntityGraph::build(
        &bm25.entity_posting_lists,
        &bm25.entity_kinds,
        &bm25.entity_labels,
        0.1,
        bm25.sections.len(),
    );
    save_json(&db_path.join("bm25.json"), &bm25)?;
    save_json(&db_path.join("spelling.json"), &spelling)?;
    save_json(&db_path.join("entity_graph.json"), &entity_graph)?;
    Ok(count)
}

/// Minimum time between mid-run searchable-index flushes.
const FLUSH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

/// Formats a duration in seconds as a compact human-readable ETA ("47s",
/// "3m12s", "1h05m").
fn format_eta(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    if s >= 3600 {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{}s", s)
    }
}

fn run_indexing(
    target_dir: &str,
    db_dir: &str,
    tag_dict_path: Option<String>,
    semantic_enabled: bool,
    ollama_entities: bool,
    ollama_model: String,
    ollama_url: String,
    force: bool,
    mut cached_files: HashMap<String, (u64, Vec<Section>)>,
    chunk_range: Option<(usize, usize)>,
) -> Result<(), String> {
    let total_start = Instant::now();
    let target_path = Path::new(target_dir);
    if !target_path.exists() {
        return Err(format!("Target directory {} does not exist", target_dir));
    }

    let db_path = Path::new(db_dir);
    fs::create_dir_all(db_path).map_err(|e| format!("Failed to create db dir: {}", e))?;
    // Session/semantic caches live with the index, not in the process cwd.
    lume::hybrid::set_cache_dir(db_path);

    let scan_start = Instant::now();
    let ignores = load_lumeignore(target_path);
    if !ignores.is_empty() {
        println!("[🚫] .lumeignore active ({} patterns): {}", ignores.len(), ignores.join(", "));
    }
    let mut files = Vec::new();
    scan_directory(target_path, target_path, db_path, &ignores, &mut files).map_err(|e| format!("Failed to scan directory: {}", e))?;
    let scan_duration = scan_start.elapsed();
    let total_files = files.len();
    println!("[📁] Scanned {} indexable files in {:?}", total_files, scan_duration);

    // Tagger is loaded up front so mid-run index flushes can use it.
    let mut tagger = None;
    let mut tagger_phrases = Vec::new();
    if let Some(ref tag_dict) = tag_dict_path {
        let tag_dict_p = Path::new(tag_dict);
        if tag_dict_p.exists() {
            println!("[📊] Loading tagger dictionary from: {}", tag_dict);
            let t = load_tagger_csv(tag_dict_p).map_err(|e| format!("Failed to load tagger dictionary: {}", e))?;
            tagger_phrases = t.phrases().to_vec();
            tagger = Some(t);
        } else {
            eprintln!("[⚠️] Dictionary path {} does not exist. Skipping tagger.", tag_dict);
        }
    }

    let mut processed_paths = std::collections::HashSet::new();
    let mut last_flush = Instant::now();
    let mut files_indexed = 0usize;
    let mut files_skipped_binary = 0usize;

    for (file_num, file_path) in files.iter().enumerate() {
        let file_progress = format!("[file {}/{}]", file_num + 1, total_files);
        let path_str = file_path.to_string_lossy().to_string();
        processed_paths.insert(path_str.clone());

        let metadata = fs::metadata(file_path).map_err(|e| e.to_string())?;
        let mtime = metadata.modified()
            .map_err(|e| e.to_string())?
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_secs();

        let mut needs_index = force 
            || !cached_files.contains_key(&path_str) 
            || cached_files.get(&path_str).unwrap().0 != mtime;

        if !needs_index && ollama_entities {
            if let Some((_, cached_sections)) = cached_files.get(&path_str) {
                if cached_sections.iter().any(|s| s.entities.is_empty()) {
                    needs_index = true;
                }
            }
        }

        if needs_index {
            let file_start = Instant::now();
            let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
            let mut sections = if ext == "pdf" {
                println!("[⚙️] {} Processing PDF file: {}", file_progress, path_str);
                run_extractor_pdf(file_path)?
            } else {
                let content = match read_text_tolerant(file_path)? {
                    Some(c) => c,
                    None => {
                        println!("[⚠️] {} Skipping {}: content is not text (binary or undecodable)", file_progress, path_str);
                        files_skipped_binary += 1;
                        continue;
                    }
                };
                if ext == "html" || ext == "htm" {
                    let (_title, cleaned) = lume::crawl::clean_html_to_markdown(&content);
                    let chunks = chunk_text_file(file_path, &cleaned);
                    println!("[⚙️] {} Processing HTML file (cleaned): {} (parsed into {} chunks)", file_progress, path_str, chunks.len());
                    chunks
                } else {
                    let chunks = chunk_text_file(file_path, &content);
                    println!("[⚙️] {} Processing text file: {} (parsed into {} chunks)", file_progress, path_str, chunks.len());
                    chunks
                }
            };
            
            let parse_duration = file_start.elapsed();
            println!("[🕒] Parsed/chunked in {:?}", parse_duration);

            if !force {
                if let Some((_, cached_sections)) = cached_files.get(&path_str) {
                    for sec in &mut sections {
                        if let Some(matching_cached) = cached_sections.iter().find(|cs| cs.title == sec.title && cs.line_number == sec.line_number) {
                            if !matching_cached.entities.is_empty() {
                                sec.entities = matching_cached.entities.clone();
                            }
                        }
                    }
                }
            }

            cached_files.insert(path_str, (mtime, sections));
            files_indexed += 1;

            if last_flush.elapsed() >= FLUSH_INTERVAL {
                match flush_searchable_indexes(&cached_files, tagger.as_ref(), &tagger_phrases, db_path) {
                    Ok(n) => println!("[💾] {} Searchable index flushed mid-run ({} sections)", file_progress, n),
                    Err(e) => eprintln!("[⚠️] Mid-run index flush failed: {}", e),
                }
                last_flush = Instant::now();
            }
        }
    }

    let cached_paths: Vec<String> = cached_files.keys().cloned().collect();
    for path_str in cached_paths {
        if !processed_paths.contains(&path_str) {
            println!("[🗑️] Removing deleted file from index cache: {}", path_str);
            cached_files.remove(&path_str);
        }
    }

    let all_sections = collect_all_sections(&cached_files);

    if all_sections.is_empty() {
        println!("[⚠️] No sections to index.");
        return Ok(());
    }

    println!(
        "[📊] Indexing {} sections total ({} files indexed this run, {} skipped as binary, {} files in corpus)...",
        all_sections.len(), files_indexed, files_skipped_binary, total_files
    );

    // ── Pass 2: semantic ingest BEFORE entity extraction ──
    // Embeddings depend only on chunked text, not on entities, so with -s -o
    // the dense vectors are available for hybrid search while extraction is
    // still grinding.
    let mut semantic_session_id = None;
    if semantic_enabled {
        let semantic_start = Instant::now();
        let shivver_url = lume::hybrid::get_shivvr_base_url();
        println!("[🌐] Initializing semantic vector store session on {}...", shivver_url);
        if let Some(token) = lume::hybrid::load_nuts_token() {
            // Fingerprint the corpus with the SAME function the search path uses
            // (get_corpus_metadata), so the saved session cache actually matches
            // at query time. Previously index-time used summed section lengths +
            // mtime 0 while search recomputed from file bytes, so the cache never
            // matched and every hybrid search silently re-ingested the corpus.
            let (corpus_size, corpus_mtime) =
                lume::hybrid::get_corpus_metadata(target_path).unwrap_or((0, 0));
            match lume::hybrid::ensure_semantic_session(
                target_dir,
                &all_sections,
                corpus_size,
                corpus_mtime,
                &token,
            ) {
                Ok(sess_id) => {
                    println!("[🌐] Ingested into semantic session: {} (completed in {:?})", sess_id, semantic_start.elapsed());
                    semantic_session_id = Some(sess_id);
                }
                Err(err) => {
                    eprintln!("[⚠️] Semantic ingestion failed: {}", err);
                }
            }
        } else {
            eprintln!("[⚠️] NUTS_SERVICES_TOKEN not found. Semantic indexing skipped.");
        }
    }

    // Make the db searchable (and the semantic session visible to the search
    // gate) before the slow extraction pass begins.
    let early_flush_start = Instant::now();
    let early_count = flush_searchable_indexes(&cached_files, tagger.as_ref(), &tagger_phrases, db_path)?;
    let early_state = IndexState {
        target_dir: target_dir.to_string(),
        db_dir: db_dir.to_string(),
        semantic_enabled,
        ollama_entities,
        ollama_model: ollama_model.clone(),
        ollama_url: ollama_url.clone(),
        tag_dict_path: tag_dict_path.clone(),
        semantic_session_id: semantic_session_id.clone(),
        cached_files: cached_files.clone(),
    };
    save_json(&db_path.join("state.json"), &early_state)?;
    println!(
        "[💾] Index searchable: {} sections written to {} in {:?}",
        early_count, db_dir, early_flush_start.elapsed()
    );
    last_flush = Instant::now();

    // ── Pass 3: corpus-wide entity extraction ──
    // One worklist across every file keeps all workers busy even when the
    // corpus is thousands of small files.
    if ollama_entities {
        struct ExtractTask {
            file_path: String,
            sec_idx: usize,
            title: String,
            body: String,
        }

        let mut skipped = 0usize;
        let mut tasks: Vec<ExtractTask> = Vec::new();
        {
            let mut paths: Vec<&String> = cached_files.keys().collect();
            paths.sort();
            for path in paths {
                for (sec_idx, sec) in cached_files[path].1.iter().enumerate() {
                    // Chunk-range numbering stays per-file, as before.
                    let chunk_num = sec_idx + 1;
                    if let Some((start, end)) = chunk_range {
                        if chunk_num < start || chunk_num > end {
                            continue;
                        }
                    }
                    if !sec.entities.is_empty() {
                        skipped += 1;
                        continue;
                    }
                    tasks.push(ExtractTask {
                        file_path: path.clone(),
                        sec_idx,
                        title: sec.title.clone(),
                        body: sec.body.clone(),
                    });
                }
            }
        }

        let total = tasks.len();
        let ollama_start_all = Instant::now();
        println!(
            "[🧠] Extracting AI entities using Ollama ({}) — {} pending, {} cached, across {} files...",
            ollama_model, total, skipped, cached_files.len()
        );

        if total > 0 {
            use std::sync::{mpsc, Arc, Mutex};

            // Default 10 concurrent Ollama calls; tune with
            // LUME_EXTRACT_WORKERS (e.g. lower for local models bound by
            // OLLAMA_NUM_PARALLEL, higher for cloud relays).
            const EXTRACT_WORKERS: usize = 10;
            let num_workers = std::env::var("LUME_EXTRACT_WORKERS")
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
                .filter(|&n| n >= 1)
                .unwrap_or(EXTRACT_WORKERS)
                .min(total);
            let tasks = Arc::new(tasks);
            let next_task = Arc::new(Mutex::new(0usize));
            let (tx, rx) = mpsc::channel();

            let mut ok_count = 0usize;
            let mut fail_count = 0usize;

            std::thread::scope(|s| {
                for worker_id in 0..num_workers {
                    let tasks = Arc::clone(&tasks);
                    let next_task = Arc::clone(&next_task);
                    let tx = tx.clone();
                    let ollama_url = &ollama_url;
                    let ollama_model = &ollama_model;
                    s.spawn(move || loop {
                        let idx = {
                            let mut lock = next_task.lock().unwrap();
                            let idx = *lock;
                            if idx >= tasks.len() {
                                break;
                            }
                            *lock += 1;
                            idx
                        };
                        let started = Instant::now();
                        let result = run_extractor_entities(&tasks[idx].body, ollama_url, ollama_model);
                        if tx.send((worker_id, idx, result, started.elapsed())).is_err() {
                            break;
                        }
                    });
                }
                drop(tx);

                // Collector: the only thread that touches `cached_files` and
                // stdout, so progress lines stay whole and state.json
                // checkpoints never race. The receiver loop ends when every
                // worker has dropped its sender.
                let mut done = 0usize;
                for (worker_id, task_idx, result, elapsed) in rx.iter() {
                    let task = &tasks[task_idx];
                    let file_label = Path::new(&task.file_path)
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or(task.file_path.as_str());
                    done += 1;
                    // ETA from observed throughput: remaining chunks × average
                    // seconds per completed chunk this run.
                    let run_elapsed = ollama_start_all.elapsed().as_secs_f64();
                    let eta = format_eta((total - done) as f64 * run_elapsed / done as f64);
                    match result {
                        Ok(mut ents) => {
                            ok_count += 1;
                            if ents.is_empty() {
                                ents.push("__LUME_PROCESSED__".to_string());
                                println!(
                                    "  [🧠] [{}/{}] [w{}] {} '{}' — no entities (marked processed) in {:?} | ETA {}",
                                    done, total, worker_id, file_label, task.title, elapsed, eta
                                );
                            } else {
                                println!(
                                    "  [🧠] [{}/{}] [w{}] {} '{}' — {} entities in {:?} | ETA {} : {:?}",
                                    done, total, worker_id, file_label, task.title, ents.len(), elapsed, eta, ents
                                );
                            }
                            if let Some((_, sections)) = cached_files.get_mut(&task.file_path) {
                                if let Some(sec) = sections.get_mut(task.sec_idx) {
                                    sec.entities = ents;
                                }
                            }

                            // Checkpoint state.json (preserving the semantic
                            // session id) so an interrupted run resumes from
                            // the last completed chunk.
                            let temp_state = IndexState {
                                target_dir: target_dir.to_string(),
                                db_dir: db_dir.to_string(),
                                semantic_enabled,
                                ollama_entities,
                                ollama_model: ollama_model.clone(),
                                ollama_url: ollama_url.clone(),
                                tag_dict_path: tag_dict_path.clone(),
                                semantic_session_id: semantic_session_id.clone(),
                                cached_files: cached_files.clone(),
                            };
                            let _ = save_json(&db_path.join("state.json"), &temp_state);

                            // Periodically rewrite the searchable indexes so
                            // long extraction runs can be queried mid-flight.
                            if last_flush.elapsed() >= FLUSH_INTERVAL {
                                match flush_searchable_indexes(&cached_files, tagger.as_ref(), &tagger_phrases, db_path) {
                                    Ok(n) => println!("  [💾] Searchable index flushed mid-run ({} sections)", n),
                                    Err(e) => eprintln!("  [⚠️] Mid-run index flush failed: {}", e),
                                }
                                last_flush = Instant::now();
                            }
                        }
                        Err(err) => {
                            fail_count += 1;
                            println!(
                                "  [🧠] [{}/{}] [w{}] {} '{}' — Failed in {:?} | ETA {} : {}",
                                done, total, worker_id, file_label, task.title, elapsed, eta, err
                            );
                        }
                    }
                }
            });

            let elapsed_all = ollama_start_all.elapsed();
            let rate = if elapsed_all.as_secs_f64() > 0.0 {
                ok_count as f64 / elapsed_all.as_secs_f64()
            } else {
                0.0
            };
            println!(
                "[🕒] Entity extraction: {} extracted, {} cached, {} failed in {:?} ({:.2} chunks/s, {} workers)",
                ok_count, skipped, fail_count, elapsed_all, rate, num_workers
            );
        } else {
            println!("[🕒] Entity extraction: all {} eligible chunks already cached.", skipped);
        }
    }

    let save_start = Instant::now();
    let section_count = flush_searchable_indexes(&cached_files, tagger.as_ref(), &tagger_phrases, db_path)?;

    let state = IndexState {
        target_dir: target_dir.to_string(),
        db_dir: db_dir.to_string(),
        semantic_enabled,
        ollama_entities,
        ollama_model,
        ollama_url,
        tag_dict_path,
        semantic_session_id,
        cached_files,
    };
    save_json(&db_path.join("state.json"), &state)?;
    println!("[💾] Index files written to {} ({} sections) in {:?}", db_dir, section_count, save_start.elapsed());

    println!("[⏱️] Total indexing job completed in {:?}", total_start.elapsed());
    Ok(())
}

fn correct_query(spelling: &SpellIndex, query: &str) -> String {
    let mut words = Vec::new();
    for word in query.split_whitespace() {
        let clean: String = word.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase();
        if clean.is_empty() {
            words.push(word.to_string());
            continue;
        }
        if spelling.vocab_set.contains(&clean) {
            words.push(word.to_string());
        } else {
            let suggestions = spelling.correct_word(&clean, 1);
            if let Some((best, _)) = suggestions.first() {
                words.push(best.clone());
            } else {
                words.push(word.to_string());
            }
        }
    }
    words.join(" ")
}

fn handle_search(args: &[String]) -> Result<(), String> {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print_search_help();
        return Ok(());
    }

    let mut db_dir = String::from(".lume-index");
    let mut spell_check = false;
    let mut limit = 10usize;
    let mut alpha = std::env::var("ALPHA")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .unwrap_or(0.5f32);
    let mut graph_beta: Option<f64> = None;
    // SKG edge scoring: significance (default) vs legacy Jaccard.
    let mut use_relatedness = true;
    let mut query_opt: Option<String> = None;

    let mut idx = 0;
    while idx < args.len() {
        let arg = &args[idx];
        if arg == "-c" || arg == "--spell-check" {
            spell_check = true;
            idx += 1;
        } else if arg == "--db" && idx + 1 < args.len() {
            db_dir = args[idx + 1].clone();
            idx += 2;
        } else if (arg == "-l" || arg == "--limit") && idx + 1 < args.len() {
            limit = args[idx + 1].parse::<usize>().map_err(|_| format!("Invalid limit: {}", args[idx + 1]))?;
            idx += 2;
        } else if (arg == "-a" || arg == "--alpha") && idx + 1 < args.len() {
            alpha = args[idx + 1].parse::<f32>().map_err(|_| format!("Invalid alpha: {}", args[idx + 1]))?;
            idx += 2;
        } else if (arg == "-g" || arg == "--graph") && idx + 1 < args.len() {
            graph_beta = Some(args[idx + 1].parse::<f64>().map_err(|_| format!("Invalid graph weight: {}", args[idx + 1]))?);
            idx += 2;
        } else if arg == "--scoring" && idx + 1 < args.len() {
            use_relatedness = match args[idx + 1].to_lowercase().as_str() {
                "relatedness" | "significance" | "skg" => true,
                "jaccard" | "overlap" => false,
                other => return Err(format!("Invalid --scoring '{}': expected 'relatedness' or 'jaccard'", other)),
            };
            idx += 2;
        } else if arg.starts_with('-') {
            return Err(format!("Unknown option: {}", arg));
        } else {
            if query_opt.is_some() {
                return Err(format!("Too many search queries specified: {}", arg));
            }
            query_opt = Some(arg.clone());
            idx += 1;
        }
    }

    let query = query_opt.ok_or_else(|| String::from("Missing search query"))?;

    let db_path = Path::new(&db_dir);
    let state_file_path = db_path.join("state.json");
    if !state_file_path.exists() {
        return Err(format!("Index state file not found at {}. Index the directory first.", state_file_path.display()));
    }
    // Session/semantic caches live with the index, not in the process cwd.
    lume::hybrid::set_cache_dir(db_path);

    let state: IndexState = load_json(&state_file_path)?;
    let bm25: Bm25Index = load_json(&db_path.join("bm25.json"))?;
    let spelling: SpellIndex = load_json(&db_path.join("spelling.json"))?;

    println!(
        "Searching corpus: {} ({} sections, db: {})",
        state.target_dir, bm25.sections.len(), db_dir
    );

    let mut corrected_query = query.clone();
    if spell_check {
        corrected_query = correct_query(&spelling, &query);
        if corrected_query != query {
            println!("Corrected query to: {}", corrected_query);
        }
    }

    // SKG graph boost (Primitive 6 → 7): resolve the query's entities, walk the
    // co-occurrence graph to their neighbors, and produce a per-section boost
    // shared by both the lexical and hybrid paths. `beta = 0` disables it and
    // reproduces the original ranking. Runs locally — no token needed.
    let beta: f64 = match graph_beta {
        Some(v) => v,
        None => std::env::var("GRAPH_ALPHA").ok().and_then(|s| s.parse().ok()).unwrap_or(0.4),
    };
    let skg_scores = compute_skg_for_search(&bm25, db_path, &corrected_query, beta, use_relatedness);

    let token_opt = lume::hybrid::load_nuts_token();
    // Tell the caller explicitly when the semantic leg can't engage — an MCP
    // client asking for alpha > 0 must be able to see it got lexical-only.
    if alpha > 0.0 && state.semantic_session_id.is_none() {
        println!("[⚠️] Semantic search unavailable for this index (no semantic session — index with -s); falling back to lexical BM25.");
    } else if alpha > 0.0 && token_opt.is_none() {
        println!("[⚠️] Semantic search unavailable (no NUTS_SERVICES_TOKEN and shivvr endpoint is not local); falling back to lexical BM25.");
    }
    if let (Some(_sess_id), Some(_token)) = (&state.semantic_session_id, token_opt) {
        if alpha > 0.0 {
            println!("Executing hybrid search (alpha={}, graph={})...", alpha, beta);
            let mut tagger = None;
            if let Some(ref tag_dict) = state.tag_dict_path {
                let tag_dict_p = Path::new(tag_dict);
                if tag_dict_p.exists() {
                    tagger = load_tagger_csv(tag_dict_p).ok();
                }
            }

            // Set environment variables for the execute_hybrid_search config lookup
            std::env::set_var("ALPHA", alpha.to_string());

            match lume::hybrid::execute_hybrid_search(
                &bm25,
                tagger.as_ref(),
                &state.target_dir,
                &corrected_query,
                &skg_scores,
                beta,
            ) {
                Ok(mut results) => {
                    results.hits.truncate(limit);
                    print_hybrid_results(&results, &corrected_query);
                    return Ok(());
                }
                Err(err) => {
                    eprintln!("Warning: Semantic hybrid search failed ({}), falling back to lexical BM25.", err);
                }
            }
        }
    }

    println!("Executing lexical BM25 search (graph={})...", beta);
    let params = Bm25Params::default();
    let variant = SearchVariant::Classic;
    let mut tagger = None;
    if let Some(ref tag_dict) = state.tag_dict_path {
        let tag_dict_p = Path::new(tag_dict);
        if tag_dict_p.exists() {
            tagger = load_tagger_csv(tag_dict_p).ok();
        }
    }
    let mut hits = bm25.search(&corrected_query, variant, &params, tagger.as_ref());
    lume::graph_search::apply_skg_boost(&mut hits, &skg_scores, beta);
    hits.truncate(limit);
    print_lexical_hits(&hits, &bm25, &corrected_query, &skg_scores);

    Ok(())
}

/// `lume eval <qna.json>` — measures retrieval quality (Hit@k, MRR, nDCG@k) of
/// the lexical BM25 + SKG-graph pipeline against a Q&A file. Relevance is judged
/// by answer-token containment (see `lume::eval`). `--compare` runs both SKG
/// scoring modes so the significance-vs-Jaccard delta is visible.
fn handle_eval(args: &[String]) -> Result<(), String> {
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        print_eval_help();
        return Ok(());
    }

    let mut db_dir = String::from(".lume-index");
    let mut k = 10usize;
    let mut beta = 0.4f64;
    let mut threshold = 0.5f64;
    let mut use_relatedness = true;
    let mut compare = false;
    let mut spell_check = false;
    let mut max_questions: Option<usize> = None;
    let mut qna_path_opt: Option<String> = None;

    let mut idx = 0;
    while idx < args.len() {
        let arg = &args[idx];
        match arg.as_str() {
            "--db" if idx + 1 < args.len() => { db_dir = args[idx + 1].clone(); idx += 2; }
            "-k" | "--limit" if idx + 1 < args.len() => {
                k = args[idx + 1].parse().map_err(|_| format!("Invalid limit: {}", args[idx + 1]))?; idx += 2;
            }
            "-g" | "--graph" if idx + 1 < args.len() => {
                beta = args[idx + 1].parse().map_err(|_| format!("Invalid graph weight: {}", args[idx + 1]))?; idx += 2;
            }
            "-t" | "--threshold" if idx + 1 < args.len() => {
                threshold = args[idx + 1].parse().map_err(|_| format!("Invalid threshold: {}", args[idx + 1]))?; idx += 2;
            }
            "-n" | "--max-questions" if idx + 1 < args.len() => {
                max_questions = Some(args[idx + 1].parse().map_err(|_| format!("Invalid count: {}", args[idx + 1]))?); idx += 2;
            }
            "--scoring" if idx + 1 < args.len() => {
                use_relatedness = match args[idx + 1].to_lowercase().as_str() {
                    "relatedness" | "significance" | "skg" => true,
                    "jaccard" | "overlap" => false,
                    other => return Err(format!("Invalid --scoring '{}': expected 'relatedness' or 'jaccard'", other)),
                };
                idx += 2;
            }
            "--compare" => { compare = true; idx += 1; }
            "-c" | "--spell-check" => { spell_check = true; idx += 1; }
            other if other.starts_with('-') => return Err(format!("Unknown option: {}", other)),
            _ => {
                if qna_path_opt.is_some() {
                    return Err(format!("Too many positional arguments: {}", arg));
                }
                qna_path_opt = Some(arg.clone());
                idx += 1;
            }
        }
    }

    let qna_path = qna_path_opt.ok_or_else(|| String::from("Missing Q&A file path"))?;

    // Load the Q&A set (UTF-8-tolerant: cp1252 smart quotes won't abort).
    let qna_bytes = std::fs::read(&qna_path).map_err(|e| format!("Failed to read {}: {}", qna_path, e))?;
    let mut questions = lume::eval::parse_qna(&qna_bytes)?;
    if let Some(n) = max_questions {
        questions.truncate(n);
    }

    // Load the index.
    let db_path = Path::new(&db_dir);
    let state_file_path = db_path.join("state.json");
    if !state_file_path.exists() {
        return Err(format!("Index state file not found at {}. Index the corpus first.", state_file_path.display()));
    }
    lume::hybrid::set_cache_dir(db_path);
    let state: IndexState = load_json(&state_file_path)?;
    let bm25: Bm25Index = load_json(&db_path.join("bm25.json"))?;
    let spelling: SpellIndex = load_json(&db_path.join("spelling.json"))?;
    let graph: Option<EntityGraph> = load_json(&db_path.join("entity_graph.json")).ok();

    let mut tagger = None;
    if let Some(ref tag_dict) = state.tag_dict_path {
        let p = Path::new(tag_dict);
        if p.exists() {
            tagger = load_tagger_csv(p).ok();
        }
    }

    let graph_edges = graph.as_ref().map(|g| g.edges.len()).unwrap_or(0);
    println!("\n\x1B[1;36m═══ Lume Retrieval Evaluation ═══\x1B[0m");
    println!("  Corpus      : {} ({} sections)", state.target_dir, bm25.sections.len());
    println!("  Q&A file    : {} ({} questions)", qna_path, questions.len());
    println!("  Graph       : {} edges  |  graph β = {}", graph_edges, beta);
    println!("  Relevance   : answer-token recall ≥ {:.2}  |  metrics @{}", threshold, k);

    let run = |use_rel: bool| -> lume::eval::EvalAggregate {
        run_eval_pass(&bm25, graph.as_ref(), &spelling, tagger.as_ref(),
            &questions, k, beta, use_rel, threshold, spell_check)
    };

    if compare {
        let jac = run(false);
        let rel = run(true);
        print_eval_compare(&jac, &rel, k);
    } else {
        let agg = run(use_relatedness);
        let mode = if use_relatedness { "relatedness (significance)" } else { "jaccard (overlap)" };
        print_eval_report(&agg, k, mode);
    }
    Ok(())
}

/// Runs one evaluation pass over all questions with a fixed SKG scoring mode and
/// returns the aggregated metrics. Pure lexical BM25 + SKG boost — deterministic
/// and fully local, which is exactly the leg where edge scoring matters.
#[allow(clippy::too_many_arguments)]
fn run_eval_pass(
    bm25: &Bm25Index,
    graph: Option<&EntityGraph>,
    spelling: &SpellIndex,
    tagger: Option<&Tagger>,
    questions: &[lume::eval::QnaItem],
    k: usize,
    beta: f64,
    use_relatedness: bool,
    threshold: f64,
    spell_check: bool,
) -> lume::eval::EvalAggregate {
    let params = Bm25Params::default();
    let skg_params = lume::graph_search::SkgBoostParams { beta, use_relatedness, ..Default::default() };
    let mut agg = lume::eval::EvalAggregate::new(k);

    for q in questions {
        let query = if spell_check { correct_query(spelling, &q.question) } else { q.question.clone() };

        // SKG boost from the question's entities (no-op when β=0 or no graph).
        let skg_scores = match graph {
            Some(g) if beta > 0.0 => {
                lume::graph_search::compute_skg_scores(bm25, g, &query, &skg_params).scores
            }
            _ => std::collections::HashMap::new(),
        };

        let mut hits = bm25.search(&query, SearchVariant::Classic, &params, tagger);
        lume::graph_search::apply_skg_boost(&mut hits, &skg_scores, beta);
        hits.truncate(k);

        // Judge each retrieved section by answer-token containment. A question is
        // skipped (not counted) only when its answer has no content tokens at all.
        let mut rels: Vec<bool> = Vec::with_capacity(hits.len());
        let mut judgeable = false;
        for h in &hits {
            if let Some(sec) = bm25.sections.get(h.section_index) {
                match lume::eval::is_relevant(&q.answer, &sec.body, threshold) {
                    Some(r) => { judgeable = true; rels.push(r); }
                    None => { rels.clear(); break; }
                }
            }
        }
        agg.record(if judgeable { Some(rels.as_slice()) } else { None });
    }
    agg
}

fn print_eval_report(agg: &lume::eval::EvalAggregate, k: usize, mode: &str) {
    println!("\n  \x1B[1mScoring mode: {}\x1B[0m", mode);
    println!("  ┌──────────────┬─────────┐");
    println!("  │ Metric       │  Value  │");
    println!("  ├──────────────┼─────────┤");
    println!("  │ Questions    │ {:>7} │", agg.judged);
    println!("  │ Hit@{:<8} │ {:>6.1}% │", k, agg.hit_rate() * 100.0);
    println!("  │ MRR          │ {:>7.4} │", agg.mrr());
    println!("  │ nDCG@{:<7} │ {:>7.4} │", k, agg.ndcg());
    println!("  └──────────────┴─────────┘");
    if agg.skipped > 0 {
        println!("  ({} questions skipped — answer had no scorable content tokens)", agg.skipped);
    }
}

fn print_eval_compare(jac: &lume::eval::EvalAggregate, rel: &lume::eval::EvalAggregate, k: usize) {
    let d = |a: f64, b: f64| {
        let delta = b - a;
        let color = if delta > 0.0 { "\x1B[32m" } else if delta < 0.0 { "\x1B[31m" } else { "\x1B[0m" };
        format!("{}{:+.4}\x1B[0m", color, delta)
    };
    println!("\n  \x1B[1mScoring comparison ({} questions judged)\x1B[0m", rel.judged);
    println!("  ┌──────────────┬──────────┬──────────────┬───────────┐");
    println!("  │ Metric       │  Jaccard │ Relatedness  │   Δ       │");
    println!("  ├──────────────┼──────────┼──────────────┼───────────┤");
    println!("  │ Hit@{:<8} │ {:>7.1}% │ {:>11.1}% │ {} │", k, jac.hit_rate() * 100.0, rel.hit_rate() * 100.0, d(jac.hit_rate(), rel.hit_rate()));
    println!("  │ MRR          │ {:>8.4} │ {:>12.4} │ {} │", jac.mrr(), rel.mrr(), d(jac.mrr(), rel.mrr()));
    println!("  │ nDCG@{:<7} │ {:>8.4} │ {:>12.4} │ {} │", k, jac.ndcg(), rel.ndcg(), d(jac.ndcg(), rel.ndcg()));
    println!("  └──────────────┴──────────┴──────────────┴───────────┘");
    println!("  Δ = relatedness − jaccard (positive favors significance scoring).");
}

/// `lume stream <query>` — streams the phase-binding + Weber relaxation over the
/// query's top-K candidates as NDJSON frames on stdout (one per step). Diagnostics
/// go to stderr so stdout stays a clean frame stream for the viz bridge.
fn handle_stream(args: &[String]) -> Result<(), String> {
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        print_stream_help();
        return Ok(());
    }

    let mut db_dir = String::from(".lume-index");
    let mut candidates = 24usize;
    let mut steps = 160usize;
    let mut beta = 0.4f64;
    let mut queries: Vec<String> = Vec::new();

    let mut idx = 0;
    while idx < args.len() {
        let arg = &args[idx];
        match arg.as_str() {
            "--db" if idx + 1 < args.len() => { db_dir = args[idx + 1].clone(); idx += 2; }
            "-k" | "--candidates" if idx + 1 < args.len() => {
                candidates = args[idx + 1].parse().map_err(|_| format!("Invalid candidates: {}", args[idx + 1]))?; idx += 2;
            }
            "--steps" if idx + 1 < args.len() => {
                steps = args[idx + 1].parse().map_err(|_| format!("Invalid steps: {}", args[idx + 1]))?; idx += 2;
            }
            "-g" | "--graph" if idx + 1 < args.len() => {
                beta = args[idx + 1].parse().map_err(|_| format!("Invalid graph weight: {}", args[idx + 1]))?; idx += 2;
            }
            "--add" if idx + 1 < args.len() => { queries.push(args[idx + 1].clone()); idx += 2; }
            other if other.starts_with('-') => return Err(format!("Unknown option: {}", other)),
            _ => { queries.push(arg.clone()); idx += 1; }
        }
    }
    if queries.is_empty() {
        return Err(String::from("Missing search query"));
    }

    let db_path = Path::new(&db_dir);
    let state_file_path = db_path.join("state.json");
    if !state_file_path.exists() {
        return Err(format!("Index state file not found at {}. Index the corpus first.", state_file_path.display()));
    }
    lume::hybrid::set_cache_dir(db_path);
    let _state: IndexState = load_json(&state_file_path)?;
    let bm25: Bm25Index = load_json(&db_path.join("bm25.json"))?;

    // Quiet candidate retrieval per query (BM25 + optional SKG), unioned with
    // overlap membership. Nothing here touches stdout (the NDJSON channel).
    let graph: Option<EntityGraph> = if beta > 0.0 { load_json(&db_path.join("entity_graph.json")).ok() } else { None };
    let cands = retrieve_union(&bm25, graph.as_ref(), beta, &queries, candidates);
    if cands.is_empty() {
        return Err("no candidates retrieved for any query".to_string());
    }

    let sp = lume::stream::StreamParams { steps, candidates, ..Default::default() };
    eprintln!("[stream] queries={:?}  union_candidates={}  steps={}", queries, cands.len(), steps);
    lume::stream::run(&bm25, &queries, &cands, &sp, true)
}

/// Per-query BM25 (+ optional SKG) retrieval, unioned by section id with the set
/// of query indices that surfaced each (the overlap membership). Order is first-
/// seen. Shared by `lume stream` and the `lume answer` loop.
fn retrieve_union(
    bm25: &Bm25Index,
    graph: Option<&EntityGraph>,
    beta: f64,
    queries: &[String],
    candidates: usize,
) -> Vec<lume::stream::Candidate> {
    use std::collections::HashMap;
    let params = Bm25Params::default();
    let mut union: HashMap<usize, (f64, Vec<usize>)> = HashMap::new();
    let mut order: Vec<usize> = Vec::new();
    for (qi, q) in queries.iter().enumerate() {
        let mut hits = bm25.search(q, SearchVariant::Classic, &params, None);
        if let Some(g) = graph {
            if beta > 0.0 {
                let skg = lume::graph_search::SkgBoostParams { beta, ..Default::default() };
                let walk = lume::graph_search::compute_skg_scores(bm25, g, q, &skg);
                lume::graph_search::apply_skg_boost(&mut hits, &walk.scores, beta);
            }
        }
        hits.truncate(candidates);
        for h in &hits {
            let e = union.entry(h.section_index).or_insert_with(|| { order.push(h.section_index); (h.score, Vec::new()) });
            if h.score > e.0 { e.0 = h.score; }
            if !e.1.contains(&qi) { e.1.push(qi); }
        }
    }
    order.iter().map(|sid| {
        let (score, members) = union[sid].clone();
        lume::stream::Candidate { section_id: *sid, score, members }
    }).collect()
}

/// `lume answer <question>` — agentic plan → retrieve → evaluate → refine →
/// answer loop over a local Ollama model, streaming NDJSON events (plan rounds,
/// the relaxation frames, and a cited answer) for the visualizer.
fn handle_answer(args: &[String]) -> Result<(), String> {
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        print_answer_help();
        return Ok(());
    }

    let mut db_dir = String::from(".lume-index");
    let mut candidates = 18usize;
    let mut steps = 140usize;
    let mut beta = 0.4f64;
    let mut max_rounds = 3usize;
    let mut model = String::from("gpt-4o-mini:latest");
    let mut ollama_url = std::env::var("OLLAMA_URL").unwrap_or_else(|_| "http://localhost:11434".to_string());
    let mut words: Vec<String> = Vec::new();

    let mut idx = 0;
    while idx < args.len() {
        let arg = &args[idx];
        match arg.as_str() {
            "--db" if idx + 1 < args.len() => { db_dir = args[idx + 1].clone(); idx += 2; }
            "-k" | "--candidates" if idx + 1 < args.len() => { candidates = args[idx + 1].parse().map_err(|_| "bad -k")?; idx += 2; }
            "--steps" if idx + 1 < args.len() => { steps = args[idx + 1].parse().map_err(|_| "bad --steps")?; idx += 2; }
            "-g" | "--graph" if idx + 1 < args.len() => { beta = args[idx + 1].parse().map_err(|_| "bad -g")?; idx += 2; }
            "--rounds" if idx + 1 < args.len() => { max_rounds = args[idx + 1].parse().map_err(|_| "bad --rounds")?; idx += 2; }
            "--model" if idx + 1 < args.len() => { model = args[idx + 1].clone(); idx += 2; }
            "--ollama-url" if idx + 1 < args.len() => { ollama_url = args[idx + 1].clone(); idx += 2; }
            other if other.starts_with('-') => return Err(format!("Unknown option: {}", other)),
            _ => { words.push(arg.clone()); idx += 1; }
        }
    }
    let question = words.join(" ");
    if question.trim().is_empty() {
        return Err(String::from("Missing question"));
    }

    let db_path = Path::new(&db_dir);
    if !db_path.join("state.json").exists() {
        return Err(format!("Index not found at {}. Index the corpus first.", db_dir));
    }
    lume::hybrid::set_cache_dir(db_path);
    let bm25: Bm25Index = load_json(&db_path.join("bm25.json"))?;
    let graph: Option<EntityGraph> = if beta > 0.0 { load_json(&db_path.join("entity_graph.json")).ok() } else { None };

    let emit = |v: serde_json::Value| println!("{}", v);
    emit(serde_json::json!({ "type": "question", "text": question, "model": model }));
    eprintln!("[answer] question={:?} model={}", question, model);

    // --- Plan ---
    let mut queries = match lume::answer::plan_queries(&ollama_url, &model, &question) {
        Ok(q) => q,
        Err(e) => { eprintln!("[answer] planner failed ({e}); using the question verbatim"); vec![question.clone()] }
    };
    emit(serde_json::json!({ "type": "plan", "round": 1, "queries": queries, "note": "" }));

    let sp = lume::stream::StreamParams { steps, candidates, ..Default::default() };
    // Passages handed to the model. Scale with -k (capped) so raising candidates
    // actually widens what the evaluator/answerer can see, instead of a fixed 10.
    let n_feed = candidates.clamp(10, 20);

    let mut cands: Vec<lume::stream::Candidate> = Vec::new();
    let mut round = 1usize;
    loop {
        cands = retrieve_union(&bm25, graph.as_ref(), beta, &queries, candidates);
        if cands.is_empty() {
            emit(serde_json::json!({ "type": "evaluate", "round": round, "sufficient": false, "note": "no candidates retrieved" }));
            break;
        }
        // Animate this round's field (no terminal "done" — the loop continues).
        lume::stream::run(&bm25, &queries, &cands, &sp, false)?;

        if round >= max_rounds { break; }
        let passages = numbered_passages(&bm25, &cands, n_feed);
        match lume::answer::evaluate(&ollama_url, &model, &question, &passages) {
            Ok(v) => {
                emit(serde_json::json!({ "type": "evaluate", "round": round, "sufficient": v.sufficient, "note": v.note }));
                if v.sufficient || v.queries.is_empty() { break; }
                let mut added = false;
                for q in v.queries {
                    if !queries.iter().any(|e| e.eq_ignore_ascii_case(&q)) { queries.push(q); added = true; }
                }
                if !added { break; }
                round += 1;
                emit(serde_json::json!({ "type": "plan", "round": round, "queries": queries, "note": "refined" }));
            }
            Err(e) => { eprintln!("[answer] evaluate failed ({e}); answering with what we have"); break; }
        }
    }

    // --- Answer over the final field ---
    let nq = queries.len();
    // Feed top-N candidates by score; track marker(1-based) -> node id (= nq + cands index).
    let mut ranked: Vec<usize> = (0..cands.len()).collect();
    ranked.sort_by(|&a, &b| cands[b].score.partial_cmp(&cands[a].score).unwrap_or(std::cmp::Ordering::Equal));
    let fed: Vec<usize> = ranked.into_iter().take(n_feed).collect();
    let mut numbered = String::new();
    for (k, &ci) in fed.iter().enumerate() {
        if let Some(sec) = bm25.sections.get(cands[ci].section_id) {
            let snip: String = sec.body.split_whitespace().take(180).collect::<Vec<_>>().join(" ");
            numbered.push_str(&format!("[{}] {}: {}\n", k + 1, sec.title.trim(), snip));
        }
    }
    let used_ids: Vec<usize> = fed.iter().map(|&ci| nq + ci).collect();

    let answer_text = lume::answer::synthesize(&ollama_url, &model, &question, &numbered)
        .unwrap_or_else(|e| format!("(answer generation failed: {})", e));
    let cite_markers = lume::answer::parse_citations(&answer_text, fed.len());
    let cited_ids: Vec<usize> = cite_markers.iter().map(|&m| nq + fed[m - 1]).collect();

    emit(serde_json::json!({
        "type": "answer", "text": answer_text, "model": model,
        "used": used_ids, "cites": cited_ids,
    }));
    emit(serde_json::json!({ "type": "done" }));
    Ok(())
}

/// Numbered passage block for the evaluator/answerer prompts (top-N by score).
fn numbered_passages(bm25: &Bm25Index, cands: &[lume::stream::Candidate], n: usize) -> String {
    let mut ranked: Vec<usize> = (0..cands.len()).collect();
    ranked.sort_by(|&a, &b| cands[b].score.partial_cmp(&cands[a].score).unwrap_or(std::cmp::Ordering::Equal));
    let mut s = String::new();
    for (k, &ci) in ranked.iter().take(n).enumerate() {
        if let Some(sec) = bm25.sections.get(cands[ci].section_id) {
            let snip: String = sec.body.split_whitespace().take(180).collect::<Vec<_>>().join(" ");
            s.push_str(&format!("[{}] {}: {}\n", k + 1, sec.title.trim(), snip));
        }
    }
    s
}

fn print_answer_help() {
    println!(r#"lume-answer
Agentic question answering over the index: plan search queries, retrieve and
animate the field, evaluate/refine, then synthesize a cited answer with a local
Ollama model. Streams NDJSON (question, plan, evaluate, relaxation frames, answer)
for the 3D visualizer.

USAGE:
  lume answer [OPTIONS] <QUESTION...>

OPTIONS:
  --db <PATH>            Persisted index metadata [default: .lume-index]
  -k, --candidates <N>   Top-N candidates per query [default: 18]
  --steps <N>            Relaxation steps per round [default: 140]
  -g, --graph <VAL>      SKG graph boost weight [default: 0.4]
  --rounds <N>           Max plan/refine rounds [default: 3]
  --model <NAME>         Ollama model [default: gpt-4o-mini:latest]
  --ollama-url <URL>     Ollama endpoint [default: $OLLAMA_URL or http://localhost:11434]
  --shivvr-url <URL>     Shivvr endpoint URL [default: http://localhost:8085]

ARGS:
  <QUESTION...>          The question to answer (remaining args joined)
"#);
}

fn print_stream_help() {
    println!(r#"lume-stream
Stream the live phase-binding + Weber search relaxation as NDJSON frames (one per
step) on stdout, for the 3D vector visualizer. Requires a reachable shivvr
endpoint (used read-only to embed the query and candidates).

USAGE:
  lume stream [OPTIONS] <QUERY> [--add <QUERY> ...]

OPTIONS:
  --db <PATH>            Path to the persisted index metadata [default: .lume-index]
  -k, --candidates <N>   Top-N retrieved candidates per query to animate [default: 24]
  --steps <N>            Relaxation steps (frames) to emit [default: 160]
  -g, --graph <VAL>      SKG graph boost weight for candidate selection [default: 0.4]
  --add <QUERY>          Additional query (additive search); results union into one field
  --shivvr-url <URL>     Shivvr endpoint URL [default: http://localhost:8085]

ARGS:
  <QUERY>                Search query string (more via --add); candidates retrieved by
                         more than one query are flagged as overlaps (members)

Each frame is a JSON object: {{type:"frame", step, r_global, nodes:[{{id, pos[3],
vel[3], acc[3], phase, cos_q, approach_vel, approach_acc, cluster, is_query}}]}}.
A leading {{type:"meta"}} frame carries node labels; a trailing {{type:"done"}}.
"#);
}

/// Loads the SKG graph and walks it for `query`, returning per-section boost
/// scores. Returns an empty map (no boost) when `beta <= 0`, the graph file is
/// missing, or no query entities resolve. Emits a stderr "SKG walk" trace.
fn compute_skg_for_search(
    bm25: &Bm25Index,
    db_path: &Path,
    query: &str,
    beta: f64,
    use_relatedness: bool,
) -> std::collections::HashMap<usize, f64> {
    use std::collections::HashMap;
    if beta <= 0.0 {
        return HashMap::new();
    }
    let graph_path = db_path.join("entity_graph.json");
    if !graph_path.exists() {
        eprintln!("\x1B[35m[SKG] No entity_graph.json found; graph boost disabled (re-run `lume index`).\x1B[0m");
        return HashMap::new();
    }
    let graph: EntityGraph = match load_json(&graph_path) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("\x1B[35m[SKG] Failed to load entity_graph.json ({}); graph boost disabled.\x1B[0m", e);
            return HashMap::new();
        }
    };
    let params = lume::graph_search::SkgBoostParams { beta, use_relatedness, ..Default::default() };
    let walk = lume::graph_search::compute_skg_scores(bm25, &graph, query, &params);
    print_skg_walk(&walk, bm25);
    walk.scores
}

/// Prints the SKG traversal (resolved seed entities → their strongest
/// neighbors) to stderr, using display labels. This is the "walk the graph from
/// Mercédès" trace made visible.
fn print_skg_walk(walk: &lume::graph_search::SkgWalk, bm25: &Bm25Index) {
    let label = |k: &str| bm25.entity_labels.get(k).cloned().unwrap_or_else(|| k.to_string());
    if walk.seeds.is_empty() {
        println!("[SKG walk] no query entities resolved — no graph boost");
        return;
    }
    let seeds: Vec<String> = walk.seeds.iter().map(|s| label(s)).collect();
    let neighbors: Vec<String> = walk
        .expanded
        .iter()
        .take(8)
        .map(|(k, w)| format!("{} ({:.2})", label(k), w))
        .collect();
    println!(
        "[SKG walk] seeds: {} → neighbors: {}",
        seeds.join(", "),
        if neighbors.is_empty() { "(none)".to_string() } else { neighbors.join(", ") }
    );
}

/// Builds a short snippet centered on the line containing the most distinct
/// query terms, with one line of surrounding context. Falls back to the first
/// non-blank lines when no query term is found in the body. This replaces the
/// old "first three lines" snippet, which never showed *why* a section matched.
fn best_snippet(body: &str, query: &str) -> String {
    if body.chars().count() <= 6000 {
        return body.trim().to_string();
    }

    use std::collections::HashSet;
    let q_tokens = lume::bm25::filter_query_stopwords(lume::tokenize(query));
    let qset: HashSet<Vec<u8>> = q_tokens.into_iter().map(|t| t.bytes).collect();

    let lines: Vec<&str> = body.lines().collect();
    if lines.is_empty() {
        return String::new();
    }

    let mut best_idx = 0usize;
    let mut best_score = 0usize;
    for (i, line) in lines.iter().enumerate() {
        let mut seen: HashSet<&Vec<u8>> = HashSet::new();
        for t in lume::tokenize(line) {
            if let Some(k) = qset.get(&t.bytes) {
                seen.insert(k);
            }
        }
        if seen.len() > best_score {
            best_score = seen.len();
            best_idx = i;
        }
    }

    if best_score == 0 {
        return lines
            .iter()
            .filter(|l| !l.trim().is_empty())
            .take(15)
            .copied()
            .collect::<Vec<_>>()
            .join("\n");
    }

    let start = best_idx.saturating_sub(35);
    let end = (best_idx + 45).min(lines.len());
    let snippet = lines[start..end].join("\n");
    let trimmed = snippet.trim();
    if trimmed.chars().count() > 6000 {
        let capped: String = trimmed.chars().take(6000).collect();
        format!("{}…", capped)
    } else {
        trimmed.to_string()
    }
}

fn print_lexical_hits(
    hits: &[SearchHit],
    bm25: &Bm25Index,
    query: &str,
    skg_scores: &std::collections::HashMap<usize, f64>,
) {
    if hits.is_empty() {
        println!("No hits found.");
        return;
    }
    for (i, hit) in hits.iter().enumerate() {
        if let Some(sec) = bm25.sections.get(hit.section_index) {
            let filename = sec.filename.as_deref().unwrap_or("unknown");
            let skg_tag = match skg_scores.get(&hit.section_index) {
                Some(s) if *s > 0.0 => format!(" [SKG: {:.2}]", s),
                _ => String::new(),
            };
            println!(
                "[{}] Score: {:.4}{} | {} (File: {}, Line: {})",
                i + 1,
                hit.score,
                skg_tag,
                sec.title,
                filename,
                sec.line_number
            );
            let filtered_entities: Vec<&str> = sec.entities.iter()
                .map(|e| e.as_str())
                .filter(|&e| e != "__LUME_PROCESSED__")
                .collect();
            if !filtered_entities.is_empty() {
                println!("  Entities: {:?}", filtered_entities);
            }
            let snippet = best_snippet(&sec.body, query);
            println!("{}\n", snippet);
        }
    }
}

fn print_hybrid_results(results: &lume::hybrid::HybridSearchResult, query: &str) {
    if results.hits.is_empty() {
        println!("No hybrid hits found.");
        return;
    }
    for hit in &results.hits {
        let filename = hit.filename.as_deref().unwrap_or("unknown");
        println!(
            "[{}] Hybrid Score: {:.4} (BM25: {:.4}, Semantic: {:.4}, SKG: {:.2}) | {} (File: {}, Line: {})",
            hit.rank,
            hit.hybrid_score,
            hit.bm25_score,
            hit.semantic_score,
            hit.skg_score,
            hit.title,
            filename,
            hit.line_number
        );
        let snippet = best_snippet(&hit.body, query);
        println!("{}\n", snippet);
    }
}

fn print_generate_help() {
    println!(r#"lume-generate
Generate style-faithful text from the indexed corpus without an LLM.

USAGE:
  lume generate [OPTIONS] [SEED]

OPTIONS:
  --db <PATH>            Path to the persisted index metadata [default: .lume-index]
  -l, --limit <LIMIT>    Max number of tokens to generate [default: 100]
  --tokens <LIMIT>       Alias for --limit
  --steer <TAGS>         Comma-separated list of tags/entities to steer generation towards
  --attempts <N>         Vector mode: candidates to try, keeping the best match [default: 6]
  --threshold <F>        Vector mode: accept a candidate at this cosine match [default: 0.75]
  --shivvr-url <URL>     Shivvr endpoint URL [default: http://localhost:8085]

ARGS:
  [SEED]                 Seed/target prompt. With a shivvr token set, generation
                         runs the inversion-steered loop: embed target → invert
                         to seed concepts → generate, score by embedding match,
                         re-steer with graph neighbors until it round-trips.
"#);
}

fn handle_generate(args: &[String]) -> Result<(), String> {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print_generate_help();
        return Ok(());
    }

    let mut db_dir = String::from(".lume-index");
    let mut limit = 100usize;
    let mut steer_tags = Vec::new();
    let mut seed_word: Option<String> = None;
    let mut attempts = 6usize;
    let mut threshold = 0.75f64;

    let mut idx = 0;
    while idx < args.len() {
        let arg = &args[idx];
        if arg == "--db" && idx + 1 < args.len() {
            db_dir = args[idx + 1].clone();
            idx += 2;
        } else if (arg == "-l" || arg == "--limit" || arg == "--tokens") && idx + 1 < args.len() {
            limit = args[idx + 1].parse::<usize>().map_err(|_| format!("Invalid limit: {}", args[idx + 1]))?;
            idx += 2;
        } else if arg == "--steer" && idx + 1 < args.len() {
            let val = &args[idx + 1];
            steer_tags = val.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            idx += 2;
        } else if arg == "--attempts" && idx + 1 < args.len() {
            attempts = args[idx + 1].parse::<usize>().map_err(|_| format!("Invalid attempts: {}", args[idx + 1]))?.max(1);
            idx += 2;
        } else if arg == "--threshold" && idx + 1 < args.len() {
            threshold = args[idx + 1].parse::<f64>().map_err(|_| format!("Invalid threshold: {}", args[idx + 1]))?;
            idx += 2;
        } else if arg.starts_with('-') {
            return Err(format!("Unknown option: {}", arg));
        } else {
            if seed_word.is_some() {
                return Err(format!("Too many seed words specified: {}", arg));
            }
            seed_word = Some(arg.clone());
            idx += 1;
        }
    }

    let db_path = Path::new(&db_dir);
    let state_file_path = db_path.join("state.json");
    if !state_file_path.exists() {
        return Err(format!("Index state file not found at {}. Index the directory first.", state_file_path.display()));
    }

    let state: IndexState = load_json(&state_file_path)?;
    let bm25: Bm25Index = load_json(&db_path.join("bm25.json"))?;

    let mut tagger = None;
    if let Some(ref tag_dict) = state.tag_dict_path {
        let tag_dict_p = Path::new(tag_dict);
        if tag_dict_p.exists() {
            tagger = load_tagger_csv(tag_dict_p).ok();
        }
    }

    println!("[🧠] Building Markov Chain from index corpus...");
    let bodies: Vec<&str> = bm25.sections.iter().map(|s| s.body.as_str()).collect();
    if bodies.is_empty() {
        return Err(String::from("No sections found in BM25 index to generate text."));
    }

    let chain = lume::semantic_mesh::MarkovChain::build(&bodies);

    // Vector/inversion-steered mode: when a shivvr token is available and we
    // have a target (the seed/prompt), generate candidates and keep the one
    // whose 768-d GTR-T5 embedding best matches the target's — re-steering with
    // SKG-graph neighbors between attempts. Falls back to plain Markov when no
    // token or no seed is present.
    let token = lume::hybrid::load_nuts_token();
    if let (Some(token), Some(target)) = (token, seed_word.as_deref()) {
        match run_inversion_steered_generate(
            &chain, &bm25, db_path, tagger.as_ref(), target, &steer_tags, limit, attempts, threshold, &token,
        ) {
            Ok(()) => return Ok(()),
            Err(e) => eprintln!("[⚠️] Vector-steered generation unavailable ({}); using plain Markov.", e),
        }
    }

    println!("[🧠] Generating steered synthesis (max {} tokens, steer={:?})...", limit, steer_tags);
    let (simulated, _history) = chain.generate_steered(
        seed_word.as_deref(),
        limit,
        tagger.as_ref(),
        &bm25.entity_posting_lists,
        &bm25.posting_lists,
        &steer_tags,
    );

    println!("\n--- Generated Text ---");
    println!("{}", simulated);
    println!("----------------------\n");

    Ok(())
}

/// Inversion-steered generation loop: embed the target → invert it to seed the
/// steer set with the target's concepts → generate candidates and score each by
/// cosine to the target embedding → keep the best, expanding the steer set with
/// SKG-graph neighbors until the round-trip is "good enough" (>= threshold).
#[allow(clippy::too_many_arguments)]
fn run_inversion_steered_generate(
    chain: &lume::semantic_mesh::MarkovChain,
    bm25: &Bm25Index,
    db_path: &Path,
    tagger: Option<&Tagger>,
    target: &str,
    base_steer: &[String],
    limit: usize,
    attempts: usize,
    threshold: f64,
    token: &str,
) -> Result<(), String> {
    use lume::hybrid::{embed_text, cosine_similarity};

    println!("[🔄] Embedding target \"{}\" (768-d GTR-T5)...", target);
    let target_vec = embed_text(target, token)?;

    // Invert the target to recover its concepts in the corpus's own words, and
    // fold those into the steer set.
    let mut steer: Vec<String> = base_steer.to_vec();
    if let Ok(inv) = lume::inversion::invert_vector(&target_vec, Some(48), token) {
        println!("[🔄] Target inverts to: \"{}\" (self-similarity {:.3})", inv.text.trim(), inv.similarity);
        for tok in lume::bm25::filter_query_stopwords(lume::tokenize(&inv.text)) {
            let w = String::from_utf8_lossy(&tok.bytes).to_string();
            if w.len() > 2 && !steer.iter().any(|s| s.eq_ignore_ascii_case(&w)) {
                steer.push(w);
            }
        }
    }

    // Build an ordered pool of SKG-graph neighbors of the target's entities to
    // mutate the steer set across attempts ("steer with other words").
    let mut neighbor_pool: Vec<String> = Vec::new();
    if let Ok(graph) = load_json::<EntityGraph>(&db_path.join("entity_graph.json")) {
        let walk = lume::graph_search::compute_skg_scores(
            bm25, &graph, target, &lume::graph_search::SkgBoostParams::default(),
        );
        for (key, _w) in walk.expanded {
            let label = bm25.entity_labels.get(&key).cloned().unwrap_or(key);
            if !neighbor_pool.contains(&label) {
                neighbor_pool.push(label);
            }
        }
    }

    let mut best_text = String::new();
    let mut best_score = -1.0f64;

    for attempt in 1..=attempts {
        // Expand the steer set with one more graph neighbor each round.
        let mut this_steer = steer.clone();
        for n in neighbor_pool.iter().take(attempt.saturating_sub(1)) {
            if !this_steer.iter().any(|s| s.eq_ignore_ascii_case(n)) {
                this_steer.push(n.clone());
            }
        }

        let (cand, _hist) = chain.generate_steered(
            Some(target), limit, tagger, &bm25.entity_posting_lists, &bm25.posting_lists, &this_steer,
        );
        let cand_vec = embed_text(&cand, token)?;
        let score = cosine_similarity(&cand_vec, &target_vec);
        println!("  [🎯] attempt {}/{}: match {:.3}{}", attempt, attempts, score,
            if score > best_score { " (best)" } else { "" });
        if score > best_score {
            best_score = score;
            best_text = cand;
        }
        if best_score >= threshold {
            break;
        }
    }

    let band = if best_score >= 0.95 { "🟢 lossless" }
        else if best_score >= 0.75 { "🔵 faithful" }
        else if best_score >= 0.50 { "🟡 concept-related" }
        else { "🔴 low / off-target" };

    println!("\n--- Generated Text (match {:.3} — {}) ---", best_score, band);
    println!("{}", best_text);
    println!("----------------------\n");
    Ok(())
}

fn handle_summarize(args: &[String]) -> Result<(), String> {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print_summarize_help();
        return Ok(());
    }

    let mut db_dir = String::from(".lume-index");
    let mut ollama_url = String::from("http://localhost:11434");
    let mut ollama_model = String::from("gemma4:31b-cloud");
    let mut queries = 4usize;
    let mut hits_per_query = 8usize;
    let mut verbose = false;
    let mut target_file: Option<String> = None;

    let mut idx = 0;
    while idx < args.len() {
        let arg = &args[idx];
        if arg == "--db" && idx + 1 < args.len() {
            db_dir = args[idx + 1].clone();
            idx += 2;
        } else if arg == "--ollama-url" && idx + 1 < args.len() {
            ollama_url = args[idx + 1].clone();
            idx += 2;
        } else if arg == "--ollama-model" && idx + 1 < args.len() {
            ollama_model = args[idx + 1].clone();
            idx += 2;
        } else if arg == "--queries" && idx + 1 < args.len() {
            queries = args[idx + 1].parse::<usize>().map_err(|_| format!("Invalid queries count: {}", args[idx + 1]))?;
            idx += 2;
        } else if arg == "--hits-per-query" && idx + 1 < args.len() {
            hits_per_query = args[idx + 1].parse::<usize>().map_err(|_| format!("Invalid hits per query: {}", args[idx + 1]))?;
            idx += 2;
        } else if arg == "-v" || arg == "-V" || arg == "--verbose" {
            verbose = true;
            idx += 1;
        } else if arg.starts_with('-') {
            return Err(format!("Unknown option: {}", arg));
        } else {
            if target_file.is_some() {
                return Err(format!("Too many files specified: {}", arg));
            }
            target_file = Some(arg.clone());
            idx += 1;
        }
    }

    lume::agent::summarize_document(
        &db_dir,
        &ollama_url,
        &ollama_model,
        target_file.as_deref(),
        queries,
        hits_per_query,
        verbose,
    )
}

fn print_summarize_help() {
    println!(r#"Lume Agentic Document Summarizer

USAGE:
  lume summarize [OPTIONS] [FILE]

OPTIONS:
  --db <DIR>                Path to the persisted index metadata [default: .lume-index]
  --ollama-url <URL>        Ollama API URL [default: http://localhost:11434]
  --ollama-model <MODEL>    Ollama model name [default: gemma4:31b-cloud]
  --queries <NUM>           Number of distinct search queries to plan [default: 4]
  --hits-per-query <NUM>    Number of snippets to retrieve per query [default: 8]
  -v, --verbose             Print verbose execution traces

If FILE is omitted, Lume will summarize the largest file in the index database.
"#);
}

fn print_serve_help() {
    println!(r#"lume serve
Start the Model Context Protocol (MCP) server over HTTP.

USAGE:
  lume serve [OPTIONS]

OPTIONS:
  -p, --port <PORT>      Port to bind the HTTP server to [default: 5863 — "LUME" on a phone keypad]
  -h, --help             Prints help information
"#);
}

fn print_agent_help() {
    println!(r#"lume agent
Run a stateful autonomous research agent loop to resolve a question.

USAGE:
  lume agent [OPTIONS] <QUESTION>

OPTIONS:
  --db <DIR>                Path to the persisted index database [default: .lume-index]
  --ollama-url <URL>        Ollama API URL [default: http://localhost:11434]
  --ollama-model <MODEL>    Ollama model name [default: gemma4:31b-cloud]
  -v, --verbose             Print verbose reasoning and tool logs
  -h, --help                Prints help information

ARGS:
  <QUESTION>                The query or question to research
"#);
}

fn print_crawl_help() {
    println!(r#"lume crawl
Stealth crawl webpage content and save it to the personal search collection.

USAGE:
  lume crawl [FLAGS] <URL>

FLAGS:
  -h, --help             Prints help information

ARGS:
  <URL>                  The webpage URL (or Hacker News story URL) to crawl
"#);
}
