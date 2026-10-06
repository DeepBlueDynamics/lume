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
    6: [{"path": p, "count": 1, "bucket_count": 1} for p in (bench.DEPTH, bench.SOG, bench.POSITION)],
}


def csv_reply(rows):
    stream = io.StringIO()
    writer = csv.writer(stream)
    columns = list(dict.fromkeys(k for row in rows for k in row)) if rows else ["_time", "_value"]
    kinds = []
    for name in columns:
        value = next((r[name] for r in rows if r.get(name) is not None), None)
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
    def __init__(self, mismatch=False, error=False, influx_rows=None, lume_rows=None, coverage=(FROM, TO)):
        self.mismatch = mismatch
        self.error = error
        self.influx_rows = influx_rows or ROWS
        self.lume_rows = lume_rows or ROWS
        self.coverage = coverage
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
                            self.reply({"rows": [{"first": pair.coverage[0], "last": pair.coverage[1]}], "truncated": False}, False)
                            return
                        if role == "influx" and "edge:" in text:
                            self.reply([{"edge": "first", "at": pair.coverage[0]}, {"edge": "last", "at": pair.coverage[1]}], True)
                            return
                        rows = (pair.influx_rows if role == "influx" else pair.lume_rows)[query_id(text, role == "influx")]
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
            self.assertEqual(report["window"]["mode"], "common_data_coverage_partial_hour")
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


