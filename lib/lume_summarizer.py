# /// script
# dependencies = [
#     "requests",
# ]
# ///

import os
import sys
import json
import re
import time
import argparse
import subprocess
from pathlib import Path
import requests

def get_ollama_url():
    """Tries to find a running Ollama service on standard local endpoints."""
    urls = ["http://host.docker.internal:11434", "http://localhost:11434", "http://172.17.0.1:11434"]
    for url in urls:
        try:
            response = requests.get(f"{url}/api/tags", timeout=2)
            if response.status_code == 200:
                return url
        except Exception:
            continue
    return "http://localhost:11434"

def extract_json_block(text):
    """Extracts the first matching JSON block from text."""
    text = text.strip()
    first_bracket = text.find('[')
    last_bracket = text.rfind(']')
    if first_bracket != -1 and last_bracket != -1 and last_bracket > first_bracket:
        return text[first_bracket:last_bracket+1]
    
    first_brace = text.find('{')
    last_brace = text.rfind('}')
    if first_brace != -1 and last_brace != -1 and last_brace > first_brace:
        return text[first_brace:last_brace+1]
    
    return text

def generate_search_plan(filename, url, model, num_queries=4):
    """Asks Ollama to generate a set of distinct queries to explore the document."""
    prompt = f"""You are an agentic search planner. Your task is to generate exactly {num_queries} distinct search queries to discover the structure, main themes, key arguments, and conclusions of the document named '{filename}'.

Rules:
1. Each query should focus on a different aspect of the document (e.g., table of contents/preface, core thesis/introduction, main theoretical chapters, final summary/conclusions).
2. The queries should be designed to return the most informative passage hits when run against a search engine.
3. The response MUST be a valid JSON array of strings:
[
  "query 1",
  "query 2",
  ...
]
Do not return any conversational text, only the JSON array."""

    payload = {
        "model": model,
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
        "stream": False,
        "options": {
            "temperature": 0.2,
            "num_ctx": 4096
        }
    }

    try:
        response = requests.post(f"{url.rstrip('/')}/api/chat", json=payload, timeout=60)
        if response.status_code == 200:
            content = response.json().get("message", {}).get("content", "").strip()
            clean_content = extract_json_block(content)
            try:
                queries = json.loads(clean_content)
                if isinstance(queries, list):
                    return [str(q) for q in queries]
            except Exception as e:
                print(f"JSON Parse Error on plan: {e}. Raw: {content}", file=sys.stderr)
        else:
            print(f"Ollama error: HTTP {response.status_code}", file=sys.stderr)
    except Exception as e:
        print(f"Failed to generate search plan: {e}", file=sys.stderr)
    
    # Fallback queries if planner fails
    return [
        "table of contents preface introduction",
        "core thesis main theory",
        "key chapters arguments",
        "conclusions summary"
    ]

def execute_search(lume_path, db_dir, query, limit, semantic_enabled):
    """Executes a search query using the Lume binary and returns unique hits."""
    cmd = [str(lume_path), "search", "--db", db_dir, "-l", str(limit)]
    if semantic_enabled:
        cmd.extend(["-a", "0.5"]) # Blend BM25 and semantic
    cmd.append(query)

    try:
        res = subprocess.run(cmd, capture_output=True, text=True, encoding="utf-8", errors="ignore")
        if res.returncode == 0:
            return res.stdout
        else:
            print(f"Lume search failed: {res.stderr}", file=sys.stderr)
    except Exception as e:
        print(f"Failed to run lume search: {e}", file=sys.stderr)
    return ""

def parse_search_hits(stdout_text):
    """Parses Lume search stdout to extract the raw text blocks."""
    # Lume output formats snippets with headers like: [1] Score: 4.6827 | Page 127 (File: ...)
    # Let's split by the hit markers and clean up the text
    hits = []
    parts = re.split(r"\[\d+\]\s+Score:\s+\d+\.\d+\s+\|", stdout_text)
    for part in parts[1:]: # Skip first split part which is header info
        lines = part.splitlines()
        # Find where text body starts (after file info line)
        body_lines = []
        for line in lines[1:]:
            body_lines.append(line)
        body = "\n".join(body_lines).strip()
        if body:
            hits.append(body)
    return hits

