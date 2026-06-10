# /// script
# dependencies = [
#     "pypdf",
#     "requests",
# ]
# ///

import os
import sys
import json
import re
import time
import argparse
import threading
from pathlib import Path
from concurrent.futures import ThreadPoolExecutor, as_completed

import pypdf
import requests

# Short words that legitimately stand alone; never treated as broken fragments.
COMMON_SHORT_WORDS = {
    "a", "i", "an", "am", "as", "at", "be", "by", "do", "go", "he", "if", "in",
    "is", "it", "me", "my", "no", "of", "on", "or", "so", "to", "up", "us",
    "we", "all", "and", "any", "are", "but", "can", "did", "for", "get", "had",
    "has", "her", "him", "his", "how", "its", "let", "may", "new", "not",
    "now", "off", "old", "one", "our", "out", "own", "per", "she", "the",
    "too", "two", "was", "who", "why", "yet", "you",
}


def build_vocab(pages_text):
    """Counts alphabetic words (3+ chars) across all pages. Used as the
    reference vocabulary for repairing extraction-broken words."""
    vocab = {}
    for text in pages_text:
        for w in re.findall(r"[A-Za-z]+", text):
            lw = w.lower()
            if len(lw) >= 3:
                vocab[lw] = vocab.get(lw, 0) + 1
    return vocab


def repair_split_words(text, vocab):
    """Repairs pypdf artifacts like 'l aw of t he' -> 'law of the'.

    Conservative: two adjacent fragments are merged only when the merged word
    appears intact at least twice elsewhere in the document AND at least one
    fragment is not a plausible standalone word.
    """
    def is_suspicious(frag):
        lf = frag.lower()
        if lf in COMMON_SHORT_WORDS:
            return False
        return vocab.get(lf, 0) < 2

    def try_merge(m):
        left, right = m.group(1), m.group(2)
        merged = left + right
        if vocab.get(merged.lower(), 0) >= 2 and (is_suspicious(left) or is_suspicious(right)):
            return merged
        return m.group(0)

    # Rejoin words hyphenated across line breaks when the joined form is known.
    def fix_hyphen(m):
        merged = m.group(1) + m.group(2)
        if vocab.get(merged.lower(), 0) >= 1:
            return merged
        return m.group(0)

    text = re.sub(r"([A-Za-z]+)-\s*\n\s*([A-Za-z]+)", fix_hyphen, text)

    # Triple splits like 't h e' first: requires both leading fragments to be
    # suspicious, so phrases of real short words are never collapsed.
    def try_merge3(m):
        a, b, c = m.group(1), m.group(2), m.group(3)
        merged = a + b + c
        if vocab.get(merged.lower(), 0) >= 2 and is_suspicious(a) and is_suspicious(b):
            return merged
        return m.group(0)

    text = re.sub(r"\b([A-Za-z]{1,2}) ([A-Za-z]{1,2}) ([A-Za-z]{1,12})\b", try_merge3, text)

    # Then pair splits, iterated to a fixpoint. Each merge strictly shrinks
    # the text, so this terminates.
    prev = None
    while prev != text:
        prev = text
        text = re.sub(r"\b([A-Za-z]{1,3}) ([A-Za-z]{1,12})\b", try_merge, text)
    return text


def extract_pdf(pdf_path):
    """Extracts text from a PDF file and returns a list of pages with text."""
    try:
        reader = pypdf.PdfReader(pdf_path)
        raw_pages = []
        for i, page in enumerate(reader.pages):
            text = page.extract_text()
            if text:
                raw_pages.append((i + 1, text))
        vocab = build_vocab(t for _, t in raw_pages)
        pages = [
            {"page_number": n, "text": repair_split_words(t, vocab)}
            for n, t in raw_pages
        ]
        return {"success": True, "pages": pages}
    except Exception as e:
        return {"success": False, "error": str(e)}

def extract_json_block(text):
    """Extracts the first matching JSON block from text."""
    text = text.strip()
    first_brace = text.find('{')
    last_brace = text.rfind('}')
    if first_brace != -1 and last_brace != -1 and last_brace > first_brace:
        return text[first_brace:last_brace+1]
    return text

def get_ollama_url():
    """Tries to find a running Ollama service on standard local endpoints."""
    urls = ["http://localhost:11434", "http://host.docker.internal:11434", "http://172.17.0.1:11434"]
    for url in urls:
        try:
            response = requests.get(f"{url}/api/tags", timeout=2)
            if response.status_code == 200:
                return url
        except Exception:
            continue
    return "http://localhost:11434"

