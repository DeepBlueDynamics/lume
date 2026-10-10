"""Use the existing matched MCP latency/QPS harness and enforce ranking parity."""
import argparse
import hashlib
import json
from pathlib import Path
from run_lume import benchmark

parser = argparse.ArgumentParser()
parser.add_argument("--root", type=Path, required=True)
parser.add_argument("--label", required=True)
parser.add_argument("--dataset", required=True)
parser.add_argument("--db", required=True)
parser.add_argument("--baseline", required=True)
args = parser.parse_args()
token = (args.root / "http-token.txt").read_text().strip()
benchmark(args.root, args.dataset, args.label, "http://lume-engine:5863/mcp",
          args.db, token, modes=("bm25",))
name = "lume-" + args.label + "-bm25-" + args.dataset
run = (args.root / "runs" / (name + ".trec")).read_bytes()
expected = (args.root / "runs" / args.baseline).read_bytes()
if run != expected:
    raise RuntimeError("rankings differ from baseline " + args.baseline)
times = sorted(json.loads(line)["ms"] for line in
               (args.root / "runs" / (name + ".lat.jsonl")).read_text().splitlines())
def percentile(fraction):
    offset = (len(times) - 1) * fraction
    lo = int(offset)
    hi = min(lo + 1, len(times) - 1)
    return times[lo] + (times[hi] - times[lo]) * (offset - lo)
throughput = json.loads((args.root / "runs" / (name + ".throughput.json")).read_text())
result = {"label": args.label, "dataset": args.dataset, "ranking_parity": True,
          "run_sha256": hashlib.sha256(run).hexdigest(),
          "p50_ms": percentile(.5), "p95_ms": percentile(.95), "p99_ms": percentile(.99),
          "qps8": throughput["worker_scaling"][0]["qps"],
          "qps16": throughput["worker_scaling"][1]["qps"]}
(args.root / "runs" / (name + ".hot-summary.json")).write_text(
    json.dumps(result, indent=2) + "\n")
print(json.dumps(result, indent=2), flush=True)
