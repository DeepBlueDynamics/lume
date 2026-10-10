"""Quality-only existing hybrid sweep; observed per-query upstream calls."""
import argparse
import csv
import json
from pathlib import Path
import time
from http_runner import JsonClient
from run_lume import document_hits, HIT
from score import evaluate, read_qrels, read_run

p = argparse.ArgumentParser()
p.add_argument("--root", type=Path, required=True)
p.add_argument("--url", default="http://lume-h1-engine:5863/mcp")
p.add_argument("--db", default="/indexes/hybrid-h1/scifact")
a = p.parse_args()
token = (a.root / "http-token.txt").read_text().strip()
client = JsonClient(a.url, {"Authorization": "Bearer " + token}, close_after_response=True)
queries = list(csv.reader((a.root / "scifact/queries.tsv").open(), delimiter="\t"))
events = a.root / "runs/h1-shivvr-calls.jsonl"
def calls():
    return [json.loads(line) for line in events.read_text().splitlines() if line]

summaries = []
deadline = time.monotonic() + 60
while True:
    try:
        ready = client.post({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})
        if "result" in ready:
            break
    except OSError:
        client.close()
    if time.monotonic() >= deadline:
        raise RuntimeError("H1 server readiness timed out")
    time.sleep(0.2)
try:
    for alpha in (0.3, 0.5, 0.7):
        for pass_name in ("first", "cached"):
            rows, observations = [], []
            name = f"lume-h1-a{int(alpha * 10)}-{pass_name}-hybrid-scifact"
            invalid = a.root / "runs" / (name + ".invalid.json")
            invalid.write_text('{"reason":"incomplete hybrid run"}\n')
            for qid, query in queries:
                before = len(calls())
                started = time.perf_counter()
                limit = 100
                requests = 0
                while True:
                    reply = client.post({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                        "params": {"name": "lume_search", "arguments": {
                            "query": query, "db": a.db, "limit": limit, "alpha": alpha, "graph": 0}}})
                    requests += 1
                    if "error" in reply or reply.get("result", {}).get("isError"):
                        raise RuntimeError("MCP hybrid query failed")
                    text = "\n".join(c["text"] for c in reply["result"]["content"] if c["type"] == "text")
                    if "falling back" in text.lower() or "semantic search unavailable" in text.lower():
                        raise RuntimeError("Refusing lexical fallback as hybrid")
                    hits = document_hits(text)
                    if len(hits) >= 100 or len(HIT.findall(text)) < limit:
                        break
                    limit *= 2
                    if limit > 65536:
                        raise RuntimeError("document cap exceeded")
                observed = calls()[before:]
                observations.append({"qid": qid, "mcp_requests": requests,
                    "shivvr_calls": len(observed), "upstream": observed,
                    "client_ms_observed": (time.perf_counter() - started) * 1000})
                for rank, (docid, score) in enumerate(hits, 1):
                    rows.append(f"{qid} Q0 {docid} {rank} {score:.4f} lume\n")
            path = a.root / "runs" / (name + ".trec")
            path.write_text("".join(rows), newline="\n")
            marker = {"quality_only": True, "queries": len(queries), "settings": {
                "alpha": alpha, "graph": 0, "model": "GTR-T5", "pass": pass_name}}
            path.with_suffix(".quality.json").write_text(json.dumps(marker, indent=2) + "\n")
            path.with_suffix(".calls.json").write_text(json.dumps(observations, indent=2) + "\n")
            metrics, _ = evaluate(read_qrels(a.root / "scifact/qrels.tsv"), read_run(path))
            row = {"run": name, **metrics, "shivvr_calls": sum(o["shivvr_calls"] for o in observations),
                   "upstream_errors": sum(e["status"] >= 400 for o in observations for e in o["upstream"])}
            summaries.append(row)
            invalid.unlink()
            print(json.dumps(row), flush=True)
finally:
    client.close()
(a.root / "runs/h1-summary.json").write_text(json.dumps(summaries, indent=2) + "\n")
