#!/usr/bin/env python3
"""Facets Oracle: exact reference implementation for metadata facets.

Computes exact facet counts in Python from a run's full match set joined with docs_meta.jsonl.
Supports:
- field facets (single-valued and multi-valued list, count desc / val asc tie break, missing count)
- range facets ([lo, lo+gap) half-open intervals, before, after, missing counts)
- query facets (exact match count over query term occurrences)
- normalization and equivalence comparison against Lume and Luxir responses
"""
import argparse
import datetime
import json
import math
from pathlib import Path
import re


def load_docs_meta(path):
    """Load docs_meta.jsonl into a mapping of id -> document metadata dict."""
    p = Path(path)
    docs = {}
    with p.open(encoding="utf-8") as f:
        for line in f:
            if line.strip():
                row = json.loads(line)
                docs[str(row["id"])] = row
    return docs


def parse_date_to_ms(val):
    """Convert a date value (epoch ms, ISO-8601 string, or YYYY-MM-DD) to epoch milliseconds float."""
    if isinstance(val, (int, float)):
        return float(val)
    if isinstance(val, str):
        val = val.strip()
        # Try ISO 8601 parsing
        try:
            if val.endswith("Z"):
                val = val[:-1] + "+00:00"
            dt = datetime.datetime.fromisoformat(val)
            if dt.tzinfo is None:
                dt = dt.replace(tzinfo=datetime.timezone.utc)
            return dt.timestamp() * 1000.0
        except ValueError:
            pass
        # Try YYYY-MM-DD
        try:
            dt = datetime.datetime.strptime(val, "%Y-%m-%d").replace(tzinfo=datetime.timezone.utc)
            return dt.timestamp() * 1000.0
        except ValueError:
            pass
    raise ValueError(f"Cannot parse date value: {val!r}")


def field_facet(match_ids, docs_meta, field, limit=None, mincount=0):
    """Compute exact field facet counts over the matching documents."""
    counts = {}
    missing = 0

    for docid in match_ids:
        doc = docs_meta.get(str(docid))
        if doc is None or field not in doc or doc[field] is None:
            missing += 1
            continue

        val = doc[field]
        if isinstance(val, (list, tuple, set)):
            if not val:
                missing += 1
            else:
                for item in sorted(set(val)):
                    k = str(item)
                    counts[k] = counts.get(k, 0) + 1
        else:
            k = str(val)
            counts[k] = counts.get(k, 0) + 1

    buckets = [
        {"val": k, "count": v}
        for k, v in counts.items()
        if v >= mincount
    ]
    # Sort count descending, tie-break val ascending
    buckets.sort(key=lambda b: (-b["count"], b["val"]))
    if limit is not None and limit > 0:
        buckets = buckets[:limit]

    return {
        "type": "field",
        "buckets": buckets,
        "missing": missing,
    }


