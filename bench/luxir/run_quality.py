#!/usr/bin/env python3
"""Collect one complete quality-only MCP search run; no latency or QPS claims."""
import argparse
import csv
import json
from pathlib import Path
import re
from http_runner import JsonClient
from run_lume import request


def collect(root, dataset, label, db, url, settings):
    if not re.fullmatch(r"[a-z0-9-]+", label):
        raise ValueError("unsafe run label")
    runs = root / "runs"
    runs.mkdir(exist_ok=True)
    name = f"lume-{label}-bm25-{dataset}"
    invalid = runs / (name + ".invalid.json")
    invalid.write_text(json.dumps({"reason": "quality run incomplete"}) + "\n")
    with (root / dataset / "queries.tsv").open(encoding="utf-8") as source:
        queries = list(csv.reader(source, delimiter="\t"))
    token = (root / "http-token.txt").read_text().strip()
    client = JsonClient(url, {"Authorization": "Bearer " + token}, close_after_response=True)
    rows = []
    try:
        for qid, query in queries:
            for rank, (docid, score) in enumerate(request(url, db, query, 0, token, client), 1):
                rows.append(f"{qid} Q0 {docid} {rank} {score:.4f} lume\n")
    finally:
        client.close()
    # Only publish a complete run, including queries that legitimately have no hits.
    marker = runs / (name + ".quality.json")
    marker.write_text(json.dumps({"quality_only": True, "queries": len(queries),
                                 "settings": settings, "db": db}, indent=2) + "\n")
    staged = runs / (name + ".trec.part")
    staged.write_text("".join(rows), encoding="utf-8", newline="\n")
    staged.replace(runs / (name + ".trec"))
    invalid.unlink()
    print(name, "complete:", len(queries), "queries", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--dataset", choices=["scifact", "trec-covid", "nfcorpus"], required=True)
    parser.add_argument("--label", required=True)
    parser.add_argument("--db", required=True)
    parser.add_argument("--url", default="http://lume-engine:5863/mcp")
    parser.add_argument("--settings", type=json.loads, default={}, help="JSON describing flags and binary provenance; no secrets")
    args = parser.parse_args()
    collect(args.root, args.dataset, args.label, args.db, args.url, args.settings)


if __name__ == "__main__":
    main()
