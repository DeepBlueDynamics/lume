import unittest
from h3_quality import query


class FakeClient:
    def __init__(self, reply):
        self.reply = reply
        self.requests = []

    def post(self, payload):
        self.requests.append(payload)
        return self.reply


class H3Tests(unittest.TestCase):
    def test_local_search_passes_alpha_and_returns_document_hits(self):
        client = FakeClient({"result": {"content": [{"type": "text",
            "text": "[1] Hybrid Score: 0.9 (File: /source/a.txt, Line: 1)"}]}})
        self.assertEqual(query(client, "/db", "captain", .7), [("a", .9)])
        args = client.requests[0]["params"]["arguments"]
        self.assertEqual(args["alpha"], .7)
        self.assertEqual(args["query"], "captain")
        self.assertEqual(args["graph"], 0)

    def test_fallback_and_mcp_errors_are_not_scored(self):
        for reply in [
            {"error": {"message": "error"}},
            {"result": {"isError": True}},
            {"result": {"content": [{"type": "text", "text": "Semantic search unavailable"}]}},
        ]:
            with self.assertRaises(RuntimeError):
                query(FakeClient(reply), "/db", "captain", .5)


if __name__ == "__main__":
    unittest.main()
