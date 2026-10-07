#!/usr/bin/env python3
"""Sequential D45 builds; artifacts outside target, target cleaned before each build."""
import argparse
import json
import os
import resource
import signal
from pathlib import Path
import shutil
import subprocess
import time

from binary_size import ROOT, measure, write

VARIANTS = {
    "thin": {"lto": "thin", "codegen_units": 1, "panic": "unwind"},
    "fat": {"lto": "fat", "codegen_units": 1, "panic": "unwind"},
    "abort": {"lto": "fat", "codegen_units": 1, "panic": "abort"},
    "small": {"lto": "fat", "codegen_units": 1, "panic": "unwind",
              "small_packages": ["pgwire", "tokio", "chrono", "zip", "lopdf",
                                 "quick-xml", "ureq"]},
    "thin-cgu16": {"lto": "thin", "codegen_units": 16, "panic": "unwind"},
}


def directory_bytes(root):
    total = 0
    for directory, _, files in os.walk(root):
        for name in files:
            try:
                total += (Path(directory) / name).stat().st_size
            except FileNotFoundError:
                pass
    return total


def build(label, output):
    configuration = VARIANTS[label]
    output = output.resolve()
    directory = output / label
    directory.mkdir(parents=True, exist_ok=True)
    scratch = ROOT / ".test-tmp/binary-size-build"
    scratch.mkdir(parents=True, exist_ok=True)
    overrides = dict(CARGO_INCREMENTAL="0", CARGO_BUILD_JOBS="2",
                       TMPDIR=str(scratch), CARGO_TARGET_TMPDIR=str(scratch),
                       CARGO_PROFILE_RELEASE_STRIP="symbols",
                       CARGO_PROFILE_RELEASE_OPT_LEVEL="3",
                       CARGO_PROFILE_RELEASE_LTO=configuration["lto"],
                       CARGO_PROFILE_RELEASE_CODEGEN_UNITS=str(configuration["codegen_units"]),
                       CARGO_PROFILE_RELEASE_PANIC=configuration["panic"])
    environment = dict(os.environ, **overrides)
    subprocess.run(["cargo", "clean"], cwd=ROOT, env=environment, check=True)
    cargo = ["cargo"]
    for package in configuration.get("small_packages", []):
        cargo += ["--config", f'profile.release.package.{json.dumps(package)}.opt-level="s"']
    commands = [
        cargo + ["build", "--locked", "--release", "--features", "ti", "--bin", "lume"],
        cargo + ["build", "--locked", "--release", "-p", "ti-bench", "--bin", "ti-bench"],
    ]
    start = time.perf_counter()
    peak = 0
    stages = []
    # Preserve the root-only production binary before resolving benchmark features.
    for stage, args in zip(("lume", "ti-bench"), commands):
        stage_start = time.perf_counter()
        with (directory / (stage + "-build.log")).open("w", encoding="utf-8") as log:
            process = subprocess.Popen(args, cwd=ROOT, env=environment,
                                       stdout=log, stderr=subprocess.STDOUT,
                                       start_new_session=True)
            while process.poll() is None:
                used = directory_bytes(ROOT / "target")
                peak = max(peak, used)
                if used >= 8_000_000_000:
                    os.killpg(process.pid, signal.SIGTERM)
                    process.wait()
                    raise RuntimeError("Target reached 8 GB; build stopped")
                print(f"{label}/{stage}: build running, target={used / 1e9:.2f} GB", flush=True)
                try:
                    process.wait(timeout=30)
                except subprocess.TimeoutExpired:
                    pass
        peak = max(peak, directory_bytes(ROOT / "target"))
        stages.append({"stage": stage, "command": args,
                       "elapsed_seconds": time.perf_counter() - stage_start,
                       "exit_code": process.returncode,
                       "max_child_rss_bytes_cumulative": resource.getrusage(
                           resource.RUSAGE_CHILDREN).ru_maxrss * 1024})
        write(directory / "build.json", {"configuration": configuration, "stages": stages,
              "environment_overrides": overrides,
              "elapsed_seconds": time.perf_counter() - start, "target_peak_bytes": peak})
        if process.returncode:
            raise RuntimeError(f"Build failed: {directory / (stage + '-build.log')}")
        if stage == "lume":
            measure(ROOT / "target/release/lume", output, label)
        else:
            shutil.copy2(ROOT / "target/release/ti-bench", directory / "ti-bench")
    subprocess.run(["cargo", "clean"], cwd=ROOT, env=environment, check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--variant", choices=VARIANTS, required=True)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    build(args.variant, args.output)


if __name__ == "__main__":
    main()
