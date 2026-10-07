#!/usr/bin/env python3
"""Summarize Pi ingest benchmark CSV into a Markdown report.

Computes:
- Rows/s throughput: mean and p95 (95th percentile)
- Process RSS: maximum (in KiB and MiB) and mean
- CPU utilization: mean and maximum
- CPU temperature: maximum and mean
- Throttling analysis: decodes Raspberry Pi vcgencmd get_throttled bitmask
- Storage sizes: WAL and shards growth
- Operational counters: records_ingested, ingest_blocked, apply_failures
"""

import argparse
import csv
import math
from pathlib import Path
import sys
from typing import Any, Dict, List, Optional, Tuple

THROTTLED_BIT_DESCRIPTIONS = {
    0: "Under-voltage detected",
    1: "Arm frequency capped",
    2: "Currently throttled",
    3: "Soft temperature limit active",
    16: "Under-voltage has occurred",
    17: "Arm frequency capping has occurred",
    18: "Throttling has occurred",
    19: "Soft temperature limit has occurred",
}


def parse_throttled_bits(val_str: str) -> Tuple[int, List[str]]:
    """Decode a hex throttled bitmask into active flags."""
    val_str = val_str.strip()
    if not val_str or val_str in ("n/a", "N/A", "none", "None"):
        return 0, []
    try:
        if val_str.lower().startswith("0x"):
            mask = int(val_str, 16)
        else:
            mask = int(val_str)
    except ValueError:
        return 0, []

    flags = []
    for bit, desc in sorted(THROTTLED_BIT_DESCRIPTIONS.items()):
        if mask & (1 << bit):
            flags.append(desc)
    return mask, flags


def calculate_percentile(values: List[float], p: float) -> float:
    """Calculate the p-th percentile (0..100) using linear interpolation."""
    if not values:
        return 0.0
    s = sorted(values)
    k = (len(s) - 1) * (p / 100.0)
    f = math.floor(k)
    c = math.ceil(k)
    if f == c:
        return float(s[int(k)])
    d0 = s[int(f)] * (c - k)
    d1 = s[int(c)] * (k - f)
    return float(d0 + d1)


def parse_float(val: Any, default: float = 0.0) -> float:
    try:
        return float(val)
    except (ValueError, TypeError):
        return default


def parse_int(val: Any, default: int = 0) -> int:
    try:
        return int(float(val))
    except (ValueError, TypeError):
        return default


def format_bytes(n_bytes: int) -> str:
    """Format bytes into human-readable string (KiB, MiB, GiB)."""
    if n_bytes < 1024:
        return f"{n_bytes} B"
    elif n_bytes < 1024 * 1024:
        return f"{n_bytes / 1024.0:.1f} KiB"
    elif n_bytes < 1024 * 1024 * 1024:
        return f"{n_bytes / (1024.0 * 1024.0):.2f} MiB"
    else:
        return f"{n_bytes / (1024.0 * 1024.0 * 1024.0):.2f} GiB"


def parse_benchmark_csv(csv_path: Path) -> List[Dict[str, Any]]:
    """Parse benchmark CSV rows into typed dictionaries."""
    rows: List[Dict[str, Any]] = []
    with open(csv_path, mode="r", encoding="utf-8", errors="replace") as f:
        reader = csv.DictReader(f)
        for r in reader:
            if not r:
                continue
            rows.append({
                "timestamp": r.get("timestamp", "").strip(),
                "elapsed_sec": parse_float(r.get("elapsed_sec", 0.0)),
                "pid": parse_int(r.get("pid", 0)),
                "rss_kb": parse_float(r.get("rss_kb", 0.0)),
                "cpu_pct": parse_float(r.get("cpu_pct", 0.0)),
                "records_ingested": parse_int(r.get("records_ingested", 0)),
                "rows_per_sec": parse_float(r.get("rows_per_sec", 0.0)),
                "wal_bytes": parse_int(r.get("wal_bytes", 0)),
                "shard_bytes": parse_int(r.get("shard_bytes", 0)),
                "temp_c": parse_float(r.get("temp_c", 0.0)),
                "ingest_blocked": parse_int(r.get("ingest_blocked", 0)),
                "apply_failures": parse_int(r.get("apply_failures", 0)),
                "throttled": r.get("throttled", "0x0").strip(),
            })
    return rows


