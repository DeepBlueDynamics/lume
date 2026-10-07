#!/usr/bin/env python3
"""Count-path acceptance: unchanged DuckDB oracles, complete corpus and empty-list hashes.

DuckDB runs on the host only. --skip-duckdb runs stored-output/hash checks locally,
and reports that the independent oracle was not run. Stores must be fresh for --prepare.
"""
import argparse
import datetime
import decimal
import json
import os
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]
IDS = {"q1-007", "q6-006", "q2-001"}

def run(command):
    result = subprocess.run([str(x) for x in command], cwd=ROOT,
                            env=dict(os.environ, CARGO_INCREMENTAL="0", TI_OPT_IN="last"),
                            capture_output=True, text=True, check=True)
    if result.stderr:
        print(result.stderr, flush=True)
    return result.stdout

def hashes(root):
    entries = json.loads((root / "manifest.json").read_text(encoding="utf-8"))
    return {(e["key"]["vessel"], e["key"]["shard"]): e["hash"] for e in entries}

def normalized(value):
    if isinstance(value, datetime.datetime):
        return value.replace(tzinfo=None).isoformat(sep=" ")
    if isinstance(value, decimal.Decimal):
        return float(value)
    return value

def equal(entry, actual, expected):
    sort = entry["tolerance"]["sort"]
    key = lambda row: tuple(str(row.get(k)) for k in sort)
    actual, expected = sorted(actual, key=key), sorted(expected, key=key)
    if len(actual) != len(expected):
        return False
    exact = entry["tolerance"]["exact"]
    bsi = entry["tolerance"]["bsi"]
    for left, right in zip(actual, expected):
        if any(left.get(k) != right.get(k) for k in exact):
            return False
        for k, scale in bsi.items():
            a, b = left.get(k), right.get(k)
            if a is None or b is None:
                if a != b:
                    return False
            elif abs(a-b) > 10 ** (-scale) + 1e-12:
                return False
    return True

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--data-dir", type=Path, required=True)
    parser.add_argument("--baseline-store", type=Path, required=True)
    parser.add_argument("--empty-store", type=Path, required=True)
    parser.add_argument("--count-store", type=Path, required=True)
    parser.add_argument("--lume-bin", type=Path)
    parser.add_argument("--prepare", action="store_true")
    parser.add_argument("--skip-duckdb", action="store_true")
    args = parser.parse_args()
    suffix = ".exe" if os.name == "nt" else ""
    lume = (args.lume_bin or ROOT / "target/debug" / ("lume" + suffix)).resolve()
    data = args.data_dir.resolve()
    empty, counted = args.empty_store.resolve(), args.count_store.resolve()
    if args.prepare:
        for store, paths in [(empty, []), (counted, ["electrical.bilge.pumpCycles"])]:
            if (store / "catalog").exists():
                raise RuntimeError(f"--prepare requires a fresh store: {store}")
            store.mkdir(parents=True, exist_ok=True)
            (store / "ti.toml").write_text(
                "[ingest]\ncount_paths = " + json.dumps(paths) + "\n",
                encoding="utf-8")
            print(run([lume, "ti", "backfill", "--signalk", data / "tier=raw",
                       "--store", store]), flush=True)
    baseline = hashes(args.baseline_store.resolve())
    candidate = hashes(empty)
    if len(baseline) != 65 or candidate != baseline:
        different = sorted(set(baseline) | set(candidate))
        different = [key for key in different if baseline.get(key) != candidate.get(key)]
        raise RuntimeError(f"empty count_paths hash regression: baseline={len(baseline)}, candidate={len(candidate)}, differences={different}")
    # The committed expectations and oracle SQL are never rewritten.
    output = run([lume, "ti", "verify", "--store", counted, "--corpus", ROOT / "tests/golden"])
    print(output, flush=True)
    verify = json.loads(output)
    if (verify["passed"], verify["failed"], verify["excluded"]) != (61, 0, 1):
        raise RuntimeError(f"boat corpus regression: {verify}")
    corpus = json.loads((ROOT / "tests/golden/corpus.json").read_text(encoding="utf-8"))
    oracle_results = []
    if not args.skip_duckdb:
        import duckdb  # Host test dependency only; never installed by this script.
        scratch = Path(os.environ.get("CARGO_TARGET_TMPDIR", ROOT / "target/tmp"))
        scratch.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="count-oracle-", dir=scratch) as tmp:
            connection = duckdb.connect()
            connection.execute("SET threads=2")
            connection.execute("SET TimeZone='UTC'")
            connection.execute("SET memory_limit='512MB'")
            connection.execute("SET max_temp_directory_size='4GB'")
            connection.execute("SET temp_directory=?", [tmp])
            connection.execute((ROOT / "tests/golden/raw_view.sql").read_text(encoding="utf-8"))
            escaped = str(data).replace("'", "''")
            connection.execute(f"CREATE VIEW raw AS SELECT * FROM read_raw('{escaped}')")
            connection.execute(f"CREATE VIEW docs AS SELECT * FROM read_docs('{escaped}')")
            for entry in corpus["entries"]:
                if entry["id"] not in IDS:
                    continue
                cursor = connection.execute(entry["oracle_sql"])
                columns = [c[0] for c in cursor.description]
                actual = [dict(zip(columns, map(normalized, row))) for row in cursor.fetchall()]
                expected = json.loads((ROOT / "tests/golden" / entry["expected_path"]).read_text(encoding="utf-8"))
                if not actual or not equal(entry, actual, expected):
                    raise RuntimeError(f"{entry['id']}: unchanged DuckDB oracle disagrees with stored expected")
                oracle_results.append({"id": entry["id"], "rows": len(actual)})
            connection.close()
    print(json.dumps({"empty_list_shard_hashes": len(candidate),
                      "boat_verify": verify,
                      "duckdb": "not run" if args.skip_duckdb else oracle_results}, indent=2))
    return 0

if __name__ == "__main__":
    raise SystemExit(main())
