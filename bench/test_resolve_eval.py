import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import threading
import unittest
from agent_mcp_run import MCP
from resolve_eval import evaluate, grade, load_phrases, load_independent, markdown, metrics, candidates

ROOT = Path(__file__).resolve().parents[1]


class ResolveEvalTests(unittest.TestCase):
    def test_fixture_size_and_splits(self):
        cases = load_phrases(ROOT / "tests/golden/resolve_phrases.json")
        self.assertEqual(len(cases), 100)
        self.assertEqual(sum(e["split"] == "development" for e in cases), 70)
        self.assertEqual(sum(e["split"] == "holdout" for e in cases), 30)

    def test_rank_slots_and_aggregate_expectations(self):
        e = {"hidden": {"expected_paths": ["depth"], "expected_agg": "min"}}
        found = [{"path": "depth", "agg": "mean"}, {"path": "wind", "agg": "mean"},
                 {"path": "depth", "agg": "min"}]
        self.assertFalse(grade(e, found, 1))
        self.assertTrue(grade(e, found, 3))
        self.assertFalse(grade(e, found[:2], 3))
        self.assertEqual(candidates({"structuredContent": {"candidates": []}}), [])
        with self.assertRaises(ValueError):
            candidates({"isError": True})

    def test_loopback_mcp_only_receives_public_phrase_and_limit(self):
        requests = []
        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass
            def do_POST(self):
                request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                requests.append(request)
                method = request["method"]
                if method == "initialize":
                    result = {}
                elif method == "notifications/initialized":
                    self.send_response(204)
                    self.end_headers()
                    return
                elif method == "tools/list":
                    result = {"tools": [{"name": "ti_resolve"}]}
                else:
                    phrase = request["params"]["arguments"]["phrase"]
                    result = {"content": [{"type": "text", "text": json.dumps({"candidates": [
                        {"path": "other" if phrase == "first" else "secret_path",
                         "column": "other" if phrase == "first" else "secret_path@mean"}]})}]}
                body = json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}).encode()
                self.send_response(200)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever)
        thread.start()
        try:
            entries = [{"id": str(i), "phrase": p, "split": s,
                        "hidden": {"expected_paths": ["secret_path"]}}
                       for i, (p, s) in enumerate([("first", "development"), ("second", "holdout")])]
            records = evaluate(MCP(f"http://127.0.0.1:{server.server_port}/mcp"), entries)
            self.assertNotIn("secret_path", json.dumps(requests))
            calls = [r for r in requests if r["method"] == "tools/call"]
            self.assertEqual(calls[0]["params"]["arguments"], {"phrase": "first", "limit": 3})
            self.assertEqual(metrics(records)["all"]["top3"], 1)
            self.assertEqual(metrics(records)["holdout"]["top1"], 1)
            self.assertIn("first → other", markdown(records))
            self.assertNotIn("second →", markdown(records))
        finally:
            server.shutdown()
            thread.join()
            server.server_close()

    def test_non_loopback_is_not_allowed(self):
        with self.assertRaises(ValueError):
            MCP("http://192.168.1.1:5863/mcp")

    def test_independent_split_does_not_inflate_primary_gate_or_leak_labels(self):
        cases = load_independent(ROOT / "tests/golden/resolve_independent.json")
        self.assertEqual(len(cases), 20)
        records = [
            dict(id="dev", phrase="public", split="development", top1=False,
                 top3=False, candidates=[], expected={"expected_paths": ["hidden"]}),
            dict(id="blind", phrase="blind", split="independent", top1=True,
                 top3=True, candidates=[], expected={"expected_paths": ["hidden"]}),
        ]
        scores = metrics(records)
        self.assertEqual(scores["all"]["total"], 1)
        self.assertEqual(scores["all"]["top3"], 0)
        self.assertEqual(scores["independent"]["top3"], 1)
        self.assertNotIn("blind →", markdown(records))
        class StubMCP:
            def tools(self):
                return [{"name": "ti_resolve"}]
            def rpc(self, method, params):
                self.params = params
                return {"structuredContent": {"candidates": []}}
        mcp = StubMCP()
        evaluate(mcp, [cases[0]])
        self.assertEqual(set(mcp.params["arguments"]), {"phrase", "limit"})
        self.assertNotIn("hidden", json.dumps(mcp.params))
