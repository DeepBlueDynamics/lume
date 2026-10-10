"""Attribute existing JSON/v4 cold opens without rebuilding either index."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time

def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--binary", type=Path, required=True)
    p.add_argument("--json-db", type=Path, default=Path("/indexes/hot-path/trec-covid/stemmed"))
    p.add_argument("--v4-db", type=Path, default=Path("/measuredb/trec-v4"))
    p.add_argument("--output", type=Path, required=True)
    a = p.parse_args()
    a.output.mkdir(parents=True, exist_ok=False)
    env = dict(os.environ, LUME_STEM="1", LUME_QUERY_INVERSION="0")
    for key in ("LUME_INDEX_FORMAT", "LUME_EMBED_MODEL", "LUME_EMBED_DIMENSIONS"):
        env.pop(key, None)
    samples = []
    for label, db in (("json", a.json_db), ("v4", a.v4_db)):
        command = [str(a.binary), "search", "--db", str(db), "--alpha", "0", "--graph", "0",
                   "coronavirus vaccine"]
        baseline = subprocess.run(command, env=dict(env, LUME_TIMING="0"), capture_output=True, check=True)
        for iteration in range(3):
            started = time.perf_counter()
            result = subprocess.run(command, env=dict(env, LUME_TIMING="1"), capture_output=True, check=True)
            elapsed = time.perf_counter() - started
            if result.stdout != baseline.stdout:
                raise RuntimeError("Timing changed CLI reply")
            (a.output / f"{label}-{iteration}.stderr.log").write_bytes(result.stderr)
            phases = [json.loads(line[len("LUME_TIMING "):]) for line in result.stderr.decode().splitlines()
                      if line.startswith("LUME_TIMING ")]
            if not any(row["phase"] == "open.total" for row in phases):
                raise RuntimeError("Missing open diagnostics")
            if label == "v4" and not any(row["phase"].startswith("v4.decode.") for row in phases):
                raise RuntimeError("Binary has no v4 attribution spans")
            samples.append({"format": label, "iteration": iteration,
                            "process_seconds": elapsed, "timings": phases})
    report = {"binary_sha256": hashlib.sha256(a.binary.read_bytes()).hexdigest(),
              "profile": "rustc 1.96 release thin LTO CGU1",
              "definition": "fresh processes, page cache primed, no cache flush, existing immutable indexes",
              "samples": samples}
    (a.output / "summary.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2), flush=True)

if __name__ == "__main__":
    main()
