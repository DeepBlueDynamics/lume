"""Matched-vector SciFact sweep or fixed held-out NFCorpus check; no upstream calls."""
import argparse
import csv
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import subprocess
import threading
import time

from http_runner import JsonClient
from hybrid_quality import document_hits, HIT
from score import evaluate, read_qrels, read_run
from run_lume import files


def query(client, db, text, alpha):
    limit = 100
    while True:
        reply = client.post({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "lume_search", "arguments": {
                "query": text, "db": str(db), "limit": limit, "alpha": alpha,
                "graph": 0, "shivvr_url": COUNTER_URL}}})
        if "error" in reply or reply.get("result", {}).get("isError"):
            raise RuntimeError("MCP hybrid search failed")
        output = "\n".join(c["text"] for c in reply["result"]["content"] if c["type"] == "text")
        if "falling back" in output.lower() or "semantic search unavailable" in output.lower():
            raise RuntimeError("Hybrid fallback is not a valid run")
        hits = document_hits(output)
        if len(hits) >= 100 or len(HIT.findall(output)) < limit:
            return hits
        limit *= 2
        if limit > 65536:
            raise RuntimeError("Document result cap exceeded")


def heldout_configs():
    # Fixed before seeing NFCorpus results: no alpha or depth tuning.
    return [("normalized-v2", 2.0, 60), ("vector", 1.0, 60),
            ("rrf", 1.0, 20), ("bm25", 0.0, 60)]


COUNTER_URL = ""