def summarize_rows(rows: List[Dict[str, Any]]) -> Dict[str, Any]:
    """Compute summary metrics from benchmark rows."""
    if not rows:
        return {
            "error": "No benchmark rows found in CSV",
            "samples": 0,
        }

    first = rows[0]
    last = rows[-1]

    duration_sec = last["elapsed_sec"] - first["elapsed_sec"]
    if duration_sec <= 0 and len(rows) > 1:
        duration_sec = (len(rows) - 1) * 10.0

    pid = last["pid"] or first["pid"]

    # Ingest rows/s rates (exclude t=0 baseline if multiple rows exist)
    rate_rows = [r["rows_per_sec"] for r in rows if r["elapsed_sec"] > 0]
    if not rate_rows:
        rate_rows = [r["rows_per_sec"] for r in rows]

    total_records = last["records_ingested"] - first["records_ingested"]
    mean_rows_sec = (total_records / duration_sec) if duration_sec > 0 else (
        sum(rate_rows) / len(rate_rows) if rate_rows else 0.0
    )
    p95_rows_sec = calculate_percentile(rate_rows, 95.0) if rate_rows else 0.0
    max_rows_sec = max(rate_rows) if rate_rows else 0.0
    min_rows_sec = min(rate_rows) if rate_rows else 0.0

    # RSS
    rss_values = [r["rss_kb"] for r in rows if r["rss_kb"] > 0]
    max_rss_kb = max(rss_values) if rss_values else 0.0
    mean_rss_kb = (sum(rss_values) / len(rss_values)) if rss_values else 0.0
    start_rss_kb = first["rss_kb"]
    end_rss_kb = last["rss_kb"]

    # CPU
    cpu_values = [r["cpu_pct"] for r in rows if r["elapsed_sec"] > 0]
    if not cpu_values:
        cpu_values = [r["cpu_pct"] for r in rows]
    mean_cpu = (sum(cpu_values) / len(cpu_values)) if cpu_values else 0.0
    max_cpu = max(cpu_values) if cpu_values else 0.0
    p95_cpu = calculate_percentile(cpu_values, 95.0) if cpu_values else 0.0

    # Temperature
    temp_values = [r["temp_c"] for r in rows if r["temp_c"] > 0]
    max_temp = max(temp_values) if temp_values else 0.0
    mean_temp = (sum(temp_values) / len(temp_values)) if temp_values else 0.0
    min_temp = min(temp_values) if temp_values else 0.0

    # Throttling
    all_masks = set()
    all_flags = set()
    for r in rows:
        mask, flags = parse_throttled_bits(r["throttled"])
        if mask != 0:
            all_masks.add(mask)
            all_flags.update(flags)

    combined_mask = 0
    for m in all_masks:
        combined_mask |= m

    # Storage
    start_wal = first["wal_bytes"]
    end_wal = last["wal_bytes"]
    start_shards = first["shard_bytes"]
    end_shards = last["shard_bytes"]

    # Blocked and failures
    start_blocked = first["ingest_blocked"]
    end_blocked = last["ingest_blocked"]
    delta_blocked = end_blocked - start_blocked

    start_failures = first["apply_failures"]
    end_failures = last["apply_failures"]
    delta_failures = end_failures - start_failures

    cpu_pass = mean_cpu <= 25.0
    rss_pass = (max_rss_kb / 1024.0) <= 400.0
    spec06_pass = cpu_pass and rss_pass

    return {
        "samples": len(rows),
        "duration_sec": duration_sec,
        "pid": pid,
        "start_time": first["timestamp"],
        "end_time": last["timestamp"],
        "records_start": first["records_ingested"],
        "records_end": last["records_ingested"],
        "records_ingested_total": total_records,
        "rows_per_sec_mean": mean_rows_sec,
        "rows_per_sec_p95": p95_rows_sec,
        "rows_per_sec_max": max_rows_sec,
        "rows_per_sec_min": min_rows_sec,
        "rss_max_kb": max_rss_kb,
        "rss_max_mib": max_rss_kb / 1024.0,
        "rss_mean_mib": mean_rss_kb / 1024.0,
        "rss_start_mib": start_rss_kb / 1024.0,
        "rss_end_mib": end_rss_kb / 1024.0,
        "cpu_mean_pct": mean_cpu,
        "cpu_max_pct": max_cpu,
        "cpu_p95_pct": p95_cpu,
        "temp_max_c": max_temp,
        "temp_mean_c": mean_temp,
        "temp_min_c": min_temp,
        "throttled_mask": combined_mask,
        "throttled_flags": sorted(all_flags),
        "wal_bytes_start": start_wal,
        "wal_bytes_end": end_wal,
        "shard_bytes_start": start_shards,
        "shard_bytes_end": end_shards,
        "ingest_blocked_start": start_blocked,
        "ingest_blocked_end": end_blocked,
        "ingest_blocked_delta": delta_blocked,
        "apply_failures_start": start_failures,
        "apply_failures_end": end_failures,
        "apply_failures_delta": delta_failures,
        "spec06_cpu_pass": cpu_pass,
        "spec06_rss_pass": rss_pass,
        "spec06_pass": spec06_pass,
    }


