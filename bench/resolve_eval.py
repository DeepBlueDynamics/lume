#!/usr/bin/env python3
"""Evaluate ti_resolve through an existing loopback Lume HTTP MCP endpoint."""
import argparse
import hashlib
import json
from pathlib import Path
import time
from agent_mcp_run import MCP, write_json

ROOT = Path(__file__).resolve().parents[1]


def load_phrases(path):
    data = json.loads(Path(path).read_text(encoding="utf-8"))
    entries = data["entries"]
    if len(entries) != 100 or len({e["id"] for e in entries}) != 100:
        raise ValueError("Require exactly 100 distinct phrase ids")
    if len({e["phrase"] for e in entries}) != 100:
        raise ValueError("Require 100 distinct phrases")
    if sum(e["split"] == "holdout" for e in entries) != 30:
        raise ValueError("Require 30 predeclared holdout phrases")
    if any(e["split"] not in ("development", "holdout") or not e["hidden"]["expected_paths"]
           or not isinstance(e["phrase"], str) or not e["phrase"].strip() for e in entries):
        raise ValueError("Invalid phrase, split or expectation")
    return entries


def candidates(result):
    if result.get("isError"):
        raise ValueError("MCP ti_resolve returned an error")
    if isinstance(result.get("structuredContent"), dict):
        value = result["structuredContent"]
    else:
        blocks = [b["text"] for b in result.get("content", []) if b.get("type") == "text"]
        if len(blocks) != 1:
            raise ValueError("Expected one JSON text block")
        value = json.loads(blocks[0])
    found = value.get("candidates")
    if not isinstance(found, list):
        raise ValueError("ti_resolve reply lacks candidates")
    if any(not isinstance(c.get("path"), str) or not isinstance(c.get("column"), str) for c in found):
        raise ValueError("Invalid candidate metadata")
    return found


def grade(entry, found, k):
    expected = entry["hidden"]
    return any(c["path"] in expected["expected_paths"] and
               ("expected_agg" not in expected or c.get("agg") == expected["expected_agg"])
               for c in found[:k])


def metrics(records):
    result = {}
    for split in ("all", "development", "holdout"):
        selected = [r for r in records if split == "all" or r["split"] == split]
        n = len(selected)
        top1 = sum(r["top1"] for r in selected)
        top3 = sum(r["top3"] for r in selected)
        result[split] = {"total": n, "top1": top1, "top3": top3,
                         "top1_percent": 100*top1/n if n else 0,
                         "top3_percent": 100*top3/n if n else 0,
                         "errors": sum("error" in r for r in selected)}
    return result


def evaluate(mcp, entries, save=None):
    if "ti_resolve" not in {t["name"] for t in mcp.tools()}:
        raise ValueError("MCP server does not provide ti_resolve")
    records = []
    for entry in entries:
        # Only public text and a fixed limit reach the resolver. No labels/splits.
        arguments = {"phrase": entry["phrase"], "limit": 3}
        started = time.perf_counter()
        record = {"id": entry["id"], "split": entry["split"], "phrase": entry["phrase"],
                  "expected": entry["hidden"], "top1": False, "top3": False,
                  "candidates": []}
        try:
            found = candidates(mcp.rpc("tools/call", {"name": "ti_resolve", "arguments": arguments}))
            record.update(candidates=found, top1=grade(entry, found, 1), top3=grade(entry, found, 3))
        except (ValueError, RuntimeError, KeyError, TypeError, json.JSONDecodeError) as error:
            record["error"] = str(error)
        record["elapsed_ms"] = round((time.perf_counter()-started)*1000, 3)
        records.append(record)
        if save:
            save(records)
    return records


def markdown(records, show_holdout=False):
    lines = ["| Split | N | Top 1 | Top 3 | Errors |",
             "|---|---:|---:|---:|---:|"]
    for split, m in metrics(records).items():
        lines.append(f"| {split} | {m['total']} | {m['top1']} ({m['top1_percent']:.1f}%) | "
                     f"{m['top3']} ({m['top3_percent']:.1f}%) | {m['errors']} |")
    lines += ["", "Top-3 misses:", ""]
    for r in records:
        if r["top3"] or (r["split"] == "holdout" and not show_holdout):
            continue
        got = ", ".join(c["column"] for c in r["candidates"]) or r.get("error", "(no candidates)")
        lines.append(f"- {r['id']}: {r['phrase']} → {got}; expected {r['expected']['expected_paths']}")
    if not show_holdout:
        lines.append("\nHoldout miss details withheld until final evaluation (--show-holdout).")
    return "\n".join(lines) + "\n"


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--mcp-url", required=True)
    p.add_argument("--phrases", type=Path, default=ROOT / "tests/golden/resolve_phrases.json")
    p.add_argument("--output", type=Path, required=True)
    p.add_argument("--label", required=True, help="binary/source revision")
    p.add_argument("--timeout", type=float, default=180)
    p.add_argument("--split", choices=["all", "development", "holdout"], default="all")
    p.add_argument("--show-holdout", action="store_true")
    args = p.parse_args(argv)
    if args.timeout <= 0:
        p.error("timeout must be positive")
    entries = load_phrases(args.phrases)
    entries = [e for e in entries if args.split == "all" or e["split"] == args.split]
    mcp = MCP(args.mcp_url, args.timeout)
    args.output.mkdir(parents=True, exist_ok=False)
    manifest = {"label": args.label, "mcp_url": mcp.url, "split": args.split,
                "fixture_sha256": hashlib.sha256(args.phrases.read_bytes()).hexdigest()}
    records = evaluate(mcp, entries, lambda rs: write_json(args.output / "results.json", {**manifest, "records": rs, "metrics": metrics(rs)}))
    report = markdown(records, args.show_holdout)
    (args.output / "summary.md").write_text(report, encoding="utf-8")
    print(report)
    return 0 if metrics(records)["all"]["top3_percent"] >= 90 else 1


if __name__ == "__main__":
    raise SystemExit(main())
