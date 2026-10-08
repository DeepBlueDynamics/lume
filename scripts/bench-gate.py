#!/usr/bin/env python3
"""CI benchmark p95 regression gate (Spec/13, D48 gap 5).

Compares a benchmark run against a committed baseline (bench/baseline/ci-<runner>.json).
Fails (exit 1) if:
- warm p95 > baseline * 1.15 AND exceeds baseline by at least 2.0 ms (noise floor).
- any answer fingerprint changes.
- any row count changes.
- expected queries are missing.

Reports 'no baseline, recording only' (exit 0) if baseline is missing or a placeholder.
Outputs a markdown summary table to $GITHUB_STEP_SUMMARY and stdout.
Stdlib only.
"""

import argparse
import json
import os
import pathlib
import sys
from typing import Any, Dict, List, Optional, Tuple


def extract_queries(data: Any) -> Dict[str, Dict[str, Any]]:
    """Extract per-query mapping keyed by query id from benchmark JSON."""
    if not isinstance(data, dict):
        return {}
    if data.get("placeholder"):
        return {}
    # 1. Top-level queries list (FullReport / cache-on.json format)
    queries = data.get("queries")
    if isinstance(queries, list) and queries:
        return {q["id"]: q for q in queries if isinstance(q, dict) and "id" in q}
    # 2. Nested under cache_on (native_q6 composite report format)
    cache_on = data.get("cache_on")
    if isinstance(cache_on, dict):
        queries = cache_on.get("queries")
        if isinstance(queries, list) and queries:
            return {q["id"]: q for q in queries if isinstance(q, dict) and "id" in q}
    return {}


def load_json(path: pathlib.Path) -> Optional[Dict[str, Any]]:
    """Safely load JSON from file; returns None if file does not exist."""
    try:
        with open(path, "r", encoding="utf-8") as f:
            return json.load(f)
    except FileNotFoundError:
        return None
    except json.JSONDecodeError as e:
        raise ValueError(f"Corrupt JSON in {path}: {e}") from e


