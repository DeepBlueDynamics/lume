#!/usr/bin/env python3
"""HOST ONLY: generate hidden DuckDB answers. The agent harness never imports this."""
import argparse
import datetime
import decimal
import hashlib
import json
from pathlib import Path
import tempfile

ROOT = Path(__file__).resolve().parents[1]


def normalize(value):
    if isinstance(value, datetime.datetime):
        return value.replace(tzinfo=None).isoformat(sep=" ")
    if isinstance(value, decimal.Decimal):
        return float(value)
    return value


def extract(rows, rule):
    mode = rule["mode"]
    if mode == "scalar":
        if len(rows) != 1:
            raise ValueError("Scalar answer requires exactly one row")
        return rows[0][rule["column"]]
    if mode == "column_set":
        return sorted(set(row[rule["column"]] for row in rows))
    if mode == "rows":
        return rows
    raise ValueError("Unknown extraction mode")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--data-dir", required=True, type=Path)
    parser.add_argument("--questions", type=Path, default=ROOT / "tests/golden/agent_questions.json")
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--scratch", type=Path, default=ROOT / ".test-tmp/agent-oracle")
    args = parser.parse_args()
    import duckdb  # Already installed on the host; no pip or subprocess invocation.
    fixture = args.questions.read_bytes()
    questions = json.loads(fixture)["questions"]
    args.scratch.mkdir(parents=True, exist_ok=True)
    answers = []
    with tempfile.TemporaryDirectory(dir=args.scratch) as scratch:
        connection = duckdb.connect()
        try:
            connection.execute("SET threads=2")
            connection.execute("SET TimeZone='UTC'")
            connection.execute("SET memory_limit='512MB'")
            connection.execute("SET max_temp_directory_size='4GB'")
            connection.execute("SET temp_directory=?", [scratch])
            connection.execute((ROOT / "tests/golden/raw_view.sql").read_text(encoding="utf-8"))
            data = args.data_dir.resolve().as_posix().replace("'", "''")
            connection.execute(f"CREATE VIEW raw AS SELECT * FROM read_raw('{data}')")
            connection.execute(f"CREATE VIEW docs AS SELECT * FROM read_docs('{data}')")
            for question in questions:
                hidden = question["hidden"]
                cursor = connection.execute(hidden["oracle_sql"])
                columns = [column[0] for column in cursor.description]
                rows = [dict(zip(columns, map(normalize, row))) for row in cursor.fetchall()]
                answers.append({"id": question["id"], "answer": extract(rows, hidden["answer_extraction"]),
                                "grading": hidden["grading"], "oracle_rows": len(rows)})
                print(f'{question["id"]}: {len(rows)} oracle rows', flush=True)
        finally:
            connection.close()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps({
        "questions_sha256": hashlib.sha256(fixture).hexdigest(),
        "data_dir": str(args.data_dir.resolve()), "answers": answers}, indent=2) + "\n",
        encoding="utf-8")


if __name__ == "__main__":
    main()
