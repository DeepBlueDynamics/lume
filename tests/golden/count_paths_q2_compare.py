#!/usr/bin/env python3
"""Compare q2-001 windows and their own notes with unchanged oracle expectations."""
import argparse
import datetime
import json
import os
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[2]
PRIMARY = "vessels.urn:mrn:imo:mmsi:367000000"

def timestamp(value):
    return datetime.datetime.fromisoformat(str(value).replace("Z", "+00:00")).replace(
        tzinfo=datetime.timezone.utc).timestamp()

def query(binary, store, sql):
    result = subprocess.run([str(binary.resolve()), "ti", "query", sql,
                             "--store", str(store.resolve()), "--json"],
                            cwd=ROOT, env=dict(os.environ, CARGO_INCREMENTAL="0"),
                            capture_output=True, text=True, check=True)
    reply = json.loads(result.stdout)
    if reply.get("truncated"):
        raise RuntimeError("comparison query was truncated")
    return reply

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--lume-bin", type=Path, required=True)
    parser.add_argument("--store", type=Path, required=True)
    parser.add_argument("--unscoped", action="store_true",
                        help="reproduce the original fleet-wide TI query")
    parser.add_argument("--inspect-background", action="store_true")
    parser.add_argument("--report", type=Path)
    parser.add_argument("--reply", type=Path, help="reuse an already captured TI JSON reply")
    args = parser.parse_args()
    corpus = json.loads((ROOT / "tests/golden/corpus.json").read_text())
    entry = next(e for e in corpus["entries"] if e["id"] == "q2-001")
    expected = json.loads((ROOT / "tests/golden" / entry["expected_path"]).read_text())
    sql = entry["ti_sql"]
    if args.unscoped:
        sql = sql.replace("WHERE vessel = '" + PRIMARY + "'\n  AND", "WHERE", 1)
    reply = (json.loads(args.reply.read_text()) if args.reply
             else query(args.lume_bin, args.store, sql))
    rows = reply["rows"]
    primary = [row for row in rows if row["vessel"] == PRIMARY]
    key = lambda row: (row["vessel"], timestamp(row["win"]))
    oracle = {key(row): row for row in expected}
    if {key(row) for row in primary} != set(oracle):
        raise RuntimeError("primary windows differ from unchanged oracle")
    if any(abs(row["wind_max"] - oracle[key(row)]["wind_max"]) > 0.001 + 1e-12
           for row in primary):
        raise RuntimeError("primary wind values differ from unchanged oracle")
    documents = json.loads((args.store / "docs/documents.json").read_text())
    covered = []
    for row in sorted(rows, key=key):
        start = timestamp(row["win"])
        notes = [doc for doc in documents if doc["kind"] == "notes"
                 and doc["vessel"] == row["vessel"]
                 and ("leak" in doc["body"].lower() or "water" in doc["body"].lower())
                 and doc["ts_start"] < start + 600
                 and (doc["ts_end"] if doc["ts_end"] is not None else doc["ts_start"] + 10) > start]
        if not notes:
            raise RuntimeError("window lacks its own matching note: " + str(row))
        covered.append({"window": row, "own_matching_notes": notes,
                        "oracle": oracle.get(key(row))})
    report = {"ti_windows": len(rows), "primary_vessel_windows": len(primary),
              "oracle_windows": len(expected),
              "background_vessel_windows": len(rows) - len(primary),
              "windows": covered}
    if args.inspect_background:
        background = next(item for item in covered if item["window"]["vessel"] != PRIMARY)
        row = background["window"]
        start = datetime.datetime.fromtimestamp(timestamp(row["win"]), datetime.timezone.utc)
        end = start + datetime.timedelta(minutes=10)
        # Project the virtual notes column and the actual same-vessel document.
        inspect = ("SELECT t.vessel, t.ts, t.notes, d.vessel AS doc_vessel, "
                   "d.id, d.body FROM telemetry t JOIN docs d ON t.vessel = d.vessel "
                   "AND t.ts >= d.ts_start AND t.ts < d.ts_end "
                   "WHERE t.vessel = '" + row["vessel"] + "' AND d.kind = 'notes' "
                   "AND match(notes, 'leak OR water') "
                   "AND \"propulsion.port.state\" = 'started' "
                   "AND \"electrical.bilge.pumpCycles\" >= 3 "
                   "AND \"environment.wind.speedTrue@max\" > 4 "
                   "AND t.ts >= TIMESTAMP '" + start.strftime("%Y-%m-%d %H:%M:%S") + "' "
                   "AND t.ts < TIMESTAMP '" + end.strftime("%Y-%m-%d %H:%M:%S") + "'")
        report["background_bucket_inspection"] = query(args.lume_bin, args.store, inspect)
    print("| TI vessel | TI window | TI wind_max | oracle wind_max | own note id |")
    print("|---|---|---:|---:|---|")
    for item in covered:
        row, match = item["window"], item["oracle"]
        print(f"| {row['vessel']} | {row['win']} | {row['wind_max']} | "
              f"{match['wind_max'] if match else '—'} | {item['own_matching_notes'][0]['id']} |")
    print(json.dumps(report, indent=2))
    if args.report:
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(json.dumps(report, indent=2) + "\n")

if __name__ == "__main__":
    main()