def generate_qna_for_chunk(chunk_text, chunk_idx, source_info, url, model):
    """Invokes Ollama to generate a single Q&A pair from a text chunk."""
    prompt = f"""You are an expert document analyst compiling a Q&A evaluation benchmark.

For the following text chunk, generate exactly one clear, fact-based question and its direct, accurate answer.

Rules:
1. The question must be specific, fact-based, and directly answerable from the provided text chunk.
2. The answer must be precise, concise, and direct (no conversational fluff).
3. The response MUST be a valid JSON object matching the schema below:

{{
  "question": "...",
  "answer": "..."
}}

Text Chunk:
\"\"\"
{chunk_text}
\"\"\""""

    payload = {
        "model": model,
        "messages": [
            {
                "role": "system",
                "content": "You are a Q&A generator outputting strictly structured JSON. You must return only a JSON object matching the requested schema."
            },
            {
                "role": "user",
                "content": prompt
            }
        ],
        "format": "json",
        "stream": False,
        "options": {
            "temperature": 0.3,
            "num_ctx": 4096
        }
    }

    try:
        response = requests.post(
            f"{url.rstrip('/')}/api/chat",
            json=payload,
            timeout=180
        )
        if response.status_code == 200:
            res_json = response.json()
            content = res_json.get("message", {}).get("content", "").strip()
            clean_content = extract_json_block(content)
            try:
                result_json = json.loads(clean_content)
                result_json["chunk_index"] = chunk_idx
                result_json["source_info"] = source_info
                return result_json
            except Exception as parse_err:
                print(f"JSON Parse Error for Chunk {chunk_idx}: {parse_err}", file=sys.stderr)
                print(f"Raw response:\n{content}", file=sys.stderr)
                return None
        else:
            print(f"Error for Chunk {chunk_idx}: Ollama HTTP {response.status_code}", file=sys.stderr)
            return None
    except Exception as e:
        print(f"Error generating Q&A for Chunk {chunk_idx}: {e}", file=sys.stderr)
        return None

