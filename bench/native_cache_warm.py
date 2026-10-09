#!/usr/bin/env python3
"""Native Windows cache warm-up checks/measurement; stdlib, lane writes only."""
import argparse
import ctypes
from ctypes import wintypes
import datetime
import json
import os
import platform
import subprocess
import sys
import time
import native_q6 as native

ROOT = native.ROOT
native.SCRATCH = ROOT / ".test-tmp/cache-warm"
SCRATCH = native.SCRATCH
TOUCHED_RUST = [
    "crates/ti-contracts/src/config.rs", "crates/ti-store/src/cache.rs",
    "crates/ti-store/src/lib.rs", "crates/ti-store/src/warm.rs",
    "crates/ti-store/tests/query_cache.rs", "crates/ti-bench/src/harness.rs",
    "crates/ti-query-bench/src/main.rs", "src/ti_http.rs",
]


def checks():
    native.checked(["rustfmt", "--edition", "2021", "--check", *TOUCHED_RUST], "fmt")
    native.checked(["cargo", "clippy", "--locked", "-p", "ti-contracts", "-p",
                    "ti-store", "-p", "ti-bench", "--all-targets", "--", "-D", "warnings"],
                   "clippy-ti")
    native.checked(["cargo", "clippy", "--locked", "-p", "ti-query-bench",
                    "--all-targets", "--no-deps", "--", "-D", "warnings"], "clippy-runner")
    native.checked(["cargo", "test", "--locked", "-p", "ti-store"], "test-store")
    native.checked(["cargo", "test", "--locked", "-p", "ti-bench"], "test-bench")
    native.checked(["cargo", "test", "--locked", "--features", "ti", "--test", "ti_http"], "test-http")
    native.checked([sys.executable, "-m", "unittest", "discover", "-s", "bench",
                    "-p", "test_*cache*.py"], "test-python")


class MemoryCounters(ctypes.Structure):
    _fields_ = [
        ("cb", wintypes.DWORD), ("PageFaultCount", wintypes.DWORD),
        *[(name, ctypes.c_size_t) for name in (
            "PeakWorkingSetSize", "WorkingSetSize", "QuotaPeakPagedPoolUsage",
            "QuotaPagedPoolUsage", "QuotaPeakNonPagedPoolUsage", "QuotaNonPagedPoolUsage",
            "PagefileUsage", "PeakPagefileUsage")],
    ]


def warm_rss(binary, store, budget, output):
    """OS peak includes engine startup; sampled warm-phase RSS reported separately."""
    output.mkdir(parents=True, exist_ok=True)
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    psapi = ctypes.WinDLL("psapi", use_last_error=True)
    kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    kernel.OpenProcess.restype = wintypes.HANDLE
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    psapi.GetProcessMemoryInfo.argtypes = [
        wintypes.HANDLE, ctypes.POINTER(MemoryCounters), wintypes.DWORD]
    psapi.GetProcessMemoryInfo.restype = wintypes.BOOL
    args = [str(binary), "warm", "--store", str(store), "--cache-bytes", str(budget), "--hold-for-rss"]
    log_path = output / "warm.log"
    with log_path.open("w", encoding="utf-8") as log:
        process = subprocess.Popen(args, cwd=ROOT, stdout=log, stderr=subprocess.STDOUT,
                                   stdin=subprocess.PIPE, text=True)
        handle = kernel.OpenProcess(0x1000 | 0x0010, False, process.pid)
        if not handle:
            process.terminate()
            process.wait()
            raise ctypes.WinError(ctypes.get_last_error())
        peak, warm_peak, baseline = 0, 0, None
        try:
            while process.poll() is None:
                counter = MemoryCounters()
                counter.cb = ctypes.sizeof(counter)
                if not psapi.GetProcessMemoryInfo(handle, ctypes.byref(counter), counter.cb):
                    if process.poll() is None:
                        process.terminate()
                        process.wait()
                        raise ctypes.WinError(ctypes.get_last_error())
                    break
                peak = max(peak, counter.PeakWorkingSetSize)
                # Read only the worker log, never modify the shared store.
                text = log_path.read_text(encoding="utf-8")
                if "WARM_BEGIN" in text:
                    if baseline is None:
                        baseline = counter.WorkingSetSize
                    warm_peak = max(warm_peak, counter.WorkingSetSize)
                if "WARM_REPORT " in text:
                    process.stdin.write("\n")
                    process.stdin.flush()
                    process.stdin.close()
                    process.wait(timeout=30)
                    break
                time.sleep(0.05)
        finally:
            kernel.CloseHandle(handle)
    if process.returncode:
        raise RuntimeError(f"Warm-only process failed: {log_path}")
    reports = [line.removeprefix("WARM_REPORT ") for line in
               log_path.read_text(encoding="utf-8").splitlines()
               if line.startswith("WARM_REPORT ")]
    if len(reports) != 1:
        raise RuntimeError("Missing warm-up report")
    report = json.loads(reports[0])
    return dict(report=report, command=args, process_peak_rss_bytes=peak,
                sampled_warm_peak_rss_bytes=warm_peak,
                first_warm_sample_rss_bytes=baseline, sample_interval_ms=50,
                rss_definition="Windows peak working set includes engine startup; warm-phase peak sampled every 50 ms")


