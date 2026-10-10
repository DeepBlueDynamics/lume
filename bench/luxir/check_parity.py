#!/usr/bin/env python3
"""Check resident MCP rankings against released TREC runs before timings."""
import argparse
import csv
import hashlib
import json
from pathlib import Path
from http_runner import JsonClient
from run_lume import request


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--dataset", required=True)
    parser.add_argument("--db", required=True)
    parser.add_argument("--url", default="http://lume-engine:5863/mcp")
    parser.add_argument("--modes", nargs="+", choices=["bm25", "default"], default=["bm25", "default"])
    args = parser.parse_args()
    token = (args.root / "http-token.txt").read_text().strip()
    with (args.root / args.dataset / "queries.tsv").open(encoding="utf-8") as source:
        queries = list(csv.reader(source, delimiter="\t"))
    client = JsonClient(args.url, {"Authorization": "Bearer " + token}, close_after_response=True)
    report = {}
    try:
        for mode in args.modes:
            graph = {"bm25": 0, "default": .4}[mode]
            rows = []
            for qid, query in queries:
                for rank, (docid, score) in enumerate(request(args.url, args.db, query, graph, token, client), 1):
                    rows.append(f"{qid} Q0 {docid} {rank} {score:.4f} lume\n")
            actual = "".join(rows).encode()
            baseline = args.root / "runs" / f"lume-released-{mode}-{args.dataset}.trec"
            expected = baseline.read_bytes()
            equal = actual == expected
            report[mode] = {"byte_identical": equal, "queries": len(queries),
                            "sha256": hashlib.sha256(actual).hexdigest(),
                            "released_sha256": hashlib.sha256(expected).hexdigest()}
            print(mode, json.dumps(report[mode]), flush=True)
            if not equal:
                raise ValueError("resident/released ranking mismatch: " + mode)
    finally:
        client.close()
    (args.root / "runs" / f"lume-resident-{args.dataset}.parity.json").write_text(json.dumps(report, indent=2) + "\n")


if __name__ == "__main__":
    main()
