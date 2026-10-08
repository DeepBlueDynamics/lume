#!/usr/bin/env python3
"""Native Windows Rust 1.96 Q1-Q8 cache A/B; writes only inside this lane."""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import platform
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
SCRATCH = ROOT / ".test-tmp/native-q6"
EDGE_TARGETS = {"Q1": 20, "Q2": 150, "Q3": 50, "Q4": 400,
                "Q5": 150, "Q6": 200, "Q7": 300}


def command(args):
    return subprocess.check_output(args, cwd=ROOT, text=True).strip()


def target_bytes():
    total = 0
    for directory, _, names in os.walk(ROOT / "target"):
        for name in names:
            try:
                total += (Path(directory) / name).stat().st_size
            except FileNotFoundError:
                pass
    return total


def checked(args, label):
    SCRATCH.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, CARGO_INCREMENTAL="0", CARGO_BUILD_JOBS="2",
               CARGO_TARGET_TMPDIR=str(SCRATCH), TMPDIR=str(SCRATCH),
               TMP=str(SCRATCH), TEMP=str(SCRATCH))
    print("RUN", subprocess.list2cmdline(args), flush=True)
    with (SCRATCH / (label + ".log")).open("w", encoding="utf-8") as log:
        process = subprocess.Popen(args, cwd=ROOT, env=env, stdout=log,
                                   stderr=subprocess.STDOUT)
        peak = 0
        while process.poll() is None:
            used = target_bytes()
            peak = max(peak, used)
            if used >= 7_800_000_000:
                subprocess.run(["taskkill", "/PID", str(process.pid), "/T", "/F"],
                               check=False)
                process.wait()
                raise RuntimeError("Target approached 8 GB; process tree stopped")
            print(f"{label}: target {used / 1e9:.2f} GB", flush=True)
            try:
                process.wait(timeout=15)
            except subprocess.TimeoutExpired:
                pass
    print(f"{label}: exit {process.returncode}; peak target {peak / 1e9:.2f} GB", flush=True)
    if process.returncode:
        raise RuntimeError(f"{label} failed; see {SCRATCH / (label + '.log')}")


def checks():
    checked(["rustfmt", "--edition", "2021", "--check",
             "crates/ti-bench/src/harness.rs", "crates/ti-query-bench/src/main.rs"], "fmt")
    checked(["cargo", "clippy", "--locked", "-p", "ti-bench", "--all-targets",
             "--", "-D", "warnings"], "clippy-bench")
    checked(["cargo", "clippy", "--locked", "-p", "ti-query-bench",
             "--all-targets", "--no-deps", "--", "-D", "warnings"], "clippy-query-bench")
    checked(["cargo", "test", "--locked", "-p", "ti-bench"], "test-bench")
    checked([sys.executable, "-m", "unittest", "discover", "-s", "bench",
             "-p", "test_shard_cache.py"], "test-python")


def build():
    checked(["cargo", "clean"], "clean-before-release")
    checked(["cargo", "build", "--locked", "--release", "-p", "ti-query-bench"], "release")


def digest(path):
    sha = hashlib.sha256()
    with path.open("rb") as source:
        while chunk := source.read(1 << 20):
            sha.update(chunk)
    return sha.hexdigest()


