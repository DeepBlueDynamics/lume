"""Stdlib tests: loopback mock Influx/Lume servers, parsing, checks and pagination."""
import contextlib
import csv
import importlib.util
import io
import json
from pathlib import Path
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("influx_vs_lume", Path(__file__).with_name("influx_vs_lume.py"))
bench = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(bench)
FROM = "2026-10-01T00:00:00Z"
TO = "2026-10-01T00:01:00Z"
CONTEXT = "vessels.urn:test:bench"
NAMES = {p + "@" + a for p in (bench.DEPTH, bench.SOG) for a in ("mean", "min", "max", "last")}
NAMES.update({bench.POSITION + ".latitude@last", bench.POSITION + ".longitude@last"})
CATALOG = {"width_seconds": 10, "time_coverage": [{"vessel": CONTEXT}],
           "tables": [{"name": "telemetry", "columns": [{"name": n} for n in sorted(NAMES)]}]}
ROWS = {
    1: [{"time": FROM, "value": 3.0}],
    2: [{"time": FROM, "value": 3.0}],
    3: [{"time": FROM, "value": 4.0}],
    4: [{"time": FROM, "speed": 4.0, "depth": 3.0}],
    5: [{"time": FROM, "depth": 3.0, "latitude": 60.0, "longitude": 24.0}],
    6: [{"path": p, "count": 1} for p in (bench.DEPTH, bench.SOG, bench.POSITION)],
}


def csv_reply(rows):
    stream = io.StringIO()
    writer = csv.writer(stream)
    columns = list(rows[0]) if rows else ["_time", "_value"]
    kinds = []
    for name in columns:
        value = rows[0].get(name) if rows else None
        kinds.append("dateTime:RFC3339" if name in ("_time", "at") else
                     "long" if isinstance(value, int) else "double" if isinstance(value, float) else "string")
    writer.writerow(["#datatype", "string", "long"] + kinds)
    writer.writerow(["#group", "false", "false"] + ["false"] * len(columns))
    writer.writerow(["#default", "_result", ""] + [""] * len(columns))
    writer.writerow(["", "result", "table"] + columns)
    for row in rows:
        writer.writerow(["", "", 0] + [row.get(c, "") for c in columns])
    return stream.getvalue()


def query_id(text, influx):
    if influx:
        if "count()" in text:
            return 6
        if "pivot(" in text:
            return 5
        if "join(tables:" in text:
            return 4
        if "every: 1m" in text:
            return 3
        if "every: 1h" in text:
            return 2
        return 1
    if "AS count" in text:
        return 6
    if "ORDER BY depth" in text:
        return 5
    if "HAVING avg(" in text:
        return 4
    if "INTERVAL '1 minute'" in text:
        return 3
    if "INTERVAL '1 hour'" in text:
        return 2
    return 1


class MockPair:
    def __init__(self, mismatch=False, error=False):
        self.mismatch = mismatch
        self.error = error
        self.calls = []
        self.servers = []

    def __enter__(self):
        pair = self
        for role in ("influx", "lume"):
            def make_handler(role):
                class Handler(BaseHTTPRequestHandler):
                    def log_message(self, *_):
                        pass

                    def do_GET(self):
                        pair.calls.append((role, self.path, None, dict(self.headers)))
                        self.reply(CATALOG, False)

                    def reply(self, value, influx):
                        body = (csv_reply(value) if influx else json.dumps(value)).encode()
                        self.send_response(200)
                        self.send_header("Content-Type", "text/csv" if influx else "application/json")
                        self.send_header("Content-Length", str(len(body)))
                        self.end_headers()
                        self.wfile.write(body)

                    def do_POST(self):
                        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                        pair.calls.append((role, self.path, body, dict(self.headers)))
                        if pair.error:
                            content = b'{"error":"reflected SECRET_BENCH_TOKEN"}'
                            self.send_response(401)
                            self.end_headers()
                            self.wfile.write(content)
                            return
                        text = body["query" if role == "influx" else "sql"]
                        if role == "lume" and text.startswith("SELECT min(ts)"):
                            self.reply({"rows": [{"first": FROM, "last": TO}], "truncated": False}, False)
                            return
                        if role == "influx" and "edge:" in text:
                            self.reply([{"edge": "first", "at": FROM}, {"edge": "last", "at": TO}], True)
                            return
                        rows = ROWS[query_id(text, role == "influx")]
                        if role == "influx":
                            aliases = {"time": "_time", "value": "_value", "latitude": "lat", "longitude": "lon"}
                            rows = [{aliases.get(k, k): v for k, v in r.items()} for r in rows]
                            if pair.mismatch:
                                rows = [dict(r, **({"_value": 99.0} if "_value" in r else {})) for r in rows]
                            self.reply(rows, True)
                        else:
                            self.reply({"rows": rows, "truncated": False}, False)
                return Handler
            server = ThreadingHTTPServer(("127.0.0.1", 0), make_handler(role))
            worker = threading.Thread(target=server.serve_forever, daemon=True)
            worker.start()
            self.servers.append((server, worker))
        self.env = {"INFLUX_URL": self.url(0), "INFLUX_ORG": "test org", "INFLUX_BUCKET": "boat",
                    "INFLUX_TOKEN": "SECRET_BENCH_TOKEN", "LUME_URL": self.url(1) + "/ti/query"}
        return self

    def url(self, index):
        return "http://127.0.0.1:" + str(self.servers[index][0].server_port)

    def __exit__(self, *_):
        for server, worker in self.servers:
            server.shutdown()
            server.server_close()
            worker.join()


