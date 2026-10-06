#!/usr/bin/env python3
"""Generate golden expected query outputs for Lume TI using DuckDB.

Executes each oracle_sql from tests/golden/corpus.json against the correctness dataset
using DuckDB 1.5.6 over tests/golden/raw_view.sql, and saves formatted JSON outputs
to tests/golden/expected/<id>.json.
"""

import argparse
import json
import os
import sys
import tempfile
import time
from pathlib import Path
import duckdb


def parse_args():
    parser = argparse.ArgumentParser(
        description="Generate golden expected query outputs using DuckDB."
    )
    parser.add_argument(
        "--data-dir",
        default="/workspace/lume/.lanes/data/correctness",
        help="Root path of the correctness dataset (containing tier=raw, docs, catalog)",
    )
    parser.add_argument(
        "--corpus",
        default="tests/golden/corpus.json",
        help="Path to corpus.json",
    )
    parser.add_argument(
        "--output-dir",
        default="tests/golden/expected",
        help="Directory to write expected JSON files",
    )
    parser.add_argument(
        "--raw-view-sql",
        default="tests/golden/raw_view.sql",
        help="Path to raw_view.sql containing read_raw and read_docs macros",
    )
    parser.add_argument(
        "--query",
        default=None,
        help="Optional single query ID to run (e.g. q1-001)",
    )
    parser.add_argument(
        "--no-materialize",
        action="store_true",
        help="Use views instead of in-memory materialized tables",
    )
    return parser.parse_args()


def main():
    args = parse_args()
    data_dir = os.path.abspath(args.data_dir)
    corpus_path = os.path.abspath(args.corpus)
    output_dir = os.path.abspath(args.output_dir)
    raw_view_path = os.path.abspath(args.raw_view_sql)

    print(f"=== Golden Expected Output Generator ===", flush=True)
    print(f"Data directory:    {data_dir}", flush=True)
    print(f"Corpus file:       {corpus_path}", flush=True)
    print(f"Output directory:  {output_dir}", flush=True)
    print(f"Raw view SQL:      {raw_view_path}", flush=True)
    print(f"DuckDB version:    {duckdb.__version__}", flush=True)

    if not os.path.exists(data_dir):
        print(f"ERROR: Data directory does not exist: {data_dir}", file=sys.stderr)
        sys.exit(1)

    with open(corpus_path, "r", encoding="utf-8") as f:
        corpus = json.load(f)

    os.makedirs(output_dir, exist_ok=True)

    print("\nInitializing DuckDB...", flush=True)
    conn = duckdb.connect()

    # Load macros from raw_view.sql
    with open(raw_view_path, "r", encoding="utf-8") as f:
        raw_view_sql = f.read()
    conn.execute(raw_view_sql)

    # Set up tables/views
    materialize = not args.no_materialize
    kind_str = "table" if materialize else "view"
    create_kw = "CREATE OR REPLACE TABLE" if materialize else "CREATE OR REPLACE VIEW"

    print(f"Setting up 'raw' {kind_str} from {data_dir}...", flush=True)
    t0 = time.time()
    conn.execute(f"{create_kw} raw AS SELECT * FROM read_raw('{data_dir}');")
    print(f"  'raw' ready in {time.time() - t0:.2f} s", flush=True)

    print(f"Setting up 'docs' {kind_str} from {data_dir}...", flush=True)
    t0 = time.time()
    conn.execute(f"{create_kw} docs AS SELECT * FROM read_docs('{data_dir}');")
    print(f"  'docs' ready in {time.time() - t0:.2f} s", flush=True)

    print(f"Setting up 'vessels' {kind_str} from {data_dir}...", flush=True)
    t0 = time.time()
    vessels_glob = f"{data_dir}/catalog/vessels/**/*.parquet"
    conn.execute(
        f"{create_kw} vessels AS SELECT * FROM read_parquet('{vessels_glob}', hive_partitioning=false, union_by_name=true);"
    )
    print(f"  'vessels' ready in {time.time() - t0:.2f} s", flush=True)

    entries = corpus["entries"]
    if args.query:
        entries = [e for e in entries if e["id"] == args.query]
        if not entries:
            print(f"ERROR: Query ID '{args.query}' not found in corpus.", file=sys.stderr)
            sys.exit(1)

    print(f"\nExecuting {len(entries)} queries...\n", flush=True)

    results = []
    zero_row_entries = []

    for idx, entry in enumerate(entries, 1):
        qid = entry["id"]
        qclass = entry.get("class", "Q?")
        oracle_sql = entry["oracle_sql"]
        out_rel_path = entry.get("expected_path", f"expected/{qid}.json")
        out_file_name = os.path.basename(out_rel_path)
        dest_path = os.path.join(output_dir, out_file_name)

        t_start = time.time()
        try:
            with tempfile.NamedTemporaryFile(suffix=".json", delete=False) as tmp_f:
                tmp_json = tmp_f.name

            conn.execute(f"COPY ({oracle_sql}) TO '{tmp_json}' (FORMAT JSON, ARRAY true);")

            with open(tmp_json, "r", encoding="utf-8") as f:
                content = f.read().strip()
            os.remove(tmp_json)

            if not content or content == "[\n\t\n]" or content == "[]":
                rows = []
            else:
                rows = json.loads(content)

            row_count = len(rows)
            with open(dest_path, "w", encoding="utf-8") as f:
                json.dump(rows, f, indent=2)
                f.write("\n")

            elapsed = time.time() - t_start
            status = "OK"
            if row_count == 0:
                zero_row_entries.append((qid, entry.get("description", "")))

            results.append((qid, qclass, status, row_count, elapsed, None))
            print(f"[{idx:02d}/{len(entries):02d}] {qid:<8} ({qclass}): {row_count:>5} rows  ({elapsed:5.2f}s)", flush=True)

        except Exception as e:
            elapsed = time.time() - t_start
            err_msg = str(e)
            results.append((qid, qclass, "ERROR", 0, elapsed, err_msg))
            print(f"[{idx:02d}/{len(entries):02d}] {qid:<8} ({qclass}): FAILED in {elapsed:5.2f}s - {err_msg}", flush=True)

    print("\n" + "=" * 60, flush=True)
    print("Execution Summary:", flush=True)
    print("=" * 60, flush=True)

    success_count = sum(1 for r in results if r[2] == "OK")
    error_count = sum(1 for r in results if r[2] == "ERROR")
    total_rows = sum(r[3] for r in results)

    print(f"Total queries:   {len(results)}")
    print(f"Passed:          {success_count}")
    print(f"Failed:          {error_count}")
    print(f"Total rows:      {total_rows}")
    print(f"Zero-row queries: {len(zero_row_entries)}")

    if zero_row_entries:
        print("\nQueries returning 0 rows:")
        for qid, desc in zero_row_entries:
            print(f"  - {qid}: {desc}")

    if error_count > 0:
        print("\nErrors occurred during execution:", file=sys.stderr)
        for r in results:
            if r[2] == "ERROR":
                print(f"  - {r[0]}: {r[5]}", file=sys.stderr)
        sys.exit(1)

    print(f"\nAll expected output JSON files successfully written to {output_dir}/")


if __name__ == "__main__":
    main()