def main():
    parser = argparse.ArgumentParser(description="Lume Extractor & Q&A Generator")
    subparsers = parser.add_subparsers(dest="command", required=True)

    # PDF extraction command
    pdf_parser = subparsers.add_parser("pdf", help="Extract text from PDF")
    pdf_parser.add_argument("pdf_path", help="Path to PDF file")

    # Q&A generation command
    qna_parser = subparsers.add_parser("qna", help="Generate Q&A evaluation benchmark from a document")
    qna_parser.add_argument("input_path", help="Path to input document (PDF, TXT, MD, etc.)")
    qna_parser.add_argument("output_path", help="Path to save generated Q&A JSON")
    qna_parser.add_argument("--model", type=str, default="gemma4:31b-cloud", help="Ollama model name")
    qna_parser.add_argument("--url", type=str, default=None, help="Ollama API URL")
    qna_parser.add_argument("--chunk-size", type=int, default=30, help="Number of lines per chunk (for text documents)")
    qna_parser.add_argument("--workers", type=int, default=5, help="Number of parallel thread workers")
    qna_parser.add_argument("--test", action="store_true", help="Run on only 1 chunk to test")

    args = parser.parse_args()

    if args.command == "pdf":
        res = extract_pdf(args.pdf_path)
        print(json.dumps(res))
        sys.exit(0 if res.get("success") else 1)

    elif args.command == "qna":
        input_path = Path(args.input_path)
        output_path = Path(args.output_path)

        if not input_path.exists():
            print(json.dumps({"success": False, "error": f"Input path does not exist: {input_path}"}))
            sys.exit(1)

        # 1. Load document content into chunks
        chunks = []  # List of (chunk_text, chunk_idx, source_info)
        if input_path.suffix.lower() == ".pdf":
            print(f"Extracting PDF text from {input_path}...", file=sys.stderr)
            pdf_res = extract_pdf(input_path)
            if not pdf_res.get("success"):
                print(json.dumps({"success": False, "error": pdf_res.get("error")}))
                sys.exit(1)
            for idx, page in enumerate(pdf_res.get("pages", [])):
                page_text = page.get("text", "").strip()
                if len(page_text) > 100:
                    chunks.append((page_text, idx, f"Page {page['page_number']}"))
        else:
            print(f"Reading text lines from {input_path}...", file=sys.stderr)
            with open(input_path, "r", encoding="utf-8") as f:
                lines = f.read().splitlines()
            
            chunk_size = args.chunk_size
            chunk_idx = 0
            for idx in range(0, len(lines), chunk_size):
                chunk_lines = lines[idx:idx + chunk_size]
                chunk_text = "\n".join(chunk_lines).strip()
                if len(chunk_text) > 100:
                    source_info = f"Lines {idx+1}-{idx+len(chunk_lines)}"
                    chunks.append((chunk_text, chunk_idx, source_info))
                    chunk_idx += 1

        if not chunks:
            print(json.dumps({"success": False, "error": "No non-empty text chunks found in document"}))
            sys.exit(1)

        print(f"Extracted {len(chunks)} chunks to generate Q&A for.", file=sys.stderr)

        resolved_url = args.url if args.url else get_ollama_url()
        print(f"Using Ollama URL: {resolved_url}", file=sys.stderr)
        print(f"Using Ollama Model: {args.model}", file=sys.stderr)

        # Resume logic
        qna_database = []
        completed_chunks = set()
        db_lock = threading.Lock()

        if output_path.exists():
            try:
                with open(output_path, "r", encoding="utf-8") as f:
                    data = json.load(f)
                    if isinstance(data, list):
                        qna_database = data
                        completed_chunks = {item["chunk_index"] for item in qna_database if "chunk_index" in item}
                        print(f"Found existing {output_path} with {len(completed_chunks)} completed chunks. Resuming...", file=sys.stderr)
            except Exception as e:
                print(f"Error loading existing output file: {e}. Starting fresh.", file=sys.stderr)
                qna_database = []

        if args.test:
            print("\n--- Running TEST on Chunk 0 ---", file=sys.stderr)
            test_chunk, test_idx, test_info = chunks[0]
            print(f"Chunk text to send ({test_info}):\n\"\"\"\n{test_chunk}\n\"\"\"", file=sys.stderr)
            res = generate_qna_for_chunk(test_chunk, test_idx, test_info, resolved_url, args.model)
            if res:
                print(f"\nSUCCESS! Generated Q&A:\n{json.dumps(res, indent=2)}")
            else:
                print("\nFAILED to generate Q&A.", file=sys.stderr)
            sys.exit(0 if res else 1)

        # Filter remaining chunks to process
        remaining_chunks = [c for c in chunks if c[1] not in completed_chunks]
        if not remaining_chunks:
            print("✓ All chunks are already generated. Nothing to do!", file=sys.stderr)
            print(json.dumps({"success": True, "count": len(qna_database)}))
            sys.exit(0)

        print(f"Generating Q&As for {len(remaining_chunks)} remaining chunks (out of {len(chunks)} total) using {args.model}...", file=sys.stderr)

        t0 = time.time()
        with ThreadPoolExecutor(max_workers=args.workers) as executor:
            futures = {
                executor.submit(generate_qna_for_chunk, text, idx, info, resolved_url, args.model): idx 
                for text, idx, info in remaining_chunks
            }

            completed = 0
            for future in as_completed(futures):
                res = future.result()
                completed += 1
                idx = futures[future]
                if res:
                    with db_lock:
                        qna_database.append(res)
                        qna_database.sort(key=lambda x: x.get("chunk_index", 0))
                        with open(output_path, "w", encoding="utf-8") as f:
                            json.dump(qna_database, f, indent=2, ensure_ascii=False)
                    print(f" [{completed}/{len(remaining_chunks)}] Completed Q&A for chunk {res['chunk_index']} ({res['source_info']})", file=sys.stderr)
                else:
                    print(f" [{completed}/{len(remaining_chunks)}] Failed Q&A for chunk {idx}. Retrying once...", file=sys.stderr)
                    # Retry once
                    for text, c_idx, info in remaining_chunks:
                        if c_idx == idx:
                            retry_res = generate_qna_for_chunk(text, idx, info, resolved_url, args.model)
                            if retry_res:
                                with db_lock:
                                    qna_database.append(retry_res)
                                    qna_database.sort(key=lambda x: x.get("chunk_index", 0))
                                    with open(output_path, "w", encoding="utf-8") as f:
                                        json.dump(qna_database, f, indent=2, ensure_ascii=False)
                                print(f" [RETRY SUCCESS] Completed Q&A for chunk {idx}", file=sys.stderr)
                            else:
                                print(f" [RETRY FAILED] Permanent failure for chunk {idx}", file=sys.stderr)
                            break

        t1 = time.time()
        print(f"✓ Completed Q&A generation run in {t1 - t0:.1f}s!", file=sys.stderr)
        print(json.dumps({"success": True, "count": len(qna_database), "elapsed_seconds": t1 - t0}))

if __name__ == "__main__":
    main()