def summarize(report, baseline):
    before = {query["id"]: query for query in baseline["cache_on"]["queries"]}
    if len(report["queries"]) != 26 or before.keys() != {q["id"] for q in report["queries"]}:
        raise ValueError("Expected the same 26 selected queries")
    for query in report["queries"]:
        original = before[query["id"]]
        if (query["rows"], query["answer_fingerprint"]) != (original["rows"], original["answer_fingerprint"]):
            raise ValueError("Answer mismatch: " + query["id"])
        warm = query["cache_warm"]
        if warm["bytes"] > warm["budget_bytes"]:
            raise ValueError("Warm-up exceeded its budget")
    classes = {}
    for name in (f"Q{index}" for index in range(1, 9)):
        queries = [q for q in report["queries"] if q["class"] == name]
        cold = max(q["cold_ms"] for q in queries)
        target = native.EDGE_TARGETS.get(name)
        classes[name] = dict(
            cold_after_warm_ms=cold,
            previous_cold_ms=baseline["classes"][name]["on"]["cold_ms"],
            edge_target_ms=target,
            status="PENDING" if target is None else "PASS" if cold <= target else "MISS",
            slowest_query=max(queries, key=lambda q: q["cold_ms"])["id"])
    return classes


def measure():
    overrides = [key for key in os.environ if os.environ[key] and (
        key in ("RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS") or key.startswith("CARGO_PROFILE_RELEASE_"))]
    if overrides:
        raise RuntimeError("Release-profile overrides are active: " + ", ".join(overrides))
    compilers = native.command([
        "pwsh", "-NoProfile", "-Command",
        "Get-Process -Name cargo,rustc,link -ErrorAction SilentlyContinue | Select-Object -ExpandProperty ProcessName; exit 0",
    ])
    if compilers:
        raise RuntimeError("Compiler/link processes are active; measure after they finish: " + compilers)
    sha = native.command(["git", "rev-parse", "HEAD"])
    date = datetime.datetime.now(datetime.timezone.utc).date().isoformat()
    root = SCRATCH / (date + "-" + sha[:7])
    if root.exists():
        raise RuntimeError("Use a fresh measurement directory")
    binary = ROOT / "target/release/ti-query-bench.exe"
    store = ROOT.parent / "data/store-full"
    rustc = native.command(["rustc", "--version"])
    baseline_file = ROOT / "bench/results/2026-10-08-0eff8df.json"
    baseline = json.loads(baseline_file.read_text(encoding="utf-8"))
    measurements = {}
    for budget in (268435456, 67108864):
        folder = root / str(budget)
        rss = warm_rss(binary, store, budget, folder / "memory")
        native.checked([
            sys.executable, "bench/shard_cache.py", "--binary", str(binary),
            "--store", str(store), "--corpus", "tests/golden/corpus.json",
            "--out", str(folder / "queries"), "--iterations", "7",
            "--cache-bytes", str(budget), "--cache-mode", "on",
            "--include-q6", "--warm-before-cold", "--toolchain", rustc,
        ], "measure-" + str(budget))
        files = list((folder / "queries/on").glob("*.json"))
        if len(files) != 1:
            raise RuntimeError("Expected one benchmark report")
        report = json.loads(files[0].read_text(encoding="utf-8"))
        if report["git_sha"] != sha[:7]:
            raise RuntimeError("Unexpected benchmark source commit")
        measurements[str(budget)] = dict(
            warmup_memory=rss, classes=summarize(report, baseline), raw=report)
    result = dict(
        date=date, benchmark_commit=sha, base_commit="5750e1d",
        rustc=rustc, platform=platform.platform(), machine=platform.machine(),
        profile=dict(release=True, lto="fat", codegen_units=1, opt_level=3,
                     panic="unwind", incremental=False),
        runner_sha256=native.digest(binary), iterations=7, selected_queries=26,
        baseline_file=str(baseline_file.relative_to(ROOT)),
        all_answer_fingerprints_match=True,
        cold_definition="sealed cache cleared; default newest-first all-field warm-up awaited before each first query; OS and document cache lifecycle uncontrolled",
        caveats=[
            "Native Windows x64, not Pi; 5 vessels x 90 days, not 1 vessel x 1 year",
            "One cold-after-warm observation per query, not a cold p95 distribution",
            "Server startup does not await warm-up; requests arriving before completion can remain cold",
            "Retained cache is byte-bounded; one field decoder's temporary allocations are additional",
            "Historical queries need not hit newest-first warm-up; no benchmark-specific fields selected",
            "Warm-only RSS covers the benchmark engine/cache process, not a complete server or Pi RSS",
            "Q8 DuckDB parity pending",
        ], measurements=measurements)
    filename = date + "-" + sha[:7]
    destination = ROOT / "bench/results"
    (destination / (filename + ".json")).write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    lines = [f"# Native cold after warm-up — {date} ({sha[:7]})", "",
             f"{rustc}; native Windows x64 release fat LTO / CGU=1 / unwind.",
             "26 queries, 7 subsequent warm iterations; all row counts/fingerprints equal the cache A/B baseline.",
             "Newest-first, all fields; no query-specific tuning. Each cold query follows a cleared cache and completed preload.",
             "Class figures are maximum per-query first-query times, not cold p95; OS cache uncontrolled.", "",
             "| Class | Edge target ms | Previous cold ms | After warm-up 256 MiB ms | Status | After warm-up 64 MiB ms | Status |",
             "|---|---:|---:|---:|---|---:|---|"]
    large, small = (measurements[str(n)]["classes"] for n in (268435456, 67108864))
    for name in large:
        a, b = large[name], small[name]
        lines.append(f"| {name} | {a['edge_target_ms'] or 'DuckDB parity'} | {a['previous_cold_ms']:.2f} | {a['cold_after_warm_ms']:.2f} | {a['status']} | {b['cold_after_warm_ms']:.2f} | {b['status']} |")
    lines += ["", "| Budget | Shards / fields | Retained MiB | Warm-up ms | Process peak RSS MiB | Sampled warm-phase peak RSS MiB |",
              "|---|---:|---:|---:|---:|---:|"]
    for budget, measurement in measurements.items():
        memory = measurement["warmup_memory"]
        warm = memory["report"]
        lines.append(f"| {int(budget) // 1048576} MiB | {warm['shards']} / {warm['fields']} | {warm['bytes']/1048576:.2f} | {warm['elapsed_ms']:.2f} | {memory['process_peak_rss_bytes']/1048576:.2f} | {memory['sampled_warm_peak_rss_bytes']/1048576:.2f} |")
    lines += ["", "Process peak is the Windows peak working set of the warm-only benchmark runner including engine startup; warm-phase RSS is sampled every 50 ms. This is not complete server or Pi RSS.",
              "The server answers immediately without joining warm-up; these timings describe queries after it completes.",
              "Default warming does not promise historical cold-start targets. See JSON for every query, warm-up counters and caveats."]
    (destination / (filename + ".md")).write_text("\n".join(lines) + "\n", encoding="utf-8")
    print("\n".join(lines), flush=True)
    print("RESULT", destination / (filename + ".json"), flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("stage", choices=("checks", "build", "measure", "clean"))
    args = parser.parse_args()
    if sys.platform != "win32" or not native.command(["rustc", "--version"]).startswith("rustc 1.96."):
        parser.error("Run on native Windows with Rust 1.96.x")
    {"checks": checks, "build": native.build, "measure": measure,
     "clean": lambda: native.checked(["cargo", "clean"], "clean-final")}[args.stage]()


if __name__ == "__main__":
    main()
