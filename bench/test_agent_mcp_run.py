"""Offline runtime tests: actual loopback HTTP for both MCP and chat."""
from collections import Counter
from contextlib import contextmanager, redirect_stdout
import copy
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import io
import json
from pathlib import Path
import tempfile
import threading
import unittest
from unittest.mock import patch

import agent_mcp_run as runner
import agent_mcp_expected as expected

TOOL = {"name": "ti_query", "description": "Read SQL",
        "inputSchema": {"type": "object", "properties": {"sql": {"type": "string"}},
                        "required": ["sql"]}}


def call(name, arguments, number=1):
    return {"id": f"call-{number}", "type": "function",
            "function": {"name": name, "arguments": json.dumps(arguments)}}


@contextmanager
def servers(script):
    requests, proxies, headers = [], [], []
    responses = iter(script)

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            if self.path == "/v1/chat/completions":
                requests.append(body)
                headers.append(self.headers.get("Authorization"))
                message = next(responses, {"role": "assistant", "content": None,
                                           "tool_calls": [call("ti_query", {"sql": "SELECT 1"})]})
                reply = {"choices": [{"message": message}]}
            else:
                method = body["method"]
                if method == "notifications/initialized":
                    self.send_response(204)
                    self.end_headers()
                    return
                if method == "initialize":
                    result = {"protocolVersion": "2024-11-05", "capabilities": {}}
                elif method == "tools/list":
                    result = {"tools": [TOOL]}
                elif method == "tools/call":
                    proxies.append(body["params"])
                    result = {"content": [{"type": "text", "text": '{"rows":[{"n":7}]}'}]}
                else:
                    result = {}
                reply = {"jsonrpc": "2.0", "id": body["id"], "result": result}
            encoded = json.dumps(reply).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(encoded)))
            self.end_headers()
            self.wfile.write(encoded)

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    url = f"http://127.0.0.1:{server.server_port}"
    try:
        mcp = runner.MCP(url + "/mcp", timeout=2)
        tools = mcp.tools()
        yield mcp, tools, url + "/v1", requests, proxies, headers
    finally:
        server.shutdown()
        server.server_close()
        thread.join()


