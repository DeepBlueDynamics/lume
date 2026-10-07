#!/usr/bin/env python3
"""Reproducible D45 build-size/feature measurements; Python stdlib only."""
import argparse
import gzip
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import statistics
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
QUERY_IDS = [
    "q1-001", "q1-002", "q1-004", "q2-002", "q2-003", "q2-005",
    "q3-001", "q3-002", "q3-004", "q4-001", "q4-002", "q4-005",
    "q5-001", "q5-002", "q7-001", "q7-002", "q7-006",
    "q8-001", "q8-002", "q8-006",
]


def command(args):
    return subprocess.check_output(args, cwd=ROOT, text=True, env=dict(os.environ, CARGO_INCREMENTAL="0"))


def write(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")


def gzip_binary(source, destination):
    digest = hashlib.sha256()
    with source.open("rb") as incoming, destination.open("wb") as raw:
        with gzip.GzipFile(filename="", mode="wb", fileobj=raw, compresslevel=9, mtime=0) as outgoing:
            while chunk := incoming.read(1024 * 1024):
                digest.update(chunk)
                outgoing.write(chunk)
    return digest.hexdigest()


def feature_audit(out):
    out.mkdir(parents=True, exist_ok=True)
    tree_args = ["cargo", "tree", "--locked", "-e", "features", "-p", "ti-sql"]
    tree = command(tree_args)
    (out / "ti-sql-feature-tree.txt").write_text(tree, encoding="utf-8")
    compact_args = ["cargo", "tree", "--locked", "-e", "normal", "-p", "ti-sql",
                    "--format", "{p} features=[{f}]", "--prefix", "none"]
    compact = command(compact_args)
    (out / "ti-sql-feature-union.txt").write_text(compact, encoding="utf-8")
    metadata = json.loads(command(["cargo", "metadata", "--locked", "--features", "ti", "--format-version", "1"]))
    packages = {p["id"]: p for p in metadata["packages"]}
    enabled = {n["id"]: n["features"] for n in metadata["resolve"]["nodes"]}
    relevant = []
    for package in packages.values():
        if package["name"].startswith(("datafusion", "arrow")) or package["name"] == "parquet":
            relevant.append({"name": package["name"], "version": package["version"],
                             "feature_definitions": package["features"],
                             "enabled": enabled.get(package["id"], []),
                             "dependencies": [{k: d[k] for k in (
                                 "name", "req", "uses_default_features", "features", "optional", "kind")}
                                              for d in package["dependencies"]]})
    write(out / "feature-audit.json", {"commands": [tree_args, compact_args],
                                      "packages": relevant})
    for line in compact.splitlines():
        if line.startswith(("datafusion v", "arrow v", "arrow-ipc v", "parquet v")) and not line.endswith("(*)"):
            print(line)
    return relevant


def measure(binary, output, label):
    output.mkdir(parents=True, exist_ok=True)
    stored = output / label / binary.name
    stored.parent.mkdir(parents=True, exist_ok=True)
    if binary.resolve() != stored.resolve():
        shutil.copy2(binary, stored)
    stripped = command(["readelf", "-SW", str(stored)])
    if ".symtab" in stripped or ".debug_" in stripped or ".zdebug_" in stripped:
        raise RuntimeError("Input must be symbol/debug stripped")
    start = time.perf_counter()
    gzip_path = stored.with_suffix(stored.suffix + ".gz")
    digest = gzip_binary(stored, gzip_path)
    result = {
        "label": label, "binary": str(stored), "bytes": stored.stat().st_size,
        "gzip_bytes": gzip_path.stat().st_size, "gzip_level": 9, "sha256": digest,
        "gzip_ms": (time.perf_counter() - start) * 1000,
        "rustc": command(["rustc", "--version"]).strip(),
        "cargo": command(["cargo", "--version"]).strip(),
        "git_sha": command(["git", "rev-parse", "HEAD"]).strip(),
        "platform": platform.platform(),
        "version": command([str(stored), "--version"]).strip(),
    }
    write(stored.parent / "size.json", result)
    print(json.dumps(result, indent=2))
    return result


def corpus(path):
    fixture = json.loads((ROOT / "tests/golden/corpus.json").read_text(encoding="utf-8"))
    entries = {entry["id"]: entry for entry in fixture["entries"]}
    if any(entries[qid].get("exclude") for qid in QUERY_IDS):
        raise RuntimeError("Timing set contains excluded query")
    fixture["entries"] = [entries[qid] for qid in QUERY_IDS]
    write(path, fixture)
    print(f"20 fixed golden queries written to {path}")


def benchmark(binary, store, output, label, iterations):
    directory = output / label
    directory.mkdir(parents=True, exist_ok=True)
    stored = directory / "ti-bench"
    if binary.resolve() != stored.resolve():
        shutil.copy2(binary, stored)
    timing_corpus = output / "timing-corpus.json"
    corpus(timing_corpus)
    args = [sys.executable, str(ROOT / "bench/shard_cache.py"),
            "--binary", str(stored), "--store", str(store),
            "--corpus", str(timing_corpus), "--out", str(directory / "timing"),
            "--iterations", str(iterations), "--cache-mode", "on",
            "--cache-bytes", str(256 * 1024 * 1024),
            "--toolchain", command(["rustc", "--version"]).strip() + " release " + label]
    # Deep lane scratch preserves lane git provenance without locating a DuckDB runner.
    working = ROOT / ".test-tmp/binary-size" / label
    working.mkdir(parents=True, exist_ok=True)
    environment = dict(os.environ, CARGO_INCREMENTAL="0", TI_OPT_IN="last",
                       TMPDIR=str(working), CARGO_TARGET_TMPDIR=str(working))
    start = time.perf_counter()
    with (directory / "timing.log").open("w", encoding="utf-8") as log:
        result = subprocess.run(args, cwd=working, env=environment, stdout=log,
                                stderr=subprocess.STDOUT)
    write(directory / "timing-command.json", {"command": args, "cwd": str(working),
          "elapsed_seconds": time.perf_counter() - start, "exit_code": result.returncode})
    if result.returncode:
        raise RuntimeError("Benchmark failed; see timing.log")
    print(f"Completed 20-query benchmark: {directory}")


def compare(output, baseline, quiet=False):
    files = sorted(output.glob("*/timing/cache-on.json"))
    reports = {}
    for path in files:
        report = json.loads(path.read_text(encoding="utf-8"))
        label = path.parent.parent.name
        reports[label] = {q["id"]: q for q in report["queries"]}
    if baseline not in reports:
        raise RuntimeError("Baseline report missing")
    reference = reports[baseline]
    rows = []
    for label, queries in reports.items():
        if set(queries) != set(QUERY_IDS):
            raise RuntimeError(f"Timing query set mismatch: {label}")
        control_path = output / label / "timing-fat-control/cache-on.json"
        paired = reference
        if baseline == "fat" and label != baseline and control_path.exists():
            control = json.loads(control_path.read_text(encoding="utf-8"))
            paired = {q["id"]: q for q in control["queries"]}
            if set(paired) != set(QUERY_IDS):
                raise RuntimeError(f"Control query set mismatch: {label}")
            for qid in QUERY_IDS:
                if (paired[qid]["rows"], paired[qid]["answer_fingerprint"]) != (
                        reference[qid]["rows"], reference[qid]["answer_fingerprint"]):
                    raise RuntimeError(f"Control answer changed: {label}/{qid}")
        metrics = []
        for qid in QUERY_IDS:
            original, actual = paired[qid], queries[qid]
            if (actual["rows"], actual["answer_fingerprint"]) != (original["rows"], original["answer_fingerprint"]):
                raise RuntimeError(f"Query answer changed: {label}/{qid}")
            metrics.append({"id": qid, "baseline_p50_ms": original["p50_ms"],
                            "p50_ms": actual["p50_ms"],
                            "change_percent": (actual["p50_ms"] / original["p50_ms"] - 1) * 100})
        classes = []
        for name in sorted({q["id"].split("-")[0] for q in metrics}):
            members = [q for q in metrics if q["id"].split("-")[0] == name]
            before = statistics.median(q["baseline_p50_ms"] for q in members)
            after = statistics.median(q["p50_ms"] for q in members)
            change = (after / before - 1) * 100
            classes.append({"class": name, "baseline_p50_ms": before,
                            "p50_ms": after, "change_percent": change,
                            "change_ms": after - before,
                            "material_regression": change > 5 + 1e-9 and after - before > 1})
        aggregate_change = (sum(q["p50_ms"] for q in metrics) /
                            sum(q["baseline_p50_ms"] for q in metrics) - 1) * 100
        flags = [q["id"] for q in metrics if q["change_percent"] > 5 + 1e-9
                 and q["p50_ms"] - q["baseline_p50_ms"] > 1]
        rows.append({"label": label, "queries": metrics, "classes": classes,
                     "material_query_regressions": flags,
                     "passes_clarified_gate": aggregate_change <= 5 + 1e-9
                         and not any(c["material_regression"] for c in classes),
                     "baseline_kind": "adjacent fat control" if paired is not reference else baseline,
                     "baseline_median_p50_ms": statistics.median(q["p50_ms"] for q in paired.values()),
                     "max_query_regression_percent": max(q["change_percent"] for q in metrics),
                     "median_p50_ms": statistics.median(q["p50_ms"] for q in metrics),
                     "all_values_match": True,
                     "within_five_percent_per_query": all(q["change_percent"] <= 5 + 1e-9 for q in metrics),
                     "sum_p50_change_percent": (
                         sum(q["p50_ms"] for q in metrics) /
                         sum(q["baseline_p50_ms"] for q in metrics) - 1) * 100})
    write(output / "comparison.json", {"baseline": baseline, "variants": rows})
    if not quiet:
        print(json.dumps(rows, indent=2))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="action", required=True)
    features = sub.add_parser("features")
    features.add_argument("--output", type=Path, required=True)
    size = sub.add_parser("measure")
    size.add_argument("--binary", type=Path, required=True)
    size.add_argument("--output", type=Path, required=True)
    size.add_argument("--label", required=True)
    queries = sub.add_parser("corpus")
    queries.add_argument("--output", type=Path, required=True)
    bench = sub.add_parser("benchmark")
    bench.add_argument("--binary", type=Path, required=True)
    bench.add_argument("--store", type=Path, required=True)
    bench.add_argument("--output", type=Path, required=True)
    bench.add_argument("--label", required=True)
    bench.add_argument("--iterations", type=int, default=31)
    comparison = sub.add_parser("compare")
    comparison.add_argument("--output", type=Path, required=True)
    comparison.add_argument("--baseline", default="fat")
    comparison.add_argument("--quiet", action="store_true", help="write full comparison JSON without dumping it")
    args = parser.parse_args()
    if args.action == "features":
        feature_audit(args.output)
    elif args.action == "measure":
        if not args.label.replace("-", "").isalnum():
            parser.error("Label must be alphanumeric with optional hyphens")
        measure(args.binary.resolve(), args.output.resolve(), args.label)
    elif args.action == "corpus":
        corpus(args.output.resolve())
    elif args.action == "benchmark":
        if args.iterations < 1 or not args.label.replace("-", "").isalnum():
            parser.error("Positive iterations and a safe label are required")
        benchmark(args.binary.resolve(), args.store.resolve(), args.output.resolve(),
                  args.label, args.iterations)
    else:
        compare(args.output.resolve(), args.baseline, args.quiet)


if __name__ == "__main__":
    main()
