#!/usr/bin/env python3
"""W10 host oracle: compare generated battery alerts, TI intervals, and DuckDB."""
import argparse
from collections import Counter
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

EPOCH = 1577836800
PATH = "electrical.batteries.house.voltage"


def identities(rows):
    return Counter((row["vessel"], row["ts_start"], row["ts_end"]) for row in rows)


def diff(expected, actual):
    missing = identities(expected) - identities(actual)
    extra = identities(actual) - identities(expected)
    def rows(counter):
        return [
            {"vessel": key[0], "ts_start": key[1], "ts_end": key[2], "count": count}
            for key, count in sorted(counter.items())
        ]
    return {"missing": rows(missing), "extra": rows(extra)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--data-dir", type=Path, required=True)
    parser.add_argument("--store", type=Path, required=True)
    args = parser.parse_args()
    import duckdb  # Host-only test tool; deliberately no Rust dependency.
    project = Path(__file__).resolve().parents[2]
    environment = dict(os.environ, CARGO_INCREMENTAL="0", CARGO_BUILD_JOBS="2",
                       TI_RULES_STORE=str(args.store.resolve()))
    process = subprocess.run([
        "cargo", "test", "--features", "ti", "--test", "ti_rules",
        "golden_battery_rule_ranges", "--", "--ignored", "--nocapture",
    ], cwd=project, env=environment, text=True, capture_output=True)
    if process.returncode:
        print(process.stdout, file=sys.stderr)
        print(process.stderr, file=sys.stderr)
        raise SystemExit(process.returncode)
    prefix = "TI_RULE_ORACLE_JSON "
    reports = [line[len(prefix):] for line in process.stdout.splitlines() if line.startswith(prefix)]
    if len(reports) != 1:
        raise RuntimeError("golden test did not print exactly one TI oracle report")
    ti = json.loads(reports[0])
    raw_root = args.data_dir / "tier=raw"
    files = sorted(str(path.resolve()) for path in raw_root.rglob("*.parquet")
                   if f"path={PATH}" in path.parts)
    if not files:
        raise RuntimeError(f"no raw {PATH} files below {raw_root}")
    scratch = project / "target" / "tmp"
    scratch.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="rules-oracle-", dir=scratch) as temporary:
        connection = duckdb.connect()
        connection.execute("SET threads=2")
        connection.execute("SET memory_limit='512MB'")
        connection.execute("SET max_temp_directory_size='4GB'")
        connection.execute("SET temp_directory=?", [temporary])
        connection.execute("""
            CREATE TEMP TABLE voltage AS
            SELECT context AS vessel,
                CAST(floor((epoch(COALESCE(
                    TRY_CAST(signalk_timestamp AS TIMESTAMP),
                    TRY_CAST(received_timestamp AS TIMESTAMP))) - ?) / ?) AS BIGINT) AS bucket,
                round(min(TRY_CAST(value AS DOUBLE)), ?) AS voltage
            FROM read_parquet(?, hive_partitioning=false, union_by_name=true)
            WHERE path = ?
            GROUP BY vessel, bucket
        """, [EPOCH, ti["width"], ti["scale"], files, PATH])
        result = connection.execute("""
            WITH matched AS (
                SELECT vessel, bucket,
                    bucket - row_number() OVER (PARTITION BY vessel ORDER BY bucket) AS island
                FROM voltage
                WHERE voltage < 24.6
            )
            SELECT vessel, ? + min(bucket) * ? AS ts_start,
                   ? + (max(bucket) + 1) * ? AS ts_end
            FROM matched
            GROUP BY vessel, island
            HAVING (max(bucket) - min(bucket) + 1) * ? >= 300
            ORDER BY vessel, ts_start
        """, [EPOCH, ti["width"], EPOCH, ti["width"], ti["width"]]).fetchall()
        oracle = [dict(zip(["vessel", "ts_start", "ts_end"], row)) for row in result]
        connection.close()
    alert_diff = diff(oracle, ti["alerts"])
    interval_diff = diff(oracle, ti["intervals"])
    passed = not any(alert_diff.values()) and not any(interval_diff.values()) \
        and ti["matched_buckets"] == ti["expected_match_buckets"]
    print(json.dumps({
        "passed": passed, "duckdb_version": duckdb.__version__, "raw_files": len(files),
        "duckdb_ranges": len(oracle), "alert_ranges": len(ti["alerts"]),
        "ti_interval_ranges": len(ti["intervals"]),
        "alerts_diff": alert_diff, "intervals_diff": interval_diff,
        "matched_buckets": ti["matched_buckets"],
        "expected_match_buckets": ti["expected_match_buckets"],
    }, indent=2))
    return 0 if passed else 1


if __name__ == "__main__":
    raise SystemExit(main())