def main():
    global COUNTER_URL
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--dataset", choices=("scifact", "nfcorpus"), default="scifact")
    parser.add_argument("--files-dir", type=Path, help="Linux corpus directory; stage from docs.jsonl if absent")
    parser.add_argument("--db", type=Path)
    parser.add_argument("--fusion-sweep", action="store_true", help="reuse H3 index; compare opt-in fusion modes")
    parser.add_argument("--source-sha", default="166cab7")
    args = parser.parse_args()
    root = args.root
    dataset = args.dataset
    db = args.db or Path("/indexes/h3") / dataset
    files_dir = args.files_dir or root / dataset / "files"
    if db.exists() != args.fusion_sweep:
        raise RuntimeError("Fusion sweep requires the existing H3 index; initial run requires a new index")
    label = "h3-heldout-nfcorpus" if dataset == "nfcorpus" else ("h3-fusion" if args.fusion_sweep else "h3")
    runs = root / "runs"
    runs.mkdir(exist_ok=True)
    calls = []
    lock = threading.Lock()

    class Counter(BaseHTTPRequestHandler):
        def log_message(self, *unused):
            pass

        def reject(self):
            with lock:
                calls.append({"method": self.command, "route": self.path.split("?")[0]})
            body = b'{"error":"H3 prohibits upstream calls"}'
            self.send_response(503)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        do_GET = do_POST = do_DELETE = reject

    counter = ThreadingHTTPServer(("127.0.0.1", 0), Counter)
    COUNTER_URL = "http://127.0.0.1:" + str(counter.server_port)
    worker = threading.Thread(target=counter.serve_forever, daemon=True)
    worker.start()
    env = dict(os.environ, SHIVVR_BASE_URL=COUNTER_URL, LUME_STEM="1",
               LUME_COORD_FLOOR="1.0", LUME_QUERY_INVERSION="0")
    summary = []
    try:
        if not args.fusion_sweep:
            if not files_dir.exists():
                files(root, dataset, files_dir)
            prefix = root / f"embeddings/embeddinggemma-2-768-{dataset}"
            command = [str(args.binary), "index", str(files_dir),
                "--db", str(db), "--embed-model", "embeddinggemma-2",
                "--embed-dimensions", "768", "--embed-docs", str(prefix) + "-docs.jsonl",
                "--embed-queries", str(prefix) + "-queries.jsonl",
                "--embed-query-texts", str(root / dataset / "queries.tsv"),
                "--shivvr-url", COUNTER_URL]
            with (runs / (label + "-index.log")).open("wb") as output:
                subprocess.run(command, env=env, stdout=output, stderr=subprocess.STDOUT, check=True)
            if calls:
                raise RuntimeError("Index made upstream calls")
        state = json.loads((db / "state.json").read_text())
        sections = sum(len(row[1]) for row in state["cached_files"].values())
        vectors = json.loads((db / "local-vectors.json").read_text())
        queries = list(csv.reader((root / dataset / "queries.tsv").open(), delimiter="\t"))
        with (root / dataset / "docs.jsonl").open() as source:
            expected_documents = sum(bool(line.strip()) for line in source)
        expected_queries = len({text.strip() for _, text in queries})
        if sections != expected_documents or len(vectors["documents"]) != expected_documents or len(vectors["queries"]) != expected_queries:
            raise RuntimeError("Unexpected imported section/query coverage")
        qrels = read_qrels(root / dataset / "qrels.tsv")
        token = (root / "http-token.txt").read_text().strip()

        def run(blend, depth, alphas, k=60):
            server_env = dict(env, LUME_BLEND_NORM="1" if blend == "normalized" else "0",
                              LUME_LOCAL_VECTOR_DEPTH=str(depth), LUME_RRF_K=str(k),
                              LUME_BLEND=blend if args.fusion_sweep or dataset == "nfcorpus" else "")
            with (runs / f"{label}-server-{blend}-{depth}-k{k}.log").open("wb") as output:
                child = subprocess.Popen([str(args.binary), "serve", "--bind", "127.0.0.1",
                    "--port", "5863", "--http-token-file", str(root / "http-token.txt")],
                    env=server_env, stdout=output, stderr=subprocess.STDOUT)
                client = JsonClient("http://127.0.0.1:5863/mcp",
                    {"Authorization": "Bearer " + token}, close_after_response=True)
                try:
                    deadline = time.monotonic() + 60
                    while True:
                        if child.poll() is not None:
                            raise RuntimeError("H3 server exited")
                        try:
                            if "result" in client.post({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}):
                                break
                        except OSError:
                            client.close()
                        if time.monotonic() >= deadline:
                            raise RuntimeError("H3 server readiness timed out")
                        time.sleep(.2)
                    for alpha in alphas:
                        name = f"lume-{label}-{blend}-k{k}-a{int(alpha * 10)}-depth{depth}-{dataset}"
                        invalid = runs / (name + ".invalid.json")
                        invalid.write_text('{"reason":"incomplete H3 run"}\n')
                        rows = []
                        for qid, text in queries:
                            for rank, (docid, score) in enumerate(query(client, db, text, alpha), 1):
                                rows.append(f"{qid} Q0 {docid} {rank} {score:.4f} lume\n")
                        if calls:
                            raise RuntimeError("Search made upstream calls")
                        path = runs / (name + ".trec")
                        path.write_text("".join(rows), newline="\n")
                        metrics, _ = evaluate(qrels, read_run(path))
                        row = {"run": name, "blend": blend, "alpha": alpha, "depth": depth,
                               "queries": len(queries), "quality_only": True, "upstream_calls": len(calls), "rrf_k": k,
                               **metrics}
                        path.with_suffix(".quality.json").write_text(json.dumps(row, indent=2) + "\n")
                        summary.append(row)
                        invalid.unlink()
                        print(json.dumps(row), flush=True)
                finally:
                    client.close()
                    child.terminate()
                    try:
                        child.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        child.kill()
                        child.wait()

        if dataset == "nfcorpus":
            for blend, alpha, k in heldout_configs():
                run(blend, 100, (alpha,), k)
        elif args.fusion_sweep:
            for depth in (100, "all"):
                for k in (20, 60, 100):
                    run("rrf", depth, (1.0,), k)
                run("normalized-v2", depth, (.5, 1.0, 1.5, 2.0))
                run("vector", depth, (1.0,))
        else:
            run("normalized", 100, (.5, .7, .9, 1.0))
            run("multiplicative", 100, (.7,))
            best = max((row for row in summary if row["blend"] == "normalized"),
                       key=lambda row: row["ndcg_10"])
            run("normalized", "all", (best["alpha"],))
    finally:
        counter.shutdown()
        counter.server_close()
        (runs / (label + "-upstream-calls.json")).write_text(json.dumps(calls, indent=2) + "\n")
        (runs / (label + "-summary.json")).write_text(json.dumps({
            "source_sha": args.source_sha, "binary_sha256": hashlib.sha256(args.binary.read_bytes()).hexdigest(),
            "profile": "rustc 1.96; thin LTO; CGU 1", "runs": summary,
            "rrf": "tested" if args.fusion_sweep or dataset == "nfcorpus" else "not tested", "upstream_calls": len(calls),
            "dataset": dataset,
            "reference": {"ndcg@10": .7794, "recall@100": .9867, "mrr@10": .7523} if dataset == "scifact" else None
        }, indent=2) + "\n")


if __name__ == "__main__":
    main()
