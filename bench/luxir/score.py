#!/usr/bin/env python3
"""Score document-level TREC runs; linear graded nDCG, binary Recall/MRR."""
import argparse
import csv
import json
import math
from pathlib import Path

DATASETS = ("scifact", "trec-covid")


def read_qrels(path):
    result = {}
    with Path(path).open(encoding="utf-8") as stream:
        for qid, docid, grade in csv.reader(stream, delimiter="\t"):
            grade = int(grade)
            if docid in result.setdefault(qid, {}):
                raise ValueError("duplicate qrel")
            result[qid][docid] = max(0, grade)  # -1 means unjudged in TREC-COVID
    return result


def read_run(path):
    queries = {}
    with Path(path).open(encoding="utf-8") as stream:
        for line in stream:
            if not line.strip():
                continue
            qid, q0, docid, rank, score, tag = line.split()
            rank, score = int(rank), float(score)
            if q0 != "Q0" or rank < 1 or not math.isfinite(score):
                raise ValueError("invalid TREC row")
            queries.setdefault(qid, []).append((rank, docid, score))
    result = {}
    for qid, rows in queries.items():
        rows.sort()
        if len(rows) > 100 or len({d for _, d, _ in rows}) != len(rows):
            raise ValueError("run must have at most 100 unique documents per query")
        if [r for r, _, _ in rows] != list(range(1, len(rows) + 1)):
            raise ValueError("ranks must be contiguous from 1")
        if any(rows[i][2] < rows[i + 1][2] for i in range(len(rows) - 1)):
            raise ValueError("rank order contradicts scores")
        result[qid] = [d for _, d, _ in rows]
    return result


def evaluate(qrels, run):
    if not qrels:
        raise ValueError("empty qrels")
    if set(run) - set(qrels):
        raise ValueError("run contains queries outside test split")
    totals = [0.0, 0.0, 0.0, 0.0]
    per_query = {}
    for qid, judged in qrels.items():
        docs = run.get(qid, [])
        relevant = {d for d, grade in judged.items() if grade > 0}
        dcg = sum(judged.get(d, 0) / math.log2(i + 2) for i, d in enumerate(docs[:10]))
        ideal = sum(g / math.log2(i + 2) for i, g in enumerate(sorted(judged.values(), reverse=True)[:10]))
        ndcg = dcg / ideal if ideal else 0.0
        recall = len(set(docs[:100]) & relevant) / len(relevant) if relevant else 0.0
        capped_recall = len(set(docs[:100]) & relevant) / min(100, len(relevant)) if relevant else 0.0
        mrr = next((1.0 / (i + 1) for i, d in enumerate(docs[:10]) if d in relevant), 0.0)
        per_query[qid] = {"ndcg_10": ndcg, "recall_100": recall, "mrr_10": mrr, "r_cap_100": capped_recall}
        for i, value in enumerate((ndcg, recall, mrr, capped_recall)):
            totals[i] += value
    return dict(zip(("ndcg_10", "recall_100", "mrr_10", "r_cap_100"), (v / len(qrels) for v in totals))), per_query


def percentile(values, fraction):
    values = sorted(values)
    if not values:
        raise ValueError("empty latency list")
    position = (len(values) - 1) * fraction
    lower = math.floor(position)
    upper = math.ceil(position)
    return values[lower] + (values[upper] - values[lower]) * (position - lower)


def read_latency(path, expected):
    times = {}
    with Path(path).open(encoding="utf-8") as stream:
        for line in stream:
            row = json.loads(line)
            qid, ms = str(row["qid"]), float(row["ms"])
            if qid in times or not math.isfinite(ms) or ms < 0:
                raise ValueError("duplicate or invalid latency")
            times[qid] = ms
    if set(times) != set(expected):
        raise ValueError("latencies must cover every test query exactly once")
    return {name: percentile(list(times.values()), p) for name, p in
            (("p50_ms", .5), ("p95_ms", .95), ("p99_ms", .99))}


def summarize(root):
    root = Path(root)
    rows = []
    invalid_runs = []
    for dataset in DATASETS:
        qrels = read_qrels(root / dataset / "qrels.tsv")
        for path in sorted((root / "runs").glob("*-" + dataset + ".trec")):
            name = path.stem
            invalid = path.with_suffix(".invalid.json")
            if invalid.exists():
                invalid_runs.append({"run": name, **json.loads(invalid.read_text())})
                continue
            engine_mode = name[:-(len(dataset) + 1)]
            engine = engine_mode.split("-")[0]
            metrics, per_query = evaluate(qrels, read_run(path))
            row = {"run": name, "dataset": dataset, "queries": len(qrels),
                   **metrics, **read_latency(path.with_suffix(".lat.jsonl"), qrels),
                   "per_query": per_query}
            variant_engine = engine_mode.rsplit("-", 1)[0]
            build_path = root / "runs" / (variant_engine + "-" + dataset + ".build.json")
            if not build_path.exists():
                build_path = root / "runs" / (engine + "-" + dataset + ".build.json")
            row["build"] = json.loads(build_path.read_text(encoding="utf-8")) if build_path.exists() else None
            throughput_path = path.with_suffix(".throughput.json")
            row["throughput"] = json.loads(throughput_path.read_text(encoding="utf-8")) if throughput_path.exists() else None
            rows.append(row)
    return {"metric_conventions": {"ndcg": "linear relevance gain, log2 discount, depth 10",
                                   "recall": "positive qrels, depth 100",
                                   "r_cap": "positive hits / min(100, positive qrels), depth 100",
                                   "mrr": "first positive qrel, depth 10",
                                   "aggregation": "macro average across every test qid; missing retrieval is zero",
                                   "latency": "linear interpolation over per-query medians from 3 timed passes"},
            "results": rows, "invalid_runs": invalid_runs}


def table(report):
    lines = ["| Run | nDCG@10 | Recall@100 | R_cap@100 | MRR@10 | p50 ms | p95 ms | p99 ms | QPS |",
             "|---|---:|---:|---:|---:|---:|---:|---:|---:|"]
    for row in report["results"]:
        throughput = row["throughput"] or {}
        qps = throughput.get("qps")
        cells = [row["run"]] + [f'{row[k]:.4f}' for k in ("ndcg_10", "recall_100", "r_cap_100", "mrr_10")] + [
            f'{row[k]:.2f}' for k in ("p50_ms", "p95_ms", "p99_ms")] + [
            f"{qps:.2f}" if qps is not None else "not run"]
        lines.append("| " + " | ".join(cells) + " |")
    return "\n".join(lines)


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--root", type=Path, required=True)
    p.add_argument("--output", type=Path, default=Path(__file__).resolve().parents[1] / "results/2026-10-09-luxir-vs-lume.json")
    args = p.parse_args()
    report = summarize(args.root)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(table(report))
    if not report["results"]:
        print("No run files exist yet; this is not a benchmark result.")


if __name__ == "__main__":
    main()