def evaluate(
    run_data: Dict[str, Any],
    baseline_data: Optional[Dict[str, Any]],
    ratio_threshold: float = 1.15,
    noise_floor_ms: float = 2.0,
) -> Dict[str, Any]:
    """Evaluate run against baseline.

    Returns dict with keys:
      - mode: 'comparison' or 'recording_only'
      - passed: bool
      - message: str
      - rows: list of evaluation records
      - failed_count: int
      - passed_count: int
    """
    run_queries = extract_queries(run_data)
    if not run_queries:
        raise ValueError("Run benchmark data contains no query records")

    base_queries = extract_queries(baseline_data) if baseline_data else {}
    if not base_queries:
        # No baseline or placeholder baseline: recording only
        rows = []
        for qid in sorted(run_queries.keys()):
            q = run_queries[qid]
            rows.append({
                "id": qid,
                "class": q.get("class", ""),
                "rows": q.get("rows"),
                "fingerprint": q.get("answer_fingerprint", ""),
                "cold_ms": q.get("cold_ms"),
                "p50_ms": q.get("p50_ms"),
                "p95_ms": q.get("p95_ms", q.get("warm_p95_ms")),
                "description": q.get("description", ""),
            })
        return {
            "mode": "recording_only",
            "passed": True,
            "message": "no baseline, recording only",
            "rows": rows,
            "failed_count": 0,
            "passed_count": len(rows),
        }

    rows = []
    all_qids = sorted(set(base_queries.keys()) | set(run_queries.keys()))
    failed_count = 0

    for qid in all_qids:
        if qid not in run_queries:
            failed_count += 1
            rows.append({
                "id": qid,
                "class": base_queries[qid].get("class", ""),
                "status": "FAIL",
                "base_rows": base_queries[qid].get("rows"),
                "run_rows": None,
                "base_fp": base_queries[qid].get("answer_fingerprint"),
                "run_fp": None,
                "base_p95": base_queries[qid].get("p95_ms", base_queries[qid].get("warm_p95_ms")),
                "run_p95": None,
                "diff_ms": None,
                "ratio": None,
                "reason": "Missing from benchmark run",
            })
            continue

        if qid not in base_queries:
            failed_count += 1
            rows.append({
                "id": qid,
                "class": run_queries[qid].get("class", ""),
                "status": "FAIL",
                "base_rows": None,
                "run_rows": run_queries[qid].get("rows"),
                "base_fp": None,
                "run_fp": run_queries[qid].get("answer_fingerprint"),
                "base_p95": None,
                "run_p95": run_queries[qid].get("p95_ms", run_queries[qid].get("warm_p95_ms")),
                "diff_ms": None,
                "ratio": None,
                "reason": "New query not present in baseline",
            })
            continue

        r = run_queries[qid]
        b = base_queries[qid]
        qclass = r.get("class", b.get("class", ""))

        r_rows = r.get("rows")
        b_rows = b.get("rows")
        r_fp = r.get("answer_fingerprint")
        b_fp = b.get("answer_fingerprint")
        r_p95 = r.get("p95_ms", r.get("warm_p95_ms"))
        b_p95 = b.get("p95_ms", b.get("warm_p95_ms"))

        diff_ms = None
        ratio = None
        if r_p95 is not None and b_p95 is not None:
            diff_ms = r_p95 - b_p95
            ratio = (r_p95 / b_p95) if b_p95 > 1e-9 else 1.0

        # Verification checks
        if r_rows != b_rows:
            failed_count += 1
            rows.append({
                "id": qid,
                "class": qclass,
                "status": "FAIL",
                "base_rows": b_rows,
                "run_rows": r_rows,
                "base_fp": b_fp,
                "run_fp": r_fp,
                "base_p95": b_p95,
                "run_p95": r_p95,
                "diff_ms": diff_ms,
                "ratio": ratio,
                "reason": f"Row count changed: {b_rows} -> {r_rows}",
            })
            continue

        if r_fp != b_fp:
            failed_count += 1
            rows.append({
                "id": qid,
                "class": qclass,
                "status": "FAIL",
                "base_rows": b_rows,
                "run_rows": r_rows,
                "base_fp": b_fp,
                "run_fp": r_fp,
                "base_p95": b_p95,
                "run_p95": r_p95,
                "diff_ms": diff_ms,
                "ratio": ratio,
                "reason": f"Answer fingerprint changed: {b_fp} -> {r_fp}",
            })
            continue

        # Check p95 regression:
        # Fails if warm p95 > baseline * 1.15 AND exceeds baseline by at least 2 ms.
        if r_p95 is not None and b_p95 is not None:
            is_ratio_regression = r_p95 > (b_p95 * ratio_threshold)
            is_noise_floor_exceeded = diff_ms >= noise_floor_ms

            if is_ratio_regression and is_noise_floor_exceeded:
                failed_count += 1
                rows.append({
                    "id": qid,
                    "class": qclass,
                    "status": "FAIL",
                    "base_rows": b_rows,
                    "run_rows": r_rows,
                    "base_fp": b_fp,
                    "run_fp": r_fp,
                    "base_p95": b_p95,
                    "run_p95": r_p95,
                    "diff_ms": diff_ms,
                    "ratio": ratio,
                    "reason": f"p95 regression: {r_p95:.2f} ms > {b_p95:.2f} ms x {ratio_threshold} (+{diff_ms:.2f} ms)",
                })
                continue
            elif is_ratio_regression and not is_noise_floor_exceeded:
                reason = f"Noise floor: +{diff_ms:.2f} ms < {noise_floor_ms:.1f} ms (sub-ms noise)"
            else:
                reason = "PASS"
        else:
            reason = "PASS (missing p95 metric)"

        rows.append({
            "id": qid,
            "class": qclass,
            "status": "PASS",
            "base_rows": b_rows,
            "run_rows": r_rows,
            "base_fp": b_fp,
            "run_fp": r_fp,
            "base_p95": b_p95,
            "run_p95": r_p95,
            "diff_ms": diff_ms,
            "ratio": ratio,
            "reason": reason,
        })

    passed_count = len(rows) - failed_count
    return {
        "mode": "comparison",
        "passed": (failed_count == 0),
        "message": "all queries passed" if failed_count == 0 else f"{failed_count} queries failed regression gate",
        "rows": rows,
        "failed_count": failed_count,
        "passed_count": passed_count,
    }