class AgentRuntimeTests(unittest.TestCase):
    def question(self):
        return {"id": "agent-01", "class": "Q3", "question": "How many buckets?"}

    def test_real_http_allowlist_and_transcript(self):
        script = [{"tool_calls": [
            call("exec_shell", {"command": "not allowed"}, 1),
            call("ti_query", {"sql": "SELECT count(*) AS n FROM telemetry"}, 2)]},
            {"tool_calls": [call("answer", {"answer": "Seven buckets", "data": 7}, 3)]}]
        with servers(script) as (mcp, tools, url, requests, proxies, headers):
            saved = []
            result = runner.run_question(self.question(), mcp, tools, url, "fake",
                                         key="TOKEN_NOT_IN_TRANSCRIPT",
                                         save=lambda r: saved.append(copy.deepcopy(r)))
        self.assertEqual(result["status"], "completed")
        self.assertEqual(result["final_answer"]["data"], 7)
        self.assertEqual(len(proxies), 1)
        self.assertEqual(proxies[0]["name"], "ti_query")
        self.assertTrue(result["tool_calls"][0]["result"]["isError"])
        self.assertEqual(result["sql"], [{"tool_call_id": "call-2",
                                         "sql": "SELECT count(*) AS n FROM telemetry"}])
        self.assertEqual({t["function"]["name"] for t in requests[0]["tools"]},
                         {"ti_query", "answer"})
        self.assertEqual(headers, ["Bearer TOKEN_NOT_IN_TRANSCRIPT"] * 2)
        self.assertNotIn("TOKEN_NOT_IN_TRANSCRIPT", json.dumps(result))
        self.assertEqual(saved[-1]["status"], "completed")
        self.assertGreater(result["timing"]["elapsed_ms"], 0)
        self.assertEqual(len(result["timing"]["llm_ms"]), 2)
        self.assertIn("rows", result["messages"][4]["content"])

    def test_twelve_turn_limit(self):
        with servers([]) as (mcp, tools, url, requests, proxies, _):
            result = runner.run_question(self.question(), mcp, tools, url, "fake")
        self.assertEqual(result["status"], "turn_limit")
        self.assertEqual(len(requests), 12)
        self.assertEqual(len(proxies), 12)

    def test_twenty_call_limit_in_one_turn(self):
        script = [{"tool_calls": [call("ti_query", {"sql": "SELECT 1"}, i)
                                  for i in range(25)]}]
        with servers(script) as (mcp, tools, url, requests, proxies, _):
            result = runner.run_question(self.question(), mcp, tools, url, "fake")
        self.assertEqual(result["status"], "tool_limit")
        self.assertEqual(len(result["tool_calls"]), 20)
        self.assertEqual(len(proxies), 20)
        self.assertEqual(len(requests), 1)

    def test_rejected_calls_consume_cap(self):
        script = [{"tool_calls": [call("foreign", {}, i) for i in range(25)]}]
        with servers(script) as (mcp, tools, url, _, proxies, _):
            result = runner.run_question(self.question(), mcp, tools, url, "fake")
        self.assertEqual(result["status"], "tool_limit")
        self.assertEqual(len(result["tool_calls"]), 20)
        self.assertFalse(proxies)

    def test_invalid_arguments_are_not_proxied(self):
        broken = call("ti_query", {})
        broken["function"]["arguments"] = "{broken"
        script = [{"tool_calls": [broken]},
                  {"tool_calls": [call("answer", {"answer": "Could not query"}, 2)]}]
        with servers(script) as (mcp, tools, url, _, proxies, _):
            result = runner.run_question(self.question(), mcp, tools, url, "fake")
        self.assertEqual(result["status"], "completed")
        self.assertFalse(proxies)
        self.assertTrue(result["tool_calls"][0]["result"]["isError"])

    def test_no_plain_text_early_finish(self):
        script = [{"content": "Seven"}, {"tool_calls": [
            call("answer", {"answer": "Seven", "data": 7})]}]
        with servers(script) as (mcp, tools, url, requests, _, _):
            result = runner.run_question(self.question(), mcp, tools, url, "fake")
        self.assertEqual(result["turns"], 2)
        self.assertEqual(len(requests), 2)

    def test_cli_all_twenty_no_hidden_prompts_and_files(self):
        fixture = json.loads((runner.ROOT / "tests/golden/agent_questions.json").read_text())
        for question in fixture["questions"]:
            question["hidden"] = {"oracle_sql": "HIDDEN_ORACLE_SENTINEL",
                                  "expected": "HIDDEN_EXPECTED_SENTINEL",
                                  "grading": "HIDDEN_GRADING_SENTINEL"}
        scratch = runner.ROOT / ".test-tmp"
        scratch.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(dir=scratch) as temporary:
            root = Path(temporary)
            question_file = root / "questions.json"
            question_file.write_text(json.dumps(fixture))
            script = [{"tool_calls": [call("answer", {"answer": "Missing data"})]}] * 20
            with servers(script) as (mcp, _, url, requests, _, _):
                with redirect_stdout(io.StringIO()), patch.dict("os.environ", {"LLM_API_KEY": ""}):
                    code = runner.main(["--mcp-url", mcp.url, "--llm-url", url,
                                        "--model", "fake", "--questions", str(question_file),
                                        "--output", str(root / "run")])
            self.assertEqual(code, 0)
            self.assertEqual(len(requests), 20)
            prompts = json.dumps(requests)
            for sentinel in ("HIDDEN_ORACLE", "HIDDEN_EXPECTED", "HIDDEN_GRADING"):
                self.assertNotIn(sentinel, prompts)
            for question in fixture["questions"]:
                result = json.loads((root / "run" / (question["id"] + ".json")).read_text())
                self.assertEqual(result["question"], question["question"])
                for field in ("messages", "tool_calls", "sql", "final_answer", "timing"):
                    self.assertIn(field, result)
                self.assertNotIn("hidden", result)
            self.assertFalse(json.loads((root / "run/summary.json").read_text())["graded"])
            self.assertIn("| agent-20 |", (root / "run/summary.md").read_text())

    def test_fixture_coverage_and_extraction(self):
        fixture = json.loads((runner.ROOT / "tests/golden/agent_questions.json").read_text())
        questions = fixture["questions"]
        self.assertEqual(len(questions), 20)
        counts = Counter(q["class"] for q in questions)
        for number in range(1, 9):
            self.assertGreaterEqual(counts[f"Q{number}"], 2)
        self.assertTrue(any("alert" in q["question"] for q in questions))
        self.assertTrue(any("note" in q["question"] for q in questions))
        for question in questions:
            self.assertNotIn("ti_sql", question)
            self.assertIn("oracle_sql", question["hidden"])
            self.assertIn(question["hidden"]["grading"]["mode"], ("exact", "tolerance", "set"))
        self.assertEqual(expected.extract([{"n": 7}], {"mode": "scalar", "column": "n"}), 7)
        self.assertEqual(expected.extract([{"t": "b"}, {"t": "a"}, {"t": "b"}],
                                         {"mode": "column_set", "column": "t"}), ["a", "b"])
        with self.assertRaises(ValueError):
            expected.extract([], {"mode": "scalar", "column": "n"})

    def test_mcp_error_and_truncation_preserved(self):
        script = [{"tool_calls": [call("ti_query", {"sql": "SELECT bad"})]},
                  {"tool_calls": [call("ti_query", {"sql": "SELECT 1"}, 2)]},
                  {"tool_calls": [call("answer", {"answer": "Partial result"}, 3)]}]
        with servers(script) as (mcp, tools, url, requests, _, _):
            replies = [{"isError": True, "error": {"code": -32000, "message": "Bad SQL"}},
                       {"rows": [{"n": 7}], "truncated": True, "max_rows": 500}]
            with patch.object(mcp, "rpc", side_effect=replies):
                result = runner.run_question(self.question(), mcp, tools, url, "fake")
        self.assertEqual(result["status"], "completed")
        self.assertEqual(result["tool_calls"][0]["result"], replies[0])
        self.assertEqual(result["tool_calls"][1]["result"], replies[1])
        self.assertIn("Bad SQL", json.dumps(requests[1]["messages"]))
        self.assertIn("truncated", json.dumps(requests[2]["messages"]))

    def test_tools_pagination_and_reserved_name(self):
        mcp = runner.MCP("http://127.0.0.1:1/mcp")
        replies = [{}, None, {"tools": [TOOL], "nextCursor": "page2"},
                   {"tools": [{"name": "ti_schema", "inputSchema": {"type": "object"}}]}]
        with patch.object(mcp, "rpc", side_effect=replies) as rpc:
            self.assertEqual([t["name"] for t in mcp.tools()], ["ti_query", "ti_schema"])
            self.assertEqual(rpc.call_args_list[-1].args, ("tools/list", {"cursor": "page2"}))
        with patch.object(mcp, "rpc", side_effect=[{}, None, {"tools": [{"name": "answer"}]}]):
            with self.assertRaises(ValueError):
                mcp.tools()

    def test_transport_error_keeps_partial_transcript(self):
        with servers([]) as (mcp, tools, url, _, _, _):
            with patch.object(runner, "post", side_effect=RuntimeError("PRIVATE_REMOTE_BODY")):
                result = runner.run_question(self.question(), mcp, tools, url, "fake")
        self.assertEqual(result["status"], "error")
        self.assertEqual(result["error"], "RuntimeError")
        self.assertNotIn("PRIVATE_REMOTE_BODY", json.dumps(result))
        self.assertEqual(result["messages"][1]["content"], self.question()["question"])

    def test_loopback_and_caps(self):
        for url in ("http://0.0.0.0:1/mcp", "http://172.17.0.1:1/mcp",
                    "http://user:password@127.0.0.1:1/mcp"):
            with self.assertRaises(ValueError):
                runner.MCP(url)
        self.assertEqual(runner.endpoint("http://127.0.0.2:1/mcp", True),
                         "http://127.0.0.2:1/mcp")
        for turns, calls in ((13, 20), (12, 21), (0, 1)):
            with self.assertRaises(ValueError):
                runner.run_question(self.question(), None, [], "unused", "fake", turns, calls)


if __name__ == "__main__":
    unittest.main()