def range_facet(match_ids, docs_meta, field, start, end, gap):
    """Compute exact range facet counts over half-open [lo, hi) intervals."""
    start = float(start)
    end = float(end)
    gap = float(gap)
    if gap <= 0:
        raise ValueError(f"gap must be > 0, got {gap}")

    intervals = []
    cur = start
    while cur < end:
        nxt = min(cur + gap, end)
        intervals.append((cur, nxt))
        cur = nxt

    bucket_counts = [0] * len(intervals)
    before = 0
    after = 0
    missing = 0

    for docid in match_ids:
        doc = docs_meta.get(str(docid))
        if doc is None or field not in doc or doc[field] is None:
            missing += 1
            continue

        raw = doc[field]
        try:
            if isinstance(raw, str) and ("-" in raw or ":" in raw):
                val = parse_date_to_ms(raw)
            else:
                val = float(raw)
        except (ValueError, TypeError):
            missing += 1
            continue

        if math.isnan(val):
            missing += 1
        elif val < start:
            before += 1
        elif val >= end:
            after += 1
        else:
            idx = int((val - start) // gap)
            if 0 <= idx < len(bucket_counts):
                bucket_counts[idx] += 1
            else:
                after += 1

    buckets = [
        {"from": lo, "to": hi, "count": c}
        for (lo, hi), c in zip(intervals, bucket_counts)
    ]

    return {
        "type": "range",
        "buckets": buckets,
        "before": before,
        "after": after,
        "missing": missing,
    }


def query_facet(match_ids, docs_text, query_text):
    """Compute exact query facet count: how many matching docs contain the query tokens."""
    tokens = [t.lower() for t in re.findall(r"\w+", query_text)]
    if not tokens:
        return {"type": "query", "count": 0}

    count = 0
    for docid in match_ids:
        text = docs_text.get(str(docid), "")
        doc_tokens = set(re.findall(r"\w+", text.lower()))
        if any(t in doc_tokens for t in tokens):
            count += 1

    return {
        "type": "query",
        "count": count,
    }


def parse_facet_spec(spec):
    """Parse a facet string specification into a structured request dict."""
    if isinstance(spec, dict):
        return spec
    s = spec.strip()
    if ":range(" in s and s.endswith(")"):
        field, inner = s[:-1].split(":range(", 1)
        parts = [p.strip() for p in inner.split(",")]
        if len(parts) != 3:
            raise ValueError(f"range spec requires start,end,gap; got {s}")
        return {
            "type": "range",
            "field": field.strip(),
            "start": float(parts[0]),
            "end": float(parts[1]),
            "gap": float(parts[2]),
        }
    if "=" in s:
        name, q = s.split("=", 1)
        return {
            "type": "query",
            "name": name.strip(),
            "query": q.strip(),
        }
    return {
        "type": "field",
        "field": s,
    }


def compute_facets_oracle(match_ids, docs_meta, requests, docs_text=None):
    """Compute exact facet counts for a set of requests against matching documents."""
    results = {}
    if docs_text is None:
        docs_text = {
            docid: doc.get("text", "")
            for docid, doc in docs_meta.items()
        }

    for req in requests:
        parsed = parse_facet_spec(req)
        rtype = parsed.get("type", "field")
        if rtype == "field":
            field_name = parsed["field"]
            limit = parsed.get("limit")
            mincount = parsed.get("mincount", 0)
            results[field_name] = field_facet(match_ids, docs_meta, field_name, limit=limit, mincount=mincount)
        elif rtype == "range":
            field_name = parsed["field"]
            start = parsed["start"]
            end = parsed["end"]
            gap = parsed["gap"]
            results[field_name] = range_facet(match_ids, docs_meta, field_name, start, end, gap)
        elif rtype == "query":
            name = parsed["name"]
            q = parsed["query"]
            results[name] = query_facet(match_ids, docs_text, q)
        else:
            raise ValueError(f"Unknown facet type {rtype!r}")

    return results


def normalize_lume_facets(facets):
    """Normalize Lume facet result map into standard format."""
    normalized = {}
    for name, f in facets.items():
        ftype = f.get("type")
        if ftype == "field":
            normalized[name] = {
                "type": "field",
                "buckets": [{"val": str(b["val"]), "count": int(b["count"])} for b in f.get("buckets", [])],
                "missing": int(f.get("missing", 0)),
            }
        elif ftype == "range":
            normalized[name] = {
                "type": "range",
                "buckets": [{"from": float(b["from"]), "to": float(b["to"]), "count": int(b["count"])} for b in f.get("buckets", [])],
                "before": int(f.get("before", 0)),
                "after": int(f.get("after", 0)),
                "missing": int(f.get("missing", 0)),
            }
        elif ftype == "query":
            normalized[name] = {
                "type": "query",
                "count": int(f.get("count", 0)),
            }
        else:
            normalized[name] = f
    return normalized


def normalize_luxir_facets(ops):
    """Normalize Luxir ops facet response into standard format."""
    normalized = {}
    for name, op in ops.items():
        if not isinstance(op, dict):
            continue
        if "buckets" in op:
            buckets = op.get("buckets", [])
            if buckets and isinstance(buckets[0].get("val"), list) and len(buckets[0]["val"]) == 2:
                # Range facet
                normalized[name] = {
                    "type": "range",
                    "buckets": [
                        {"from": float(b["val"][0]), "to": float(b["val"][1]), "count": int(b["count"])}
                        for b in buckets
                    ],
                    "missing": int(op.get("missing", 0)),
                }
            else:
                # Field or query facet
                normalized[name] = {
                    "type": "field",
                    "buckets": [
                        {"val": str(b["val"]), "count": int(b["count"])}
                        for b in buckets
                    ],
                    "missing": int(op.get("missing", 0)),
                }
        elif "count" in op:
            normalized[name] = {
                "type": "query",
                "count": int(op["count"]),
            }
    return normalized


def assert_facets_equal(actual, expected, check_range_bounds=True):
    """Assert that actual facet results match the expected oracle results."""
    assert set(actual.keys()) == set(expected.keys()), f"Key mismatch: {set(actual.keys())} vs {set(expected.keys())}"
    for k in expected:
        act = actual[k]
        exp = expected[k]
        assert act["type"] == exp["type"], f"Facet {k} type mismatch: {act['type']} != {exp['type']}"
        if exp["type"] == "field":
            assert act.get("missing", 0) == exp.get("missing", 0), f"Facet {k} missing mismatch: {act.get('missing')} != {exp.get('missing')}"
            act_buckets = {b["val"]: b["count"] for b in act.get("buckets", [])}
            exp_buckets = {b["val"]: b["count"] for b in exp.get("buckets", [])}
            assert act_buckets == exp_buckets, f"Facet {k} buckets mismatch: {act_buckets} != {exp_buckets}"
        elif exp["type"] == "range":
            assert act.get("missing", 0) == exp.get("missing", 0), f"Facet {k} range missing mismatch: {act.get('missing')} != {exp.get('missing')}"
            if "before" in exp:
                assert act.get("before", 0) == exp.get("before", 0), f"Facet {k} before mismatch: {act.get('before')} != {exp.get('before')}"
            if "after" in exp:
                assert act.get("after", 0) == exp.get("after", 0), f"Facet {k} after mismatch: {act.get('after')} != {exp.get('after')}"
            act_counts = [b["count"] for b in act.get("buckets", [])]
            exp_counts = [b["count"] for b in exp.get("buckets", [])]
            assert act_counts == exp_counts, f"Facet {k} range bucket counts mismatch: {act_counts} != {exp_counts}"
            if check_range_bounds:
                act_bounds = [(b["from"], b["to"]) for b in act.get("buckets", [])]
                exp_bounds = [(b["from"], b["to"]) for b in exp.get("buckets", [])]
                assert act_bounds == exp_bounds, f"Facet {k} range bounds mismatch: {act_bounds} != {exp_bounds}"
        elif exp["type"] == "query":
            assert act.get("count", 0) == exp.get("count", 0), f"Facet {k} query count mismatch: {act.get('count')} != {exp.get('count')}"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--docs-meta", type=Path, required=True, help="Path to docs_meta.jsonl")
    parser.add_argument("--match-ids", type=Path, help="File containing match document IDs (one per line)")
    parser.add_argument("--trec", type=Path, help="TREC run file to extract match IDs for a query")
    parser.add_argument("--qid", type=str, help="Query ID to extract from TREC file")
    parser.add_argument("--facets", nargs="+", default=["category", "tags", "year:range(1990,2030,5)"], help="Facet requests")
    parser.add_argument("--output", type=Path, help="Optional output JSON path")
    args = parser.parse_args()

    docs_meta = load_docs_meta(args.docs_meta)

    match_ids = []
    if args.match_ids:
        match_ids = [line.strip() for line in args.match_ids.read_text(encoding="utf-8").splitlines() if line.strip()]
    elif args.trec:
        with args.trec.open(encoding="utf-8") as f:
            for line in f:
                parts = line.split()
                if len(parts) >= 3:
                    if args.qid is None or parts[0] == args.qid:
                        match_ids.append(parts[2])
    else:
        # If no matches specified, run oracle over all docs in docs_meta
        match_ids = list(docs_meta.keys())

    oracle_facets = compute_facets_oracle(match_ids, docs_meta, args.facets)
    output_json = json.dumps(oracle_facets, indent=2)
    if args.output:
        args.output.write_text(output_json + "\n", encoding="utf-8")
        print(f"Wrote oracle facets to {args.output}")
    else:
        print(output_json)


if __name__ == "__main__":
    main()