def render_markdown(
    eval_result: Dict[str, Any],
    run_path: pathlib.Path,
    baseline_path: Optional[pathlib.Path],
    runner: str,
    ratio_threshold: float = 1.15,
    noise_floor_ms: float = 2.0,
) -> str:
    """Generate Markdown report for GITHUB_STEP_SUMMARY and stdout."""
    lines = []
    lines.append("# CI Benchmark Regression Gate Report")
    lines.append("")
    lines.append(f"- **Runner:** `{runner}`")
    lines.append(f"- **Run results:** `{run_path}`")
    lines.append(f"- **Baseline:** `{baseline_path if baseline_path else 'None'}`")

    mode = eval_result["mode"]
    if mode == "recording_only":
        lines.append(f"- **Gate status:** ℹ️ **{eval_result['message'].upper()}**")
        lines.append("")
        lines.append("> [!NOTE]")
        lines.append(f"> {eval_result['message']}. No committed baseline found for `{runner}` (or baseline is placeholder).")
        lines.append("> To establish a baseline, trigger workflow with `update_baseline: true` and commit the uploaded artifact.")
        lines.append("")
        lines.append("| Query | Class | Rows | Warm p95 (ms) | Warm p50 (ms) | Cold (ms) | Fingerprint | Description |")
        lines.append("|---|---|---:|---:|---:|---:|---|---|")
        for r in eval_result["rows"]:
            p95 = f"{r['p95_ms']:.2f}" if r['p95_ms'] is not None else "—"
            p50 = f"{r['p50_ms']:.2f}" if r['p50_ms'] is not None else "—"
            cold = f"{r['cold_ms']:.2f}" if r['cold_ms'] is not None else "—"
            fp = f"`{r['fingerprint'][:8]}`" if r['fingerprint'] else "—"
            desc = r.get("description", "")
            lines.append(f"| `{r['id']}` | {r['class']} | {r['rows']} | {p95} | {p50} | {cold} | {fp} | {desc} |")
        lines.append("")
        return "\n".join(lines)

    # Comparison mode
    status_badge = "✅ **PASS**" if eval_result["passed"] else "❌ **FAIL**"
    pct = round((ratio_threshold - 1.0) * 100)
    lines.append(f"- **Gate status:** {status_badge} ({eval_result['message']})")
    lines.append(f"- **Thresholds:** regression > {pct}% (`>{ratio_threshold:.2f}x`) AND delta >= `{noise_floor_ms:.1f} ms` fails; row count or fingerprint change fails")
    lines.append(f"- **Summary:** `{eval_result['passed_count']}` passed, `{eval_result['failed_count']}` failed of `{len(eval_result['rows'])}` queries evaluated.")
    lines.append("")

    lines.append("| Query | Class | Rows (base / run) | FP | Base p95 ms | Run p95 ms | Delta ms | Ratio | Status | Reason |")
    lines.append("|---|---|---:|:---:|---:|---:|---:|---:|:---:|---|")

    for r in eval_result["rows"]:
        q_id = f"`{r['id']}`"
        q_class = r["class"]
        base_rows = r["base_rows"] if r["base_rows"] is not None else "—"
        run_rows = r["run_rows"] if r["run_rows"] is not None else "—"
        rows_str = f"{base_rows} / {run_rows}"

        if r["base_fp"] and r["run_fp"]:
            fp_str = "MATCH" if r["base_fp"] == r["run_fp"] else "DIFF"
        else:
            fp_str = "—"

        base_p95_str = f"{r['base_p95']:.2f}" if r["base_p95"] is not None else "—"
        run_p95_str = f"{r['run_p95']:.2f}" if r["run_p95"] is not None else "—"
        delta_str = f"{r['diff_ms']:+.2f}" if r["diff_ms"] is not None else "—"
        ratio_str = f"{r['ratio']:.2f}x" if r["ratio"] is not None else "—"
        status_icon = "✅ PASS" if r["status"] == "PASS" else "❌ FAIL"
        reason = r["reason"]

        lines.append(
            f"| {q_id} | {q_class} | {rows_str} | {fp_str} | {base_p95_str} | {run_p95_str} | {delta_str} | {ratio_str} | {status_icon} | {reason} |"
        )

    lines.append("")
    return "\n".join(lines)


def main(argv: Optional[List[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run", type=pathlib.Path, required=True,
                        help="Path to the benchmark results JSON from the current run")
    parser.add_argument("--baseline", type=pathlib.Path, default=None,
                        help="Path to baseline JSON (default: bench/baseline/ci-<runner>.json)")
    parser.add_argument("--runner", type=str, default="ubuntu-22.04",
                        help="Runner identifier (default: ubuntu-22.04)")
    parser.add_argument("--ratio-threshold", type=float, default=1.15,
                        help="Max allowed p95 ratio before failure (default: 1.15)")
    parser.add_argument("--noise-floor-ms", type=float, default=2.0,
                        help="Noise floor threshold in milliseconds (default: 2.0 ms)")
    parser.add_argument("--summary-file", type=pathlib.Path, default=None,
                        help="Path to write markdown summary (defaults to GITHUB_STEP_SUMMARY env if set)")

    args = parser.parse_args(argv)

    if not args.run.exists():
        print(f"ERROR: Run benchmark file not found: {args.run}", file=sys.stderr)
        return 2

    # Determine baseline path if not specified
    baseline_path = args.baseline
    if baseline_path is None:
        repo_root = pathlib.Path(__file__).resolve().parent.parent
        baseline_path = repo_root / "bench" / "baseline" / f"ci-{args.runner}.json"

    run_data = load_json(args.run)
    baseline_data = load_json(baseline_path) if baseline_path else None

    try:
        eval_result = evaluate(
            run_data=run_data,
            baseline_data=baseline_data,
            ratio_threshold=args.ratio_threshold,
            noise_floor_ms=args.noise_floor_ms,
        )
    except Exception as e:
        print(f"ERROR: Evaluation failed: {e}", file=sys.stderr)
        return 2

    markdown = render_markdown(
        eval_result=eval_result,
        run_path=args.run,
        baseline_path=baseline_path,
        runner=args.runner,
        ratio_threshold=args.ratio_threshold,
        noise_floor_ms=args.noise_floor_ms,
    )

    # Print markdown to stdout
    print(markdown)

    # Write to summary file (CLI arg or $GITHUB_STEP_SUMMARY)
    summary_path = args.summary_file or (
        pathlib.Path(os.environ["GITHUB_STEP_SUMMARY"])
        if "GITHUB_STEP_SUMMARY" in os.environ
        else None
    )
    if summary_path:
        try:
            with open(summary_path, "a", encoding="utf-8") as f:
                f.write(markdown + "\n")
        except Exception as e:
            print(f"WARNING: Could not write to summary file {summary_path}: {e}", file=sys.stderr)

    return 0 if eval_result["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
