"""HOST ONLY: grade an agent_mcp_run transcript directory against the hidden expected file.

The runtime (agent_mcp_run.py) never imports this module and never reads expected.json.

Scalar answers pass when a number in the final answer text (or `data`) matches the expected
value: exactly for `exact`, within `absolute` for `tolerance`. Row-set answers are compared as
sets: first against the answer's `data` rows when present, otherwise against the rows of the
agent's last successful ti_query result (the evidence it answered from). Text columns compare
exactly; numeric columns use the per-column absolute tolerance. `set` answers compare the
expected strings with those named in the answer text or data.
"""
import argparse
import json
import re
from pathlib import Path

NUMBER = re.compile(r"-?\d+(?:\.\d+)?")


def numbers(value):
    if isinstance(value, bool):
        return []
    if isinstance(value, (int, float)):
        return [float(value)]
    if isinstance(value, str):
        return [float(n) for n in NUMBER.findall(value.replace(",", ""))]
    if isinstance(value, dict):
        return [n for v in value.values() for n in numbers(v)]
    if isinstance(value, list):
        return [n for v in value for n in numbers(v)]
    return []


def final(record):
    answer = record.get("final_answer")
    if isinstance(answer, dict):
        return answer.get("answer") or "", answer.get("data")
    return answer or "", None


def tool_rows(record):
    """Rows from the last successful ti_query call, newest first."""
    for call in reversed(record.get("tool_calls") or []):
        if call.get("name") != "ti_query":
            continue
        result = call.get("result")
        text = result
        if isinstance(result, dict):
            content = result.get("content")
            if isinstance(content, list) and content and isinstance(content[0], dict):
                text = content[0].get("text")
            elif "rows" in result:
                return result["rows"]
        if isinstance(text, str):
            try:
                parsed = json.loads(text)
            except ValueError:
                continue
            if isinstance(parsed, dict) and isinstance(parsed.get("rows"), list):
                return parsed["rows"]
    return None


def norm_time(value):
    text = str(value).replace("T", " ").replace("Z", "")
    if re.fullmatch(r"\d{4}-\d{2}-\d{2}", text):
        text += " 00:00:00"  # a DATE equals the midnight bucket start
    return text[:19]


def value_match(actual, expected, limit):
    """Text and timestamps compare normalized; numbers within limit (0 = exact)."""
    if actual is None or expected is None:
        return actual is expected
    if isinstance(expected, (int, float)) and not isinstance(expected, bool):
        try:
            return abs(float(actual) - float(expected)) <= limit + 1e-9
        except (TypeError, ValueError):
            return False
    return norm_time(actual) == norm_time(expected) or str(actual) == str(expected)


def constant_columns(expected_rows):
    """Columns with one value across every expected row (e.g. the vessel the question names).

    The question already fixes them, so an answer may omit them.
    """
    if len(expected_rows) < 2:
        return set()
    first = expected_rows[0]
    return {c for c in first if all(r.get(c) == first[c] for r in expected_rows)}


def row_match(actual, expected, grading, optional=frozenset()):
    """Match by value, not column name: the agent chooses its own aliases.

    Each expected value must match a distinct value in the actual row; numeric columns
    use their per-column tolerance, other columns compare exactly after normalization.
    """
    if not isinstance(actual, dict):
        return False
    tol = grading.get("absolute", {})
    tol = tol if isinstance(tol, dict) else {}
    available = list(actual.values())
    for column, want in expected.items():
        limit = tol.get(column, 0)
        hit = next((i for i, v in enumerate(available) if value_match(v, want, limit)), None)
        if hit is None:
            if column in optional:
                continue
            return False
        available.pop(hit)
    return True


def grade_rows(rows, expected, grading):
    if not isinstance(rows, list) or len(rows) != len(expected):
        return False, f"row count {len(rows) if isinstance(rows, list) else None} vs {len(expected)}"
    remaining = list(rows)
    optional = constant_columns(expected)
    for want in expected:
        hit = next((r for r in remaining
                    if isinstance(r, dict) and row_match(r, want, grading, optional)), None)
        if hit is None:
            return False, f"no row matching {want}"
        remaining.remove(hit)
    return True, "rows match"


def grade(record, expected):
    text, data = final(record)
    mode = expected["grading"]["mode"]
    answer = expected["answer"]
    if record.get("status") != "completed":
        return False, f"status {record.get('status')}"
    if isinstance(answer, list) and answer and isinstance(answer[0], dict):
        if isinstance(data, list) and data and isinstance(data[0], dict):
            ok, why = grade_rows(data, answer, expected["grading"])
            if ok:
                return True, "answer data " + why
        ok, why = grade_rows(tool_rows(record), answer, expected["grading"])
        return ok, ("final ti_query " if ok else "") + why
    if mode == "set":
        haystack = json.dumps([text, data])
        missing = [a for a in answer if str(a) not in haystack]
        return not missing, f"missing {len(missing)} of {len(answer)}" if missing else "all present"
    found = numbers(text) + numbers(data)
    want = float(answer)
    limit = expected["grading"].get("absolute", 0) if mode == "tolerance" else 0
    if any(abs(n - want) <= limit + 1e-9 for n in found):
        return True, f"found {want}"
    return False, f"expected {answer}, answer numbers {found[:6]}"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("run_dir", type=Path)
    parser.add_argument("--expected", type=Path, required=True)
    args = parser.parse_args()
    expected = {a["id"]: a for a in json.loads(args.expected.read_text(encoding="utf-8"))["answers"]}
    lines = ["| id | class | status | turns | tools | grade | note |", "|---|---|---|---:|---:|---|---|"]
    passed = 0
    results = []
    for qid in sorted(expected):
        path = args.run_dir / f"{qid}.json"
        if not path.exists():
            lines.append(f"| {qid} | | missing | | | FAIL | no transcript |")
            results.append({"id": qid, "pass": False, "note": "no transcript"})
            continue
        record = json.loads(path.read_text(encoding="utf-8"))
        ok, note = grade(record, expected[qid])
        passed += ok
        results.append({"id": qid, "pass": ok, "note": note})
        lines.append(f"| {qid} | {record.get('class', '')} | {record.get('status')} | {record.get('turns', '')} | "
                     f"{len(record.get('tool_calls') or [])} | {'PASS' if ok else 'FAIL'} | {note[:90]} |")
    lines.append(f"\n**{passed}/{len(expected)} correct**")
    report = "\n".join(lines)
    (args.run_dir / "grade.md").write_text(report, encoding="utf-8")
    (args.run_dir / "grade.json").write_text(json.dumps({"passed": passed, "total": len(expected),
                                                         "results": results}, indent=2), encoding="utf-8")
    print(report)


if __name__ == "__main__":
    main()