def run_main(pair, *args):
    stdout, stderr = io.StringIO(), io.StringIO()
    with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
        code = bench.main(["--runs", "3", *args], pair.env)
    text = stdout.getvalue()
    report = json.loads(text.split("```json\n", 1)[1].rsplit("\n```", 1)[0]) if "```json\n" in text else None
    return code, text, stderr.getvalue(), report


class BenchmarkTests(unittest.TestCase):
    def test_six_pairs_http_auth_common_window_and_all_run_checks(self):
        with MockPair() as pair:
            code, output, errors, report = run_main(pair)
            self.assertEqual(code, 0, errors + output)
            self.assertEqual(report["window"]["mode"], "common_data_coverage")
            self.assertEqual(report["window"]["from"], bench.iso(bench.timestamp(FROM)))
            self.assertEqual(report["window"]["to"], bench.iso(bench.timestamp(TO)))
            self.assertEqual(len(report["queries"]), 6)
            for query in report["queries"]:
                self.assertEqual(query["status"], "PASS")
                self.assertEqual(len(query["checks"]), 3)
                for role in ("influx", "lume"):
                    self.assertIsNotNone(query[role]["cold_first_ms"])
                    self.assertIsNotNone(query[role]["warm_p50_ms"])
                    self.assertGreaterEqual(query[role]["warm_p95_ms"], query[role]["warm_p50_ms"])
                    self.assertEqual(len(query[role]["runs"]), 3)
            self.assertNotIn("SECRET_BENCH_TOKEN", output + errors)
            for role, route, body, headers in pair.calls:
                if role == "influx":
                    self.assertEqual(route, "/api/v2/query?org=test+org")
                    self.assertEqual(headers["Authorization"], "Token SECRET_BENCH_TOKEN")
                    self.assertEqual(body["type"], "flux")
                    self.assertIn(CONTEXT, body["query"])
                elif body:
                    self.assertEqual(route, "/ti/query")
                    self.assertEqual(body["max_rows"], 500)
                    self.assertEqual(headers["Accept"], "application/json")

    def test_explicit_window_skips_discovery_and_mismatches_are_visible(self):
        with MockPair(mismatch=True) as pair:
            code, output, errors, report = run_main(pair, "--from", FROM, "--to", TO)
            self.assertEqual(code, 1)
            self.assertIn("MISMATCH", output)
            self.assertEqual(report["window"]["mode"], "explicit")
            self.assertTrue(report["queries"][0]["checks"][0]["differences"])
            self.assertFalse(any(body and "edge:" in body.get("query", "") for _, _, body, _ in pair.calls))

    def test_reflected_token_redacted_on_backend_failure(self):
        with MockPair(error=True) as pair:
            code, output, errors, report = run_main(pair, "--from", FROM, "--to", TO)
            self.assertEqual(code, 1)
            self.assertNotIn("SECRET_BENCH_TOKEN", output + errors)
            self.assertIn("[REDACTED]", output)
            self.assertTrue(all(q["status"] == "ERROR" for q in report["queries"]))
            self.assertIsNone(report["queries"][0]["lume"]["warm_p50_ms"])

    def test_offline_dry_run_without_credentials_or_network(self):
        stdout = io.StringIO()
        with patch.object(bench.Client, "http", side_effect=AssertionError("Network in dry run")), contextlib.redirect_stdout(stdout):
            self.assertEqual(bench.main(["--dry-run", "--from", FROM, "--to", TO], {}), 0)
        text = stdout.getvalue()
        self.assertEqual(text.count("```flux"), 6)
        self.assertEqual(text.count("```sql"), 6)
        self.assertIn('timeSrc: "_start"', text)
        self.assertIn("@max", text)
        self.assertIn("@min", text)
        self.assertIn("group(columns: [])", text)
        self.assertIn('r._field == "lat"', text)

    def test_csv_multiple_tables_defaults_types_and_error_table(self):
        a = csv_reply([{"_time": FROM, "_value": 2.5, "count": 7, "label": "comma, quote \"here\""}])
        b = csv_reply([{"_time": TO, "_value": 4.0}])
        rows = bench.csv_rows(a + "\n" + b)
        self.assertEqual(len(rows), 2)
        self.assertEqual(rows[0]["_value"], 2.5)
        self.assertEqual(rows[0]["count"], 7)
        self.assertEqual(rows[0]["label"], 'comma, quote "here"')
        self.assertEqual(rows[0]["result"], "_result")
        with self.assertRaisesRegex(bench.BenchError, "broken"):
            bench.csv_rows(',error,reference\n,broken,12\n')
        with self.assertRaises(bench.BenchError):
            bench.csv_rows(',a,b\n,x\n')

    def test_value_time_and_exact_count_tolerances(self):
        self.assertEqual(bench.compare([{"v": 1.0}], [{"v": 1.0005}], .001, 0)["status"], "PASS")
        self.assertEqual(bench.compare([{"count": 100}], [{"count": 101}], 100, 1)["status"], "MISMATCH")
        self.assertEqual(bench.compare([], [], .001, 0)["status"], "EMPTY")
        self.assertEqual(bench.compare([{"v": None}], [{"v": 0}], .001, 0)["status"], "MISMATCH")
        self.assertEqual(bench.compare([{"v": float("nan")}], [{"v": float("nan")}], .001, 0)["status"], "MISMATCH")
        self.assertEqual(bench.percentile([1, 2, 3, 4], .5), 2.5)
        a = bench.canonical([{"_time": "2026-10-01T00:00:00.000000001Z", "_value": 1}], ["time", "value"], True)
        b = bench.canonical([{"time": FROM, "value": 1}], ["time", "value"])
        self.assertEqual(bench.compare(a, b, 0, 0)["status"], "MISMATCH")
        self.assertEqual(bench.result_time("2026-10-01T02:00:00.000000001+02:00"), a[0]["time"])
        with self.assertRaises(bench.BenchError):
            bench.canonical([{"time": FROM}], ["time", "value"])
        self.assertEqual(bench.compare([{"time": FROM}], [{"time": TO}], 0, 0, 60)["status"], "PASS")

    def test_truncation_time_split_avoids_partial_aggregates(self):
        args = bench.parse_args(["--runs", "2"])
        args.width = 10
        names, mapping = bench.fields(CATALOG, "telemetry")
        start, stop = bench.timestamp(FROM), bench.timestamp("2026-10-01T00:02:00Z")
        plan = bench.plans(args, start, stop, names, mapping, "boat", CONTEXT, CONTEXT)[2]
        calls = []
        class Stub:
            def lume(self, sql):
                calls.append(sql)
                return {"rows": [{"time": FROM, "value": 999}] if len(calls) == 1 else [{"time": FROM, "value": 4}],
                        "truncated": len(calls) == 1}
        rows, requests = bench.execute_lume(Stub(), plan, args, start, stop, names, mapping, "boat", CONTEXT, CONTEXT)
        self.assertEqual(requests, 3)
        self.assertEqual(len(rows), 2)
        self.assertTrue(all(row["value"] == 4 for row in rows))
        self.assertIn("2026-10-01T00:01:00.000000Z", calls[1])
        self.assertIn("2026-10-01T00:01:00.000000Z", calls[2])
        class Stuck:
            def lume(self, _):
                return {"rows": [], "truncated": True}
        one = bench.plans(args, start, bench.timestamp(TO), names, mapping, "boat", CONTEXT, CONTEXT)[2]
        with self.assertRaisesRegex(bench.BenchError, "One Lume output bin"):
            bench.execute_lume(Stuck(), one, args, start, bench.timestamp(TO), names, mapping, "boat", CONTEXT, CONTEXT)

    def test_sql_and_flux_escaping_and_missing_columns(self):
        self.assertEqual(bench.quote("a'b"), "'a''b'")
        self.assertEqual(bench.ident('a"b'), '"a""b"')
        self.assertIn('\\${', bench.flux_string('${secret}'))
        args = bench.parse_args([])
        args.width = 10
        plans = bench.plans(args, bench.timestamp(FROM), bench.timestamp(TO), set(), {}, "boat", CONTEXT, CONTEXT)
        self.assertTrue(all(p["missing"] for p in plans))
        with self.assertRaises(bench.BenchError):
            bench.endpoint("http://user:password@127.0.0.1", "/ti/query")

    def test_changing_warm_answers_are_checked(self):
        args = bench.parse_args(["--runs", "3"])
        args.width = 10
        names, mapping = bench.fields(CATALOG, "telemetry")
        count = 0
        class Changing:
            redact = staticmethod(str)
            def influx(self, _):
                nonlocal count
                count += 1
                return [{"_time": FROM, "_value": 9 if count == 2 else 3}]
            def lume(self, _):
                return {"rows": [{"time": FROM, "value": 3}], "truncated": False}
        with patch.object(bench, "plans", return_value=[{"id": 1, "name": "changing", "flux": "", "sql": "", "columns": ["time", "value"], "bin_seconds": None, "missing": None}]):
            report = bench.benchmark(Changing(), args, bench.timestamp(FROM), bench.timestamp(TO), names, mapping, "boat", CONTEXT, CONTEXT)
        self.assertEqual(report[0]["status"], "MISMATCH")
        self.assertEqual([c["status"] for c in report[0]["checks"]], ["PASS", "MISMATCH", "PASS"])


if __name__ == "__main__":
    unittest.main()
