"""Stage 0: fresh TREC index plus process-cold opens; source/index mounts read-only."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import resource
import subprocess
import time


def records(stderr):
    return [json.loads(line[len("LUME_TIMING "):])
            for line in stderr.splitlines() if line.startswith("LUME_TIMING ")]


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--binary", type=Path, required=True)
    p.add_argument("--output", type=Path, required=True)
    p.add_argument("--source", type=Path, default=Path("/indexes/trec-covid-files"))
    p.add_argument("--existing", type=Path, default=Path("/indexes/hot-path/trec-covid/stemmed"))
    p.add_argument("--new-db", type=Path, default=Path("/measuredb/trec-stage0"))
    a = p.parse_args()
    if a.new_db.exists():
        raise RuntimeError("Refusing to reuse a stage-0 build output")
    a.output.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, LUME_TIMING="1", LUME_STEM="1", LUME_QUERY_INVERSION="0")
    started = time.perf_counter()
    with (a.output / "index.stdout.log").open("wb") as out, (a.output / "index.stderr.log").open("wb") as err:
        result = subprocess.run([str(a.binary), "index", str(a.source), "--db", str(a.new_db)],
                                env=env, stdout=out, stderr=err)
    if result.returncode:
        raise RuntimeError("Stage-0 indexing failed; inspect index.stderr.log")
    build = {"seconds": time.perf_counter() - started,
             "peak_child_rss_bytes": resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss * 1024,
             "timings": records((a.output / "index.stderr.log").read_text())}
    if not build["timings"]:
        raise RuntimeError("Missing index diagnostics")
    opens = []
    # No OS cache flush. Each query is a fresh process, not a resident warm query.
    for index in (a.existing, a.new_db):
        command = [str(a.binary), "search", "--db", str(index),
                   "--alpha", "0", "--graph", "0", "coronavirus vaccine"]
        baseline = subprocess.run(command, env=dict(env, LUME_TIMING="0"), capture_output=True, check=True)
        for iteration in range(3):
            started = time.perf_counter()
            result = subprocess.run(command, env=env, capture_output=True, check=True)
            if result.stdout != baseline.stdout:
                raise RuntimeError("Timing changed query stdout")
            name = ("existing" if index == a.existing else "new") + "-" + str(iteration)
            (a.output / (name + ".stderr.log")).write_bytes(result.stderr)
            timings = records(result.stderr.decode())
            if not any(row["phase"] == "open.total" for row in timings):
                raise RuntimeError("Missing cold-open diagnostics")
            opens.append({"index": str(index), "iteration": iteration,
                          "process_seconds": time.perf_counter() - started, "timings": timings})
    report = {"binary_sha256": hashlib.sha256(a.binary.read_bytes()).hexdigest(),
              "profile": "rustc 1.96 release thin LTO CGU1",
              "storage": "Linux Docker volumes; source and existing index read-only",
              "cold_definition": "fresh process, page cache not flushed; baseline primes pages",
              "phase_accounting": "BM25 total includes tokenize; parse_reconstruct = serde wall minus underlying reads; serialize = serde/flush wall minus underlying writes; do not sum parent and child spans",
              "build": build, "opens": opens}
    (a.output / "summary.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"build_seconds": build["seconds"], "opens": len(opens),
                      "output": str(a.output)}, indent=2), flush=True)


if __name__ == "__main__":
    main()