class PiRegressions(unittest.TestCase):
    def test_epoch_aligned_first_aggregate_and_outward_explicit_and_discovered_bounds(self):
        lo, hi = "2026-10-06T22:36:21.309Z", "2026-10-06T22:43:59Z"
        start, stop = bench.snap_window("2026-10-06T22:36:20Z", "2026-10-06T22:36:20.000000001Z", 10)
        self.assertEqual(bench.iso(start), "2026-10-06T22:36:20.000000Z")
        self.assertEqual(bench.iso(stop), "2026-10-06T22:36:30.000000Z")
        hour = "2026-10-06T22:00:00Z"
        rows = {**ROWS, 2: [{"time": hour, "value": 71.04}]}
        for bounds in ([], ["--from", lo, "--to", hi]):
            with self.subTest(bounds=bounds), MockPair(influx_rows=rows, lume_rows=rows, coverage=(lo, hi)) as pair:
                _, _, _, report = run_main(pair, *bounds)
                self.assertEqual(report["window"]["from"], "2026-10-06T22:36:20.000000Z")
                self.assertEqual(report["window"]["to"], "2026-10-06T22:44:00.000000Z")
                self.assertEqual(report["queries"][1]["status"], "PASS")
                flux = report["queries"][1]["flux"]
                self.assertIn('import "date"', flux)
                self.assertIn("date.truncate(t: r._time, unit: 1h)", flux)
                self.assertIn("date.truncate(t: r._time, unit: 1m)", report["queries"][2]["flux"])
                for q in report["queries"]:
                    for text in (q["sql"], q["flux"]):
                        self.assertIn("22:36:20.000000Z", text)
                        self.assertIn("22:44:00.000000Z", text)
                        self.assertNotIn("22:36:21.309", text)
                self.assertEqual(report["window"]["requested"]["from"], lo if bounds else bench.iso(bench.timestamp(lo)))

    def test_position_roles_lat_lon_and_nearest_sample_in_minimum_bucket(self):
        raw_time = "2026-10-01T00:00:03.309Z"
        raw = {**ROWS, 5: [{"time": raw_time, "depth": 3.0, "role": "depth"},
                           {"time": raw_time, "latitude": 60.0, "longitude": 24.0, "role": "position"}]}
        with MockPair(influx_rows=raw) as pair:
            _, _, _, report = run_main(pair, "--from", FROM, "--to", TO)
            q = report["queries"][4]
            self.assertEqual(q["status"], "FIDELITY")
            self.assertAlmostEqual(q["max_observed_difference"]["time_seconds"], 3.309)
            self.assertIn('r._field == "lat" or r._field == "lon"', q["flux"])
            self.assertIn('pivot(rowKey: ["_time"], columnKey: ["_field"]', q["flux"])
            self.assertIn('on: ["bucket"]', q["flux"])
            self.assertIn('sort(columns: ["distance", "position_time"])', q["flux"])
            self.assertIn('/ 10000000000 * 10000000000', q["flux"])
            self.assertEqual(q["influx"]["row_counts"], [1, 1, 1])

    def test_bounded_fidelity_means_min_time_and_strict_extrema(self):
        args = bench.parse_args([])
        args.width = 10
        item = lambda q: {"id": q}
        sample = lambda v: [{"time": bench.result_time(FROM), "value": v}]
        mean = bench.assess(item(3), sample(100), sample(100.5), args)
        self.assertEqual(mean["status"], "FIDELITY")
        self.assertEqual(mean["max_observed_difference"]["value_absolute"], .5)
        self.assertEqual(bench.assess(item(3), sample(100), sample(102), args)["status"], "MISMATCH")
        self.assertEqual(bench.assess(item(2), sample(100), sample(100.5), args)["status"], "MISMATCH")
        a = [{"time": bench.result_time("2026-10-01T00:00:09.999999999Z"), "depth": 3, "latitude": 60, "longitude": 24}]
        b = [{"time": bench.result_time(FROM), "depth": 3, "latitude": 60, "longitude": 24}]
        self.assertEqual(bench.assess(item(5), a, b, args)["status"], "FIDELITY")
        self.assertEqual(bench.assess(item(5), a, [{**b[0], "depth": 3.01}], args)["status"], "MISMATCH")
        self.assertEqual(bench.assess(item(5), [{**a[0], "time": bench.result_time(TO)}], b, args)["status"], "MISMATCH")
        # A changed predicate membership set cannot be excused without evidence.
        self.assertEqual(bench.assess(item(4), [], [{"time": bench.result_time(FROM), "speed": 2.001, "depth": 3}], args)["status"], "MISMATCH")

    def test_raw_counts_require_exact_bucket_counts_and_values(self):
        args = bench.parse_args([])
        args.width = 10
        raw = [{"time": bench.result_time("2026-10-01T00:00:01Z"), "value": 2},
               {"time": bench.result_time("2026-10-01T00:00:09Z"), "value": 3}]
        retained = [{"time": bench.result_time(FROM), "value": 3}]
        q = {"id": 1, "raw_aggregate": "last"}
        check = bench.assess(q, raw, retained, args)
        self.assertEqual(check["status"], "FIDELITY")
        self.assertEqual(check["max_observed_difference"]["bucket_row_count_difference"], 0)
        self.assertEqual(bench.assess(q, raw, [{**retained[0], "value": 3.1}], args)["status"], "MISMATCH")
        q6 = {"id": 6}
        left = [{"path": "depth", "count": 100, "bucket_count": 10}]
        right = [{"path": "depth", "count": 10}]
        self.assertEqual(bench.assess(q6, left, right, args)["status"], "FIDELITY")
        self.assertEqual(bench.assess(q6, [{**left[0], "bucket_count": 11}], right, args)["status"], "MISMATCH")
        self.assertEqual(bench.assess(q6, [{**left[0], "count": 10}], right, args)["status"], "PASS")
        self.assertEqual(bench.assess(q6, [{"path": "depth", "count": 100}], right, args)["status"], "MISMATCH")

    def test_q6_full_path_sets_symmetric_difference_and_fidelity_in_json(self):
        raw_counts = [{"path": r["path"], "count": 3, "bucket_count": 1} for r in ROWS[6]]
        raw_counts.append({"path": "influx.extra", "count": 2, "bucket_count": 1})
        retained = [*ROWS[6], {"path": "lume.extra", "count": 1}]
        with MockPair(influx_rows={**ROWS, 6: raw_counts}, lume_rows={**ROWS, 6: retained}) as pair:
            _, _, _, report = run_main(pair, "--from", FROM, "--to", TO)
            q = report["queries"][5]
            self.assertEqual(q["status"], "MISMATCH")
            paths = q["checks"][0]["path_sets"]
            self.assertEqual(paths["influx_only"], ["influx.extra"])
            self.assertEqual(paths["lume_only"], ["lume.extra"])
            self.assertEqual(paths["symmetric_difference"], ["influx.extra", "lume.extra"])
            self.assertEqual(q["max_observed_difference"]["bucket_count_absolute"], 0)
            self.assertNotIn("contains(value: r._measurement", q["flux"])
            self.assertIn('unique(column: "_time")', q["flux"])
        with MockPair(influx_rows={**ROWS, 6: raw_counts[:-1]}) as pair:
            code, _, _, report = run_main(pair, "--from", FROM, "--to", TO)
            self.assertEqual(report["queries"][5]["status"], "FIDELITY")
            self.assertEqual(code, 0)

    def test_default_uses_latest_complete_common_hours_and_short_history_fallback(self):
        args = bench.parse_args([])
        lo = bench.timestamp("2026-10-01T21:10:21Z")
        hi = bench.timestamp("2026-10-02T01:20:59Z")
        start, stop, mode = bench.choose_window(args, lo, hi)
        self.assertEqual(bench.iso(start), "2026-10-01T22:00:00.000000Z")
        self.assertEqual(bench.iso(stop), "2026-10-02T01:00:00.000000Z")
        self.assertEqual(mode, "latest_full_common_hours")
        with MockPair(coverage=(bench.iso(lo), bench.iso(hi))) as pair:
            _, _, _, report = run_main(pair)
            self.assertEqual(report["window"]["mode"], mode)
            self.assertEqual(report["window"]["from"], bench.iso(start))
            self.assertEqual(report["window"]["to"], bench.iso(stop))
        self.assertEqual(bench.choose_window(args, bench.timestamp(FROM), bench.timestamp(TO))[2], "common_data_coverage_partial_hour")


if __name__ == "__main__":
    unittest.main()
