#!/usr/bin/env python3
"""DuckDB baseline benchmark for Lume TI golden corpus queries (Q1-Q8)."""

import argparse
import json
import os
import sys
import time

try:
    import duckdb
except ImportError:
    print("DuckDB not installed in this Python environment", file=sys.stderr)
    sys.exit(3)


def parse_args():
    parser = argparse.ArgumentParser(description="DuckDB baseline benchmark")
    parser.add_argument("--data-dir", default=".lanes/data/correctness")
    parser.add_argument("--corpus", default="tests/golden/corpus.json")
    parser.add_argument("--raw-view-sql", default="tests/golden/raw_view.sql")
    parser.add_argument("--iterations", type=int, default=5)
    parser.add_argument("--output-json", default=None)
    return parser.parse_args()


def percentile(data, p):
    if not data:
        return 0.0
    sorted_data = sorted(data)
    idx = int(len(sorted_data) * p)
    if idx >= len(sorted_data):
        idx = len(sorted_data) - 1
    return sorted_data[idx]


def main():
    args = parse_args()
    data_dir = os.path.abspath(args.data_dir)
    corpus_path = os.path.abspath(args.corpus)
    raw_view_path = os.path.abspath(args.raw_view_sql)

    if not os.path.exists(data_dir):
        print(f"Data dir {data_dir} does not exist", file=sys.stderr)
        sys.exit(1)

    with open(corpus_path, "r", encoding="utf-8") as f:
        corpus = json.load(f)

    with open(raw_view_path, "r", encoding="utf-8") as f:
        raw_view_sql = f.read()

    conn = duckdb.connect()
    conn.execute(raw_view_sql)
    conn.execute(f"CREATE OR REPLACE VIEW raw AS SELECT * FROM read_raw('{data_dir}');")
    conn.execute(f"CREATE OR REPLACE VIEW docs AS SELECT * FROM read_docs('{data_dir}');")

    queries_out = []
    class_durations = {}

    for entry in corpus["entries"]:
        if entry.get("exclude"):
            continue
        qid = entry["id"]
        qclass = entry.get("class", "QX")
        sql = entry.get("oracle_sql")
        if not sql:
            continue

        # Cold run
        t0 = time.perf_counter()
        res = conn.execute(sql).fetchall()
        cold_ms = (time.perf_counter() - t0) * 1000.0
        rows = len(res)

        # Warm runs
        warm_ms_list = []
        for _ in range(args.iterations):
            t1 = time.perf_counter()
            _ = conn.execute(sql).fetchall()
            warm_ms_list.append((time.perf_counter() - t1) * 1000.0)

        p50 = percentile(warm_ms_list, 0.50)
        p95 = percentile(warm_ms_list, 0.95)
        p99 = percentile(warm_ms_list, 0.99)

        queries_out.append({
            "id": qid,
            "class": qclass,
            "description": entry.get("description", ""),
            "rows": rows,
            "cold_ms": cold_ms,
            "p50_ms": p50,
            "p95_ms": p95,
            "p99_ms": p99,
        })
        class_durations.setdefault(qclass, []).extend(warm_ms_list)

    class_stats = {}
    for qclass, durs in sorted(class_durations.items()):
        class_stats[qclass] = {
            "p50_ms": percentile(durs, 0.50),
            "p95_ms": percentile(durs, 0.95),
            "p99_ms": percentile(durs, 0.99),
        }

    report = {
        "duckdb_version": duckdb.__version__,
        "iterations": args.iterations,
        "classes": class_stats,
        "queries": queries_out,
    }

    if args.output_json:
        os.makedirs(os.path.dirname(os.path.abspath(args.output_json)), exist_ok=True)
        with open(args.output_json, "w", encoding="utf-8") as f:
            json.dump(report, f, indent=2)

    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
