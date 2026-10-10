"""Paired serial/parallel v4 opens of one frozen index, plus full MCP byte parity."""
import argparse
import csv
import hashlib
import json
import os
from pathlib import Path
import statistics
import subprocess
import time

from measure_v4_snapshot import resident, timings


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--db", type=Path, required=True)
    parser.add_argument("--queries", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--iterations", type=int, default=5)
    args = parser.parse_args()
    if args.iterations < 1:
        parser.error("--iterations must be positive")
    args.output.mkdir(parents=True, exist_ok=False)
    base_env = dict(os.environ, LUME_STEM="1", LUME_QUERY_INVERSION="0")
    for key in ("LUME_INDEX_FORMAT", "LUME_EMBED_MODEL", "LUME_EMBED_DIMENSIONS",
                "LUME_OPEN_THREADS", "LUME_TIMING"):
        base_env.pop(key, None)
    modes = {"serial": dict(base_env, LUME_OPEN_THREADS="1"), "parallel": base_env}
    command = [str(args.binary), "search", "--db", str(args.db),
               "--alpha", "0", "--graph", "0", "coronavirus vaccine"]
    baseline = None
    for env in modes.values():
        reply = subprocess.run(command, env=dict(env, LUME_TIMING="0"),
                               capture_output=True, check=True, timeout=120)
        if baseline is not None and baseline != reply.stdout:
            raise RuntimeError("Untimed serial/parallel CLI byte parity failed")
        baseline = reply.stdout
    samples = []
    for iteration in range(args.iterations):
        for mode, env in modes.items():
            started = time.perf_counter()
            reply = subprocess.run(command, env=dict(env, LUME_TIMING="1"),
                                   capture_output=True, check=True, timeout=120)
            elapsed = time.perf_counter() - started
            log = args.output / f"{iteration}-{mode}.stderr.log"
            log.write_bytes(reply.stderr)
            if reply.stdout != baseline:
                raise RuntimeError("Timed CLI reply byte parity failed")
            phases = timings(reply.stderr.decode())
            totals = [row for row in phases if row["phase"] == "open.total"]
            if len(totals) != 1 or "memory" not in totals[0]:
                raise RuntimeError("Require one open.total with Linux peak RSS")
            samples.append({"iteration": iteration, "mode": mode,
                            "process_seconds": elapsed, "open_ms": totals[0]["ms"],
                            "peak_rss_bytes": totals[0]["memory"][1], "timings": phases})
    with args.queries.open(encoding="utf-8") as stream:
        queries = list(csv.reader(stream, delimiter="\t"))
    if not queries:
        raise ValueError("Empty query set")
    fingerprints = {}
    for floor in ("0.5", "1.0"):
        expected = None
        for mode, env in modes.items():
            replies, _ = resident(args.binary, args.db, queries,
                                  dict(env, LUME_COORD_FLOOR=floor, LUME_TIMING="0"),
                                  args.output / f"{floor}-{mode}.server.log", 0)
            (args.output / f"{floor}-{mode}.replies.json").write_text(
                json.dumps({qid: raw.decode() for qid, raw in replies.items()}) + "\n")
            fingerprints[f"{floor}-{mode}"] = {
                qid: hashlib.sha256(raw).hexdigest() for qid, raw in replies.items()}
            if expected is not None and expected != replies:
                misses = [qid for qid, _ in queries if expected[qid] != replies[qid]]
                (args.output / "parity-misses.json").write_text(json.dumps(misses) + "\n")
                raise RuntimeError("Full reply byte parity failed: " + str(misses))
            expected = replies
    summaries = {}
    for mode in modes:
        rows = [row for row in samples if row["mode"] == mode]
        summaries[mode] = {
            "open_median_ms": statistics.median(row["open_ms"] for row in rows),
            "peak_rss_median_bytes": statistics.median(row["peak_rss_bytes"] for row in rows),
            "process_median_seconds": statistics.median(row["process_seconds"] for row in rows)}
    report = {"binary_sha256": hashlib.sha256(args.binary.read_bytes()).hexdigest(),
              "storage": "one frozen Linux-volume index; read-only source",
              "cold_definition": "fresh process, primed page cache; not OS-cache cold",
              "order": "serial then parallel, alternating, five pairs by default",
              "threads": {"serial": 1, "parallel": "min(4, available_parallelism)"},
              "summary": summaries, "samples": samples, "reply_sha256": fingerprints,
              "reply_byte_parity": True}
    (args.output / "summary.json").write_text(json.dumps(report, indent=2) + "\n")
    print("| mode | open median ms | peak RSS median MB |")
    print("|---|---:|---:|")
    for mode, row in summaries.items():
        print(f"| {mode} | {row['open_median_ms']:.2f} | {row['peak_rss_median_bytes']/1e6:.2f} |")
    print("Full reply byte parity: PASS (coordination floors 0.5 and 1.0)")


if __name__ == "__main__":
    main()
