#!/usr/bin/env python3
"""Same-binary sealed-cache A/B; stdlib only, no store/config writes."""
import argparse
import json
import pathlib
import statistics
import subprocess

QUERY_IDS = """q1-001 q1-002 q1-004 q2-002 q2-003 q2-005
q3-001 q3-002 q3-004 q4-001 q4-002 q4-005 q5-001 q5-002
q7-001 q7-002 q7-006 q8-001 q8-002 q8-006""".split()


def compare(before, after):
    if not before.get("queries") and not after.get("queries"):
        old, new = before.get("classes", {}), after.get("classes", {})
        if not old or old.keys() != new.keys():
            raise ValueError("A/B reports have empty or different class sets")
        return [
            dict(id=key, rows=None, values_match=None,
                 off_cold_ms=a["cold_ms"], on_cold_ms=new[key]["cold_ms"],
                 off_p50_ms=a["p50_ms"], on_p50_ms=new[key]["p50_ms"],
                 warm_speedup=a["p50_ms"] / max(new[key]["p50_ms"], 1e-9),
                 cache_stats={}, granularity="class")
            for key, a in old.items()
        ]
    old = {q["id"]: q for q in before.get("queries", [])}
    new = {q["id"]: q for q in after.get("queries", [])}
    if not old or old.keys() != new.keys():
        raise ValueError("A/B query sets differ")
    rows = []
    for qid, a in old.items():
        b = new[qid]
        same = (a["rows"], a["answer_fingerprint"]) == (b["rows"], b["answer_fingerprint"])
        stats = b.get("cache_stats") or {}
        if stats.get("used_bytes", 0) > stats.get("budget_bytes", 0):
            raise ValueError("Cache exceeded byte budget")
        rows.append({"id": qid, "rows": b["rows"], "values_match": same,
                     "off_cold_ms": a["cold_ms"], "on_cold_ms": b["cold_ms"],
                     "off_p50_ms": a["p50_ms"], "on_p50_ms": b["p50_ms"],
                     "warm_speedup": a["p50_ms"] / max(b["p50_ms"], 1e-9),
                     "cache_stats": stats})
    return rows


def markdown(rows):
    if not rows:
        raise ValueError("Cannot render an empty comparison")
    lines = ["| Query/class | Rows | Cold off/on ms | Warm p50 off/on ms | Speedup | Values |",
             "|---|---:|---:|---:|---:|---|"]
    for r in rows:
        count = "—" if r["rows"] is None else r["rows"]
        verdict = "not checked (class summary)" if r["values_match"] is None else ("PASS" if r["values_match"] else "MISMATCH")
        lines.append(f"| {r['id']} | {count} | {r['off_cold_ms']:.2f} / {r['on_cold_ms']:.2f} | "
                     f"{r['off_p50_ms']:.2f} / {r['on_p50_ms']:.2f} | {r['warm_speedup']:.2f}× | "
                     f"{verdict} |")
    return "\n".join(lines) + "\n"


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--binary", type=pathlib.Path, required=True)
    p.add_argument("--store", type=pathlib.Path, required=True)
    p.add_argument("--corpus", type=pathlib.Path, default=pathlib.Path("tests/golden/corpus.json"))
    p.add_argument("--out", type=pathlib.Path, required=True)
    p.add_argument("--iterations", type=int, default=7)
    p.add_argument("--cache-bytes", type=int, default=268435456)
    p.add_argument("--ids", nargs="+", default=QUERY_IDS)
    p.add_argument("--toolchain", required=True, help="record rustc version/build profile")
    args = p.parse_args()
    if args.iterations < 1 or args.cache_bytes < 1:
        p.error("iterations and cache-bytes must be positive")
    binary, store, out = args.binary.resolve(), args.store.resolve(), args.out.resolve()
    corpus = json.loads(args.corpus.read_text(encoding="utf-8"))
    entries = {e["id"]: e for e in corpus["entries"]}
    chosen = [entries[qid] for qid in args.ids]
    if any(e.get("exclude") for e in chosen):
        p.error("selected query is excluded")
    out.mkdir(parents=True, exist_ok=True)
    selected = out / "corpus.json"
    selected.write_text(json.dumps({**corpus, "entries": chosen}, indent=2), encoding="utf-8")
    reports = []
    commands = []
    for label, budget in [("off", 0), ("on", args.cache_bytes)]:
        folder = out / label
        folder.mkdir(exist_ok=True)
        # Isolated cwd keeps the runner from auto-starting a DuckDB baseline.
        cwd = pathlib.Path(__file__).resolve().parents[1] / ".test-tmp" / "query-cache" / out.name / label / "isolated" / "work"
        cwd.mkdir(parents=True, exist_ok=True)
        command = [str(binary), "bench", "--store", str(store), "--corpus", str(selected),
                   "--out-dir", str(folder), "--iterations", str(args.iterations),
                   "--cache-bytes", str(budget), "--parquet", str(store)]
        commands.append(command)
        with (folder / "run.log").open("w", encoding="utf-8") as log:
            subprocess.run(command, cwd=cwd, stdout=log, stderr=subprocess.STDOUT, check=True)
        matches = list(folder.glob("*.json"))
        if len(matches) != 1:
            raise RuntimeError("Use a fresh output directory; expected one report")
        reports.append(json.loads(matches[0].read_text(encoding="utf-8")))
    rows = compare(*reports)
    result = {"toolchain": args.toolchain, "commands": commands,
              "cold_definition": "decoded application cache cleared per query; OS cache uncontrolled",
              "cache_bytes": args.cache_bytes, "queries": rows,
              "median_off_p50_ms": statistics.median(r["off_p50_ms"] for r in rows),
              "median_on_p50_ms": statistics.median(r["on_p50_ms"] for r in rows)}
    (out / "comparison.json").write_text(json.dumps(result, indent=2), encoding="utf-8")
    table = markdown(rows)
    (out / "comparison.md").write_text(table, encoding="utf-8")
    print(table)
    if any(r["values_match"] is False for r in rows):
        raise SystemExit(1)


if __name__ == "__main__":
    main()
