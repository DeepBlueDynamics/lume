#!/usr/bin/env python3
"""Lightweight SciFact byte-parity checker for Raspberry Pi (aarch64).

Zero third-party dependencies (pure standard library).
Runs lume serve, executes SciFact queries, formats TREC run, and verifies
byte-parity against the expected baseline.
"""

import argparse
import csv
import hashlib
import json
import math
import os
from pathlib import Path
import re
import subprocess
import sys
import time
import urllib.error
import urllib.request

HIT = re.compile(
    r"^\[\d+\] (?:Hybrid )?Score: ([-+0-9.eE]+).*? \(File: (.*), Line: \d+\)$",
    re.MULTILINE,
)


def parse_hits(text, limit=100):
    scores = {}
    for match in HIT.finditer(text):
        score = float(match.group(1))
        filepath = match.group(2).replace("\\", "/").rsplit("/", 1)[-1]
        if not filepath.endswith(".txt") or not math.isfinite(score):
            continue
        docid = filepath[:-4]
        scores[docid] = max(scores.get(docid, -math.inf), score)
    ranked = sorted(scores.items(), key=lambda pair: (-pair[1], pair[0]))
    return ranked[:limit]


def rpc_call(url, token, method, params):
    req_body = json.dumps({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    }).encode("utf-8")
    req = urllib.request.Request(
        url,
        data=req_body,
        headers={
            "Authorization": f"Bearer {token}",
            "Content-Type": "application/json",
        },
    )
    with urllib.request.urlopen(req, timeout=10) as resp:
        return json.loads(resp.read().decode("utf-8"))


def wait_for_ready(url, token, deadline_seconds=30):
    start = time.monotonic()
    while time.monotonic() - start < deadline_seconds:
        try:
            res = rpc_call(url, token, "tools/list", {})
            if "result" in res:
                return True
        except (urllib.error.URLError, OSError):
            pass
        time.sleep(0.1)
    return False


def run_parity(binary, index_dir, queries_file, baseline_file=None, profile="f3-f2", port=5863):
    binary = Path(binary).resolve()
    index_dir = Path(index_dir).resolve()
    queries_file = Path(queries_file).resolve()
    if baseline_file:
        baseline_file = Path(baseline_file).resolve()

    token = hashlib.sha256(os.urandom(32)).hexdigest()
    token_file = Path(f"/tmp/lume-token-{port}.txt")
    token_file.write_text(token + "\n")
    url = f"http://127.0.0.1:{port}/mcp"

    env = dict(os.environ)
    env["LUME_COORD_FLOOR"] = "1.0" if profile == "f3-f2" else "0.5"

    server_cmd = [
        str(binary),
        "serve",
        "--bind", "127.0.0.1",
        "--port", str(port),
        "--http-token-file", str(token_file),
    ]

    print(f"Starting lume serve on port {port} (profile: {profile})...", flush=True)
    server = subprocess.Popen(
        server_cmd,
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )

    try:
        if not wait_for_ready(url, token):
            raise RuntimeError(f"Server failed to become ready on {url} within timeout.")

        with open(queries_file, encoding="utf-8") as f:
            queries = list(csv.reader(f, delimiter="\t"))

        print(f"Executing {len(queries)} SciFact queries against index: {index_dir}...", flush=True)
        out_rows = []
        for qid, query_text in queries:
            limit = 100
            while True:
                params = {
                    "name": "lume_search",
                    "arguments": {
                        "query": query_text,
                        "db": str(index_dir),
                        "limit": limit,
                        "alpha": 0,
                        "graph": 0,
                    },
                }
                res = rpc_call(url, token, "tools/call", params)
                content = res.get("result", {}).get("content", [])
                text = "\n".join(c.get("text", "") for c in content if c.get("type") == "text")
                hits = parse_hits(text, limit=100)
                section_count = len(HIT.findall(text))
                if len(hits) >= 100 or section_count < limit or limit >= 16384:
                    break
                limit *= 2

            for rank, (docid, score) in enumerate(hits, 1):
                out_rows.append(f"{qid} Q0 {docid} {rank} {score:.4f} lume\n")

        actual_content = "".join(out_rows).encode("utf-8")
        actual_sha = hashlib.sha256(actual_content).hexdigest()
        print(f"Run completed: {len(out_rows)} lines generated. SHA256: {actual_sha}", flush=True)

        if baseline_file:
            expected_content = Path(baseline_file).read_bytes()
            expected_sha = hashlib.sha256(expected_content).hexdigest()
            if actual_content == expected_content:
                print(f"BYTE PARITY PASS: profile={profile}, queries={len(queries)}", flush=True)
                return True
            else:
                print(f"BYTE PARITY FAIL: hash mismatch!", file=sys.stderr)
                print(f"  Actual:   {actual_sha}", file=sys.stderr)
                print(f"  Expected: {expected_sha}", file=sys.stderr)
                out_path = Path(f"/tmp/actual-{profile}-scifact.trec")
                out_path.write_bytes(actual_content)
                print(f"  Wrote actual output to: {out_path}", file=sys.stderr)
                return False
        else:
            out_path = Path(f"/tmp/scifact-{profile}.trec")
            out_path.write_bytes(actual_content)
            print(f"Wrote TREC run to: {out_path}", flush=True)
            return True

    finally:
        if server.poll() is None:
            server.terminate()
            try:
                server.wait(timeout=5)
            except subprocess.TimeoutExpired:
                server.kill()
                server.wait()
        if token_file.exists():
            token_file.unlink()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True, help="Path to lume binary")
    parser.add_argument("--index", type=Path, required=True, help="Path to SciFact index directory")
    parser.add_argument("--queries", type=Path, required=True, help="Path to queries.tsv")
    parser.add_argument("--baseline", type=Path, help="Expected .trec run file for byte-parity check")
    parser.add_argument("--profile", choices=["f3-f2", "off"], default="f3-f2")
    parser.add_argument("--port", type=int, default=5863)
    args = parser.parse_args()

    ok = run_parity(
        binary=args.binary,
        index_dir=args.index,
        queries_file=args.queries,
        baseline_file=args.baseline,
        profile=args.profile,
        port=args.port,
    )
    if not ok:
        sys.exit(1)


if __name__ == "__main__":
    main()