def measure():
    sha = command(["git", "rev-parse", "HEAD"])
    date = datetime.datetime.now(datetime.timezone.utc).date().isoformat()
    output = SCRATCH / (date + "-" + sha[:7])
    if output.exists():
        raise RuntimeError("Measurement output exists; preserve it and use a fresh run")
    binary = ROOT / "target/release/ti-query-bench.exe"
    rustc = command(["rustc", "--version"])
    checked([sys.executable, "bench/shard_cache.py", "--binary", str(binary),
             "--store", str(ROOT.parent / "data/store-full"),
             "--corpus", "tests/golden/corpus.json", "--out", str(output),
             "--iterations", "7", "--cache-bytes", "268435456",
             "--cache-mode", "both", "--include-q6",
             "--toolchain", rustc + " native Windows release fat LTO / CGU=1"],
            "measure")
    raw = {}
    for mode in ("off", "on"):
        reports = list((output / mode).glob("*.json"))
        if len(reports) != 1:
            raise RuntimeError("Expected one native report per cache mode")
        raw[mode] = json.loads(reports[0].read_text(encoding="utf-8"))
        if raw[mode]["git_sha"] != sha[:7]:
            raise RuntimeError("Native report source commit differs")
        if raw[mode]["total_queries_run"] != 26:
            raise RuntimeError("Expected all 26 selected queries")
    comparison = json.loads((output / "comparison.json").read_text(encoding="utf-8"))
    if not all(q["values_match"] for q in comparison["queries"]):
        raise RuntimeError("Cache A/B answer mismatch")
    classes = {}
    for name in (f"Q{index}" for index in range(1, 9)):
        metrics = {}
        for mode in ("off", "on"):
            queries = [q for q in raw[mode]["queries"] if q["class"] == name]
            if not queries:
                raise RuntimeError("Missing query class " + name)
            metrics[mode] = {
                "cold_ms": max(q["cold_ms"] for q in queries),
                "warm_p50_ms": max(q["p50_ms"] for q in queries),
                "warm_p95_ms": max(q["p95_ms"] for q in queries),
                "warm_p99_ms": max(q["p99_ms"] for q in queries),
                "slowest_p95_query": max(queries, key=lambda q: q["p95_ms"])["id"],
            }
        target = EDGE_TARGETS.get(name)
        metrics["edge_target_p95_ms"] = target
        metrics["status"] = ("PENDING" if target is None else
                             "PASS" if metrics["on"]["warm_p95_ms"] <= target else "MISS")
        classes[name] = metrics
    result = {
        "date": date, "benchmark_commit": sha, "base_commit": "83fcce7",
        "rustc": rustc, "cargo": command(["cargo", "--version"]),
        "platform": platform.platform(), "machine": platform.machine(),
        "profile": {"release": True, "lto": "fat", "codegen_units": 1,
                    "opt_level": 3, "panic": "unwind", "incremental": False},
        "runner_sha256": digest(binary), "iterations": 7,
        "cache_bytes": 268435456, "selected_queries": 26,
        "all_answer_fingerprints_match": True,
        "class_summary": "maximum per-query metric, not pooled class percentile",
        "cold_definition": "sealed bitmap cache cleared per query; OS cache uncontrolled; document indexes/query caches retain their normal lifecycle",
        "caveats": ["Native x64 host, not Pi/HALPI2", "5 vessels x 90 days, not 1 vessel x 1 year",
                    "Seven warm iterations; per-query p95/p99 are the maximum of seven samples",
                    "No DuckDB timing; Q8 parity pending"],
        "classes": classes, "cache_off": raw["off"], "cache_on": raw["on"],
        "comparison": comparison,
    }
    destination = ROOT / "bench/results"
    destination.mkdir(parents=True, exist_ok=True)
    filename = date + "-" + sha[:7]
    (destination / (filename + ".json")).write_text(
        json.dumps(result, indent=2) + "\n", encoding="utf-8")
    lines = [f"# Native Q1-Q8 cache benchmark — {date} ({sha[:7]})", "",
             f"Rust: {rustc}. Native Windows x64 release, fat LTO, CGU=1, unwind.",
             "Store: store-full (5 vessels x 90 days). Seven warm iterations per query.",
             "26 queries, all fingerprints and row counts match cache off/on.",
             "Each class reports its slowest per-query metric; no pooled percentile.",
             "Cold clears only sealed bitmaps; OS and normal document cache lifecycle are uncontrolled.", "",
             "| Class | Edge target p95 ms | Cold off/on ms | Warm p50 off/on ms | Warm p95 off/on ms | Warm p99 off/on ms | Warm-on status |",
             "|---|---:|---:|---:|---:|---:|---|"]
    for name, metric in classes.items():
        before, after = metric["off"], metric["on"]
        pair = lambda field: f"{before[field]:.2f} / {after[field]:.2f}"
        target = metric["edge_target_p95_ms"]
        lines.append(f"| {name} | {target if target else 'DuckDB parity'} | "
                     f"{pair('cold_ms')} | {pair('warm_p50_ms')} | {pair('warm_p95_ms')} | "
                     f"{pair('warm_p99_ms')} | {metric['status']} |")
    lines += ["", "Q8 is pending: no DuckDB baseline. Host measurements do not establish Pi edge p95.",
              "The JSON includes every query, raw class summaries, cache statistics and commands.",
              "", "## Q6 queries", "",
              "| Query | Rows | Cold off/on ms | Warm p50 off/on ms | Warm p95 off/on ms | Warm p99 off/on ms |",
              "|---|---:|---:|---:|---:|---:|"]
    indexed = {q["id"]: q for q in raw["off"]["queries"]}
    for after in raw["on"]["queries"]:
        if after["class"] != "Q6":
            continue
        before = indexed[after["id"]]
        pair = lambda field: f"{before[field]:.2f} / {after[field]:.2f}"
        lines.append(f"| {after['id']} | {after['rows']} | {pair('cold_ms')} | "
                     f"{pair('p50_ms')} | {pair('p95_ms')} | {pair('p99_ms')} |")
    (destination / (filename + ".md")).write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(json.dumps(classes, indent=2), flush=True)
    print("RESULT", destination / (filename + ".json"), flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("stage", choices=("checks", "build", "measure", "clean"))
    args = parser.parse_args()
    if sys.platform != "win32" or not command(["rustc", "--version"]).startswith("rustc 1.96."):
        parser.error("Run on native Windows with Rust 1.96.x")
    {"checks": checks, "build": build, "measure": measure,
     "clean": lambda: checked(["cargo", "clean"], "clean-final")}[args.stage]()


if __name__ == "__main__":
    main()