def generate_markdown(metrics: Dict[str, Any]) -> str:
    """Format summary metrics into a structured Markdown document."""
    if metrics.get("error"):
        return f"# Ingest Benchmark Error\n\n{metrics['error']}\n"

    duration_min = metrics["duration_sec"] / 60.0

    if metrics["throttled_mask"] == 0:
        throttled_str = "None (0x0)"
    else:
        flags_desc = ", ".join(metrics["throttled_flags"]) if metrics["throttled_flags"] else "unknown"
        throttled_str = f"**0x{metrics['throttled_mask']:x}** ({flags_desc})"

    temp_str = (
        f"{metrics['temp_max_c']:.1f}°C (Mean: {metrics['temp_mean_c']:.1f}°C, Min: {metrics['temp_min_c']:.1f}°C)"
        if metrics["temp_max_c"] > 0 else "N/A"
    )

    cpu_verdict = "PASS" if metrics.get("spec06_cpu_pass", False) else "FAIL"
    rss_verdict = "PASS" if metrics.get("spec06_rss_pass", False) else "FAIL"
    spec06_verdict = "PASS" if metrics.get("spec06_pass", False) else "FAIL"

    lines = [
        f"# Pi Ingest Benchmark Summary ({duration_min:.1f} min)",
        "",
        f"- **Process PID:** `{metrics['pid']}`",
        f"- **Time Window:** `{metrics['start_time']}` to `{metrics['end_time']}`",
        f"- **Duration:** `{metrics['duration_sec']:.1f}s` ({duration_min:.2f} min)",
        f"- **Samples Collected:** `{metrics['samples']}`",
        "",
        "## Spec/06 Verification (Sustained Ingest Limits)",
        "",
        "| Requirement | Target Limit | Measured | Verdict |",
        "| :--- | :--- | :--- | :--- |",
        f"| **CPU Usage (1-core)** | ≤ 25.0% of one core | {metrics['cpu_mean_pct']:.1f}% mean | **{cpu_verdict}** |",
        f"| **Process Memory** | ≤ 400.0 MB RSS | {metrics['rss_max_mib']:.1f} MiB max | **{rss_verdict}** |",
        f"| **Overall Spec/06 Verdict** | — | — | **{spec06_verdict}** |",
        "",
        "## Key Performance Indicators",
        "",
        "| Metric | Value | Reference / Notes |",
        "| :--- | :--- | :--- |",
        f"| **Throughput (Mean)** | **{metrics['rows_per_sec_mean']:.1f} rows/s** ({metrics['rows_per_sec_mean']:.1f} values/s) | Sustained rate over benchmark |",
        f"| **Throughput (p95)** | **{metrics['rows_per_sec_p95']:.1f} rows/s** ({metrics['rows_per_sec_p95']:.1f} values/s) | 95th percentile 10s rate |",
        f"| **Process RSS (Max)** | **{metrics['rss_max_mib']:.1f} MiB** ({metrics['rss_max_kb']:,.0f} KiB) | Memory peak |",
        f"| **CPU Usage (Mean)** | **{metrics['cpu_mean_pct']:.1f}%** | Peak: {metrics['cpu_max_pct']:.1f}%, p95: {metrics['cpu_p95_pct']:.1f}% |",
        f"| **CPU Temperature (Max)** | **{metrics['temp_max_c']:.1f}°C** | {temp_str} |",
        f"| **Throttling** | {throttled_str} | `vcgencmd get_throttled` |",
        f"| **Ingest Blocked** | **{metrics['ingest_blocked_delta']}** (Total: {metrics['ingest_blocked_end']}) | Admission cap blocks |",
        f"| **Apply Failures** | **{metrics['apply_failures_delta']}** (Total: {metrics['apply_failures_end']}) | Failed boundary writes |",
        "",
        "## Ingest & Storage Details",
        "",
        f"- **Records Ingested:** {metrics['records_ingested_total']:,} total values (from {metrics['records_start']:,} to {metrics['records_end']:,})",
        f"- **Rate Range:** Min: {metrics['rows_per_sec_min']:.1f} rows/s, Max: {metrics['rows_per_sec_max']:.1f} rows/s",
        f"- **RSS Range:** Start: {metrics['rss_start_mib']:.1f} MiB, End: {metrics['rss_end_mib']:.1f} MiB, Max: {metrics['rss_max_mib']:.1f} MiB",
        f"- **WAL Storage:** Start: {format_bytes(metrics['wal_bytes_start'])}, End: {format_bytes(metrics['wal_bytes_end'])}",
        f"- **Shards Storage:** Start: {format_bytes(metrics['shard_bytes_start'])}, End: {format_bytes(metrics['shard_bytes_end'])}",
        "",
    ]
    return "\n".join(lines)


def main() -> None:
    parser = argparse.ArgumentParser(description="Summarize Pi ingest benchmark CSV into Markdown.")
    parser.add_argument("csv_path", type=Path, help="Path to input benchmark CSV file")
    parser.add_argument("--output", "-o", type=Path, help="Path to write output Markdown file")
    args = parser.parse_args()

    if not args.csv_path.exists():
        sys.stderr.write(f"Error: CSV file not found: {args.csv_path}\n")
        sys.exit(1)

    rows = parse_benchmark_csv(args.csv_path)
    metrics = summarize_rows(rows)
    md = generate_markdown(metrics)

    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(md, encoding="utf-8")
        sys.stderr.write(f"Wrote summary to {args.output}\n")

    print(md)


if __name__ == "__main__":
    main()