def synthesize_summary(filename, url, model, context_text):
    """Sends the aggregated context to Ollama to synthesize a complete summary."""
    prompt = f"""You are a senior document analyst. Below is a collection of retrieved text passages from the document '{filename}'.
Use these passages to synthesize a comprehensive, high-quality, structured summary of the entire document.

Retrieved Passages:
\"\"\"
{context_text}
\"\"\"

Your summary should include:
1. **Document Overview**: A high-level description of what the document is about.
2. **Key Themes and Arguments**: Detailed bullet points explaining the core concepts, theories, or topics discussed.
3. **Structure & Organization**: An outline of how the document is structured (if a table of contents or chapter names were retrieved).
4. **Conclusions**: The main takeaways or final thoughts of the document.

Write a professional, detailed, and cohesive summary. Do not refer to the fact that you read 'snippets' or 'passages'; write the summary as if you have read the complete document."""

    payload = {
        "model": model,
        "messages": [
            {
                "role": "system",
                "content": "You are a professional summarization assistant. You must write a cohesive, comprehensive summary based only on the provided context."
            },
            {
                "role": "user",
                "content": prompt
            }
        ],
        "stream": False,
        "options": {
            "temperature": 0.3,
            "num_ctx": 16384
        }
    }

    try:
        response = requests.post(f"{url.rstrip('/')}/api/chat", json=payload, timeout=240)
        if response.status_code == 200:
            return response.json().get("message", {}).get("content", "").strip()
        else:
            return f"Error: Ollama returned HTTP {response.status_code}"
    except Exception as e:
        return f"Error communicating with Ollama: {e}"

def main():
    parser = argparse.ArgumentParser(description="Lume Agentic Document Summarizer")
    parser.add_argument("--db", type=str, default=".lume-index", help="Lume index database path")
    parser.add_argument("--model", type=str, default="gpt-4o-mini:latest", help="Ollama model name")
    parser.add_argument("--url", type=str, default=None, help="Ollama API URL")
    parser.add_argument("--queries", type=int, default=4, help="Number of planned search queries")
    parser.add_argument("--hits-per-query", type=int, default=8, help="Number of hits to retrieve per search query")
    args = parser.parse_args()

    resolved_url = args.url if args.url else get_ollama_url()
    db_path = Path(args.db)
    state_file = db_path / "state.json"

    if not state_file.exists():
        print(f"Error: Lume index state file not found at {state_file}. Index a directory first.")
        sys.exit(1)

    # Load index state
    with open(state_file, "r", encoding="utf-8") as f:
        state = json.load(f)
    
    cached_files = list(state.get("cached_files", {}).keys())
    if not cached_files:
        print("Error: No cached files found in Lume index.")
        sys.exit(1)

    print("=== LUME AGENTIC DOCUMENT SUMMARIZER ===", file=sys.stderr)
    print(f"Found {len(cached_files)} files in index. Selecting the largest file to summarize...", file=sys.stderr)
    
    # Sort files by estimated chunk size (number of sections)
    file_sizes = []
    for f in cached_files:
        sections = state["cached_files"][f][1]
        file_sizes.append((len(sections), f))
    file_sizes.sort(reverse=True)
    
    selected_file = file_sizes[0][1]
    print(f"Target Document: {selected_file}", file=sys.stderr)
    print(f"Ollama Model: {args.model}", file=sys.stderr)
    print(f"Ollama Endpoint: {resolved_url}", file=sys.stderr)

    # Locate Lume binary
    root = Path(__file__).parent.parent
    lume_path = root / "target" / "release" / "lume"
    if not lume_path.exists():
        lume_path = root / "target" / "release" / "lume.exe"
    
    if not lume_path.exists():
        print(f"Error: Lume binary not found at {lume_path}. Compile it first using 'cargo build --release'.")
        sys.exit(1)

    # 1. Generate Search Plan
    print("\n[🧠] Planning search queries to explore the document...", file=sys.stderr)
    filename = Path(selected_file).name
    queries = generate_search_plan(filename, resolved_url, args.model, args.queries)
    for idx, q in enumerate(queries):
        print(f"  Query {idx+1}: \"{q}\"", file=sys.stderr)

    # 2. Execute searches and gather unique contexts
    print("\n[🔍] Executing searches against the Lume index...", file=sys.stderr)
    semantic_enabled = state.get("semantic_enabled", False)
    unique_snippets = set()
    
    for q in queries:
        stdout_text = execute_search(lume_path, args.db, q, args.hits_per_query, semantic_enabled)
        hits = parse_search_hits(stdout_text)
        for h in hits:
            # Only index snippets belonging to the selected target file (if there are multiple)
            # In simple cases, just add all unique bodies
            unique_snippets.add(h)
        print(f"  Ran query: \"{q}\" (Collected {len(hits)} hits)", file=sys.stderr)

    print(f"\n[📊] Gathered {len(unique_snippets)} unique passage snippets (approx. {sum(len(s) for s in unique_snippets)} characters).", file=sys.stderr)

    # 3. Synthesize summary
    print("[🧠] Synthesizing comprehensive summary...", file=sys.stderr)
    context_text = "\n\n---\n\n".join(unique_snippets)
    
    t0 = time.time()
    summary = synthesize_summary(filename, resolved_url, args.model, context_text)
    t1 = time.time()
    
    print(f"✓ Summary generated in {t1-t0:.1f}s!\n", file=sys.stderr)
    print(summary)

if __name__ == "__main__":
    main()
