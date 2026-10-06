#!/usr/bin/env python3
"""Host-only W9 oracle: real Parquet -> DuckDB -> checked-in outputs -> Lume verify."""
import argparse
import datetime
import decimal
import json
import os
from pathlib import Path
import subprocess
import tempfile

EPOCH = 1577836800

def run(command, project, environment):
    print("+", " ".join(map(str, command)), flush=True)
    subprocess.run(list(map(str, command)), cwd=project, env=environment, check=True)

def normalized(value):
    if isinstance(value, (datetime.datetime, datetime.date)):
        return value.isoformat(sep=" ") if isinstance(value, datetime.datetime) else value.isoformat()
    if isinstance(value, decimal.Decimal):
        return float(value)
    if isinstance(value, list):
        return [normalized(v) for v in value]
    return value

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--data-dir", type=Path, required=True)
    parser.add_argument("--store", type=Path, required=True)
    parser.add_argument("--lume-bin", type=Path)
    parser.add_argument("--bench-bin", type=Path)
    parser.add_argument("--prepare", action="store_true", help="generate data and import boat, robots, mapped docs")
    parser.add_argument("--write-expected", action="store_true", help="refresh checked-in outputs from independent DuckDB")
    args = parser.parse_args()
    import duckdb  # Host-only, no runtime dependency.
    corpus_root = Path(__file__).resolve().parent
    project = corpus_root.parents[2]
    suffix = ".exe" if os.name == "nt" else ""
    lume = (args.lume_bin or project / "target" / "debug" / ("lume" + suffix)).resolve()
    bench = (args.bench_bin or project / "target" / "debug" / ("ti-bench" + suffix)).resolve()
    data = args.data_dir.resolve()
    store = args.store.resolve()
    environment = dict(os.environ, CARGO_INCREMENTAL="0", CARGO_BUILD_JOBS="2")
    if args.prepare:
        run([bench, "gen", "--profile", "robots", "--root", data], project, environment)
        run([lume, "ti", "backfill", "--signalk", data / "signalk" / "tier=raw",
             "--store", store], project, environment)
        run([lume, "ti", "backfill", "--parquet", str(data / "raw" / "*.parquet"),
             "--entity", "robot_id", "--time", "timestamp", "--time-unit", "ms",
             "--wide", "--prefix", "robot.", "--units", data / "units.toml", "--store", store], project, environment)
        run([lume, "ti", "import-docs", "--parquet", data / "documents" / "incidents.parquet",
             "--entity", "entity", "--time", "start_ms", "--time-end", "end_ms", "--time-unit", "ms",
             "--id", "id", "--kind", "kind", "--title", "title", "--body", "body", "--store", store], project, environment)
    robot_files = sorted(str(p) for p in (data / "raw").glob("*.parquet"))
    boat_files = sorted(str(p) for p in (data / "signalk" / "tier=raw").rglob("*.parquet"))
    if len(robot_files) != 6 or not boat_files:
        raise RuntimeError("oracle requires six robot/day files and the mixed Signal K boat fixture")
    corpus = json.loads((corpus_root / "corpus.json").read_text(encoding="utf-8"))
    scratch = project / "target" / "tmp"
    scratch.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="robots-oracle-", dir=scratch) as temporary:
        connection = duckdb.connect()
        connection.execute("SET threads=2")
        connection.execute("SET TimeZone='UTC'")
        connection.execute("SET memory_limit='512MB'")
        connection.execute("SET max_temp_directory_size='4GB'")
        connection.execute("SET temp_directory=?", [temporary])
        connection.execute("""
            CREATE TEMP TABLE robot_raw AS SELECT * FROM read_parquet(?, hive_partitioning=false)
        """, [robot_files])
        connection.execute("""
            CREATE TEMP TABLE robot_buckets AS
            SELECT robot_id AS vessel,
                CAST(floor((epoch(timestamp)-?) / 10) AS BIGINT) AS bucket,
                round(avg(battery_voltage),3) AS "robot.battery_voltage@mean",
                round(min(battery_voltage),3) AS "robot.battery_voltage@min",
                round(max(battery_voltage),3) AS "robot.battery_voltage@max",
                round(avg(motor_current),2) AS "robot.motor_current@mean",
                round(min(motor_current),2) AS "robot.motor_current@min",
                round(max(motor_current),2) AS "robot.motor_current@max",
                arg_max(mode,timestamp) AS "robot.mode",
                arg_max(fault,timestamp) AS "robot.fault",
                round(max(pose_x),3) AS "robot.pose_x@max",
                ['default'] AS "robot.motor_current$source"
            FROM robot_raw GROUP BY vessel,bucket
        """, [EPOCH])
        connection.execute("""
            CREATE TEMP TABLE boat_raw AS SELECT * FROM read_parquet(?, hive_partitioning=false)
        """, [boat_files])
        connection.execute("""
            CREATE TEMP TABLE telemetry AS
            SELECT *,vessel AS entity,to_timestamp(?+bucket*10) AS ts FROM robot_buckets
            UNION ALL BY NAME
            SELECT context AS vessel,context AS entity,
                CAST(floor((epoch(signalk_timestamp)-?)/10) AS BIGINT) AS bucket,
                to_timestamp(?+CAST(floor((epoch(signalk_timestamp)-?)/10) AS BIGINT)*10) AS ts
            FROM boat_raw GROUP BY ALL
        """, [EPOCH,EPOCH,EPOCH,EPOCH])
        connection.execute("""
            CREATE TEMP TABLE docs AS
            SELECT id,entity AS vessel,entity,kind,title,body,
                to_timestamp(start_ms/1000) AS ts_start,to_timestamp(end_ms/1000) AS ts_end
            FROM read_parquet(?,hive_partitioning=false)
        """, [str(data / "documents" / "incidents.parquet")])
        results = []
        for entry in corpus["entries"]:
            cursor = connection.execute(entry["oracle_sql"])
            columns = [column[0] for column in cursor.description]
            rows = [dict(zip(columns,map(normalized,row))) for row in cursor.fetchall()]
            if not rows:
                raise RuntimeError(f"{entry['id']}: empty oracle would make acceptance vacuous")
            expected = corpus_root / entry["expected_path"]
            if args.write_expected:
                expected.parent.mkdir(parents=True,exist_ok=True)
                expected.write_text(json.dumps(rows,indent=2)+"\n",encoding="utf-8")
            else:
                reference = json.loads(expected.read_text(encoding="utf-8"))
                if rows != reference:
                    raise RuntimeError(f"{entry['id']}: DuckDB differs from stored expected output: {rows!r} vs {reference!r}")
            results.append({"id":entry["id"],"rows":len(rows)})
        connection.close()
    run([lume,"ti","verify","--store",store,"--corpus",corpus_root],project,environment)
    print(json.dumps({"passed":True,"duckdb_version":duckdb.__version__,"checked":len(results),
                      "nonempty":len(results),"entries":results},indent=2))
    return 0

if __name__ == "__main__":
    raise SystemExit(main())
