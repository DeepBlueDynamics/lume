#!/usr/bin/env python3
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock
import urllib.error

import run_luxir
from run_luxir import (
    LuxirAdmin,
    embed_identified,
    embed_request,
    embedding_cache_path,
    get_dir_size,
    http_post,
    hybrid_collection_name,
    hybrid_run_name,
    read_queries,
    trec_tag,
    MemoryMonitor,
)


class _Resp:
    def __init__(self, payload, status=200):
        self.status = status
        self._raw = json.dumps(payload).encode("utf-8")

    def read(self):
        return self._raw

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False


class RunLuxirTests(unittest.TestCase):
    def test_get_dir_size(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            p = Path(tmpdir)
            (p / "f1.txt").write_bytes(b"12345")
            sub = p / "sub"
            sub.mkdir()
            (sub / "f2.txt").write_bytes(b"67890")
            self.assertEqual(get_dir_size(p), 10)

    def test_read_queries(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            p = Path(tmpdir)
            queries_path = p / "queries.tsv"
            qrels_path = p / "qrels.tsv"
            queries_path.write_text("q1\tquery one\nq2\tquery two\nq3\tquery three\n", encoding="utf-8")
            qrels_path.write_text("q1\td1\t1\nq3\td2\t2\n", encoding="utf-8")

            queries = read_queries(queries_path, qrels_path)
            self.assertEqual(len(queries), 2)
            self.assertEqual(queries[0], ("q1", "query one"))
            self.assertEqual(queries[1], ("q3", "query three"))

    def test_memory_monitor(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            p = Path(tmpdir)
            rss_path = p / "rss.json"
            rss_path.write_text(json.dumps({"current_rss_bytes": 1000}), encoding="utf-8")
            mon = MemoryMonitor(rss_path)
            mon.start()
            rss_path.write_text(json.dumps({"current_rss_bytes": 5000}), encoding="utf-8")
            import time
            time.sleep(0.15)
            peak, idle = mon.stop()
            self.assertGreaterEqual(peak, 5000)

    def test_gtr_request_is_texts_only(self):
        texts = ["a", "b"]
        body = embed_request("gtr-t5-base", texts, 256, "query")
        self.assertEqual(list(body.keys()), ["texts"])
        self.assertEqual(body, {"texts": ["a", "b"]})
        self.assertEqual(json.dumps(body), json.dumps({"texts": texts}))
        self.assertEqual(json.dumps(body), '{"texts": ["a", "b"]}')
        batch = ["say \"hi\"", "c"]
        sliced = batch[0:2]
        again = embed_request("gtr-t5-base", sliced, 768, "document")
        self.assertEqual(json.dumps(again), json.dumps({"texts": sliced}))

    def test_gemma_request_document_and_query(self):
        doc = embed_request("embeddinggemma-2", ["d"], 768, "document")
        self.assertEqual(list(doc.keys()), ["texts", "model", "task", "dimensions"])
        self.assertEqual(json.dumps(doc), json.dumps({
            "texts": ["d"],
            "model": "embeddinggemma-2",
            "task": "document",
            "dimensions": 768
        }))
        self.assertEqual(
            json.dumps(doc),
            '{"texts": ["d"], "model": "embeddinggemma-2", "task": "document", "dimensions": 768}'
        )
        qry = embed_request("embeddinggemma-2", ["q"], 768, "query")
        self.assertEqual(qry["task"], "query")
        self.assertEqual(
            json.dumps(qry),
            '{"texts": ["q"], "model": "embeddinggemma-2", "task": "query", "dimensions": 768}'
        )
        with self.assertRaises(ValueError):
            embed_request("embeddinggemma-2", ["q"], 768, "passage")
        with self.assertRaises(ValueError):
            embed_request("other", ["q"], 768, "query")

    def test_names_and_trec_tag(self):
        self.assertEqual(hybrid_collection_name("scifact", "gtr-t5-base"), "scifact_hybrid")
        self.assertEqual(hybrid_run_name("scifact", "gtr-t5-base"), "luxir-hybrid-scifact")
        self.assertEqual(hybrid_collection_name("trec-covid", "gtr-t5-base"), "trec_covid_hybrid")
        self.assertEqual(trec_tag("hybrid", "gtr-t5-base", "luxir-hybrid-scifact"), "luxir-hybrid")
        self.assertEqual(trec_tag("bm25", "embeddinggemma-2", "luxir-bm25-scifact"), "luxir-bm25")
        self.assertEqual(
            hybrid_collection_name("scifact", "embeddinggemma-2"),
            "scifact_hybrid_embgemma2"
        )
        self.assertEqual(
            hybrid_run_name("trec-covid", "embeddinggemma-2"),
            "luxir-hybrid-embgemma2-trec-covid"
        )
        self.assertEqual(
            trec_tag("hybrid", "embeddinggemma-2", "luxir-hybrid-embgemma2-scifact"),
            "luxir-hybrid-embgemma2-scifact"
        )

    def test_cache_path_jsonl(self):
        path = embedding_cache_path(
            "/workspace/lume/.lanes/data/luxir-bench",
            "embeddinggemma-2",
            768,
            "scifact",
            "docs"
        )
        self.assertEqual(
            path,
            Path("/workspace/lume/.lanes/data/luxir-bench/embeddings/embeddinggemma-2-768-scifact-docs.jsonl")
        )
        queries = embedding_cache_path(
            "/workspace/lume/.lanes/data/luxir-bench", "gtr-t5-base", 768, "trec-covid", "queries"
        )
        self.assertEqual(queries.name, "gtr-t5-base-768-trec-covid-queries.jsonl")
        with self.assertRaises(ValueError):
            embedding_cache_path("/tmp", "gtr-t5-base", 768, "scifact", "other")

    def test_embed_texts_default_posts_gtr_body(self):
        posts = []

        def fake_post(url, data=None, timeout=60, retries=3):
            posts.append((url, data))
            return 200, {"vectors": [[0.0] * 768 for _ in data["texts"]]}

        admin = LuxirAdmin(embed_endpoint="http://shivvr.test/embed")
        with mock.patch("run_luxir.http_post", fake_post):
            got = admin.embed_texts(["one", "two"])
        self.assertEqual(posts, [(
            "http://shivvr.test/embed",
            {"texts": ["one", "two"]}
        )])
        self.assertEqual(json.dumps(posts[0][1]), '{"texts": ["one", "two"]}')
        self.assertEqual(got, [[0.0] * 768, [0.0] * 768])

    def test_hybrid_schema_dims_stay_768(self):
        posts = []

        def fake_post(url, data=None, timeout=60, retries=3):
            posts.append((url, json.dumps(data)))
            return 200, {}

        admin = LuxirAdmin(endpoint="http://luxir-bench:9400")
        with mock.patch("run_luxir.http_post", fake_post):
            admin.create_collection("scifact_hybrid", mode="hybrid")
            admin.create_collection("scifact_hybrid", mode="hybrid", dims=768)
            admin.create_collection("scifact_hybrid_embgemma2", mode="hybrid", dims=768)
        expected = json.dumps({
            "fields": {
                "text": {"parent": "_t"},
                "embedding_v": {"type": "vector", "dims": 768, "metric": "cosine"}
            }
        })
        schemas = [body for url, body in posts if url.endswith("/_schema")]
        self.assertEqual(schemas, [expected, expected, expected])
        self.assertTrue(posts[2][0].endswith("/collections/scifact_hybrid/_schema"))
        self.assertTrue(posts[8][0].endswith("/collections/scifact_hybrid_embgemma2/_schema"))

    def test_cache_hit_does_not_call_shivvr(self):
        posts = []

        def fake_post(url, data=None, timeout=60, retries=3):
            posts.append(data)
            return 200, {"vectors": [[9.0]]}

        with tempfile.TemporaryDirectory() as tmp:
            path = embedding_cache_path(tmp, "embeddinggemma-2", 1, "scifact", "docs")
            path.parent.mkdir(parents=True)
            path.write_text(json.dumps({"id": "d1", "vector": [0.5]}) + "\n", encoding="utf-8")
            before = path.read_bytes()
            with mock.patch("run_luxir.http_post", fake_post):
                vecs = embed_identified(
                    LuxirAdmin(), [("d1", "text")], "embeddinggemma-2", 1, "document", path
                )
            self.assertEqual(vecs, [[0.5]])
            self.assertEqual(posts, [])
            self.assertEqual(path.read_bytes(), before)

    def test_partial_cache_posts_missing_in_order(self):
        posts = []

        def fake_post(url, data=None, timeout=60, retries=3):
            posts.append((url, data))
            return 200, {
                "model": "embeddinggemma-2",
                "dim": 2,
                "vectors": [[float(i + 1), 0.0] for i in range(len(data["texts"]))]
            }

        with tempfile.TemporaryDirectory() as tmp:
            path = embedding_cache_path(tmp, "embeddinggemma-2", 2, "scifact", "docs")
            path.parent.mkdir(parents=True)
            path.write_text(json.dumps({"id": "a", "vector": [0.0, 0.0]}) + "\n", encoding="utf-8")
            admin = LuxirAdmin(embed_endpoint="http://shivvr.test/embed")
            with mock.patch("run_luxir.http_post", fake_post):
                vecs = embed_identified(
                    admin,
                    [("a", "ta"), ("b", "tb"), ("c", "tc")],
                    "embeddinggemma-2",
                    2,
                    "document",
                    path,
                    batch_size=64
                )
            self.assertEqual(len(posts), 1)
            url, body = posts[0]
            self.assertEqual(url, "http://shivvr.test/embed")
            self.assertEqual(body["texts"], ["tb", "tc"])
            self.assertEqual(
                json.dumps(body),
                '{"texts": ["tb", "tc"], "model": "embeddinggemma-2", "task": "document", "dimensions": 2}'
            )
            self.assertEqual(vecs, [[0.0, 0.0], [1.0, 0.0], [2.0, 0.0]])
            rows = [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()]
            self.assertEqual([row["id"] for row in rows], ["a", "b", "c"])
            self.assertEqual(rows[2]["vector"], [2.0, 0.0])

    def test_gemma_query_task_is_query(self):
        posts = []

        def fake_post(url, data=None, timeout=60, retries=3):
            posts.append(data)
            return 200, {"model": "embeddinggemma-2", "dim": 2, "vectors": [[0.1, 0.2]]}

        with tempfile.TemporaryDirectory() as tmp:
            path = embedding_cache_path(tmp, "embeddinggemma-2", 2, "scifact", "queries")
            with mock.patch("run_luxir.http_post", fake_post):
                vecs = embed_identified(
                    LuxirAdmin(embed_endpoint="http://shivvr.test/embed"),
                    [("q1", "what")],
                    "embeddinggemma-2",
                    2,
                    "query",
                    path
                )
            self.assertEqual(vecs, [[0.1, 0.2]])
            self.assertEqual(posts[0]["task"], "query")
            self.assertEqual(list(posts[0].keys()), ["texts", "model", "task", "dimensions"])

    def test_batches_of_two_for_three_texts(self):
        posts = []

        def fake_post(url, data=None, timeout=60, retries=3):
            posts.append(data)
            return 200, {"vectors": [[1.0] for _ in data["texts"]]}

        with tempfile.TemporaryDirectory() as tmp:
            path = embedding_cache_path(tmp, "gtr-t5-base", 1, "scifact", "docs")
            with mock.patch("run_luxir.http_post", fake_post):
                vecs = embed_identified(
                    LuxirAdmin(),
                    [("1", "t1"), ("2", "t2"), ("3", "t3")],
                    "gtr-t5-base",
                    1,
                    "document",
                    path,
                    batch_size=2
                )
            self.assertEqual(vecs, [[1.0], [1.0], [1.0]])
            self.assertEqual(len(posts), 2)
            self.assertEqual(json.dumps(posts[0]), '{"texts": ["t1", "t2"]}')
            self.assertEqual(json.dumps(posts[1]), '{"texts": ["t3"]}')
            self.assertNotIn("model", posts[0])

    def test_later_batch_keeps_earlier_ids(self):
        posts = []

        def fake_post(url, data=None, timeout=60, retries=3):
            posts.append(list(data["texts"]))
            return 200, {"vectors": [[9.0] for _ in data["texts"]]}

        with tempfile.TemporaryDirectory() as tmp:
            path = embedding_cache_path(tmp, "gtr-t5-base", 1, "scifact", "docs")
            admin = LuxirAdmin()
            with mock.patch("run_luxir.http_post", fake_post):
                embed_identified(
                    admin, [("a", "ta"), ("b", "tb")], "gtr-t5-base", 1, "document", path
                )
                embed_identified(
                    admin, [("c", "tc")], "gtr-t5-base", 1, "document", path
                )
            ids = [json.loads(line)["id"] for line in path.read_text(encoding="utf-8").splitlines()]
            self.assertEqual(ids, ["a", "b", "c"])
            self.assertEqual(posts, [["ta", "tb"], ["tc"]])

    def test_gtr_post_retries_and_body_stays_texts_only(self):
        seen = []

        def fake_urlopen(req, timeout=60):
            seen.append(req.data)
            if len(seen) == 1:
                raise urllib.error.URLError("down")
            return _Resp({"vectors": [[0.25, 0.5]]})

        admin = LuxirAdmin(embed_endpoint="http://shivvr.test/embed")
        with tempfile.TemporaryDirectory() as tmp:
            path = embedding_cache_path(tmp, "gtr-t5-base", 2, "scifact", "queries")
            with mock.patch("run_luxir.urllib.request.urlopen", fake_urlopen), \
                 mock.patch("run_luxir.time.sleep", lambda *_a, **_k: None):
                vecs = embed_identified(
                    admin,
                    [("q1", 'say "hi"')],
                    "gtr-t5-base",
                    2,
                    "query",
                    path
                )
        self.assertEqual(vecs, [[0.25, 0.5]])
        expected = json.dumps({"texts": ['say "hi"']}).encode("utf-8")
        self.assertEqual(seen, [expected, expected])
        self.assertEqual(seen[0].decode("utf-8"), '{"texts": ["say \\"hi\\""]}')
        self.assertNotIn(b"model", seen[0])
        self.assertNotIn(b"task", seen[0])
        self.assertNotIn(b"dimensions", seen[0])
        # http_post itself is what retried; a direct call agrees on the body bytes.
        seen.clear()
        with mock.patch("run_luxir.urllib.request.urlopen", fake_urlopen), \
             mock.patch("run_luxir.time.sleep", lambda *_a, **_k: None):
            status, resp = http_post("http://shivvr.test/embed", {"texts": ['say "hi"']})
        self.assertEqual(status, 200)
        self.assertEqual(resp["vectors"], [[0.25, 0.5]])
        self.assertEqual(seen, [expected, expected])

    def test_gemma_dim_mismatch_raises(self):
        def fake_post(url, data=None, timeout=60, retries=3):
            return 200, {"model": "embeddinggemma-2", "dim": 512, "vectors": [[0.0] * 512]}

        with tempfile.TemporaryDirectory() as tmp:
            path = embedding_cache_path(tmp, "embeddinggemma-2", 768, "scifact", "docs")
            with mock.patch("run_luxir.http_post", fake_post):
                with self.assertRaises(RuntimeError):
                    embed_identified(
                        LuxirAdmin(),
                        [("d1", "text")],
                        "embeddinggemma-2",
                        768,
                        "document",
                        path
                    )
            self.assertFalse(path.exists())

    def test_gemma_model_mismatch_raises(self):
        def fake_post(url, data=None, timeout=60, retries=3):
            return 200, {"model": "gtr-t5-base", "dim": 2, "vectors": [[0.1, 0.2]]}

        with tempfile.TemporaryDirectory() as tmp:
            path = embedding_cache_path(tmp, "embeddinggemma-2", 2, "scifact", "queries")
            with mock.patch("run_luxir.http_post", fake_post):
                with self.assertRaises(RuntimeError):
                    embed_identified(
                        LuxirAdmin(), [("q", "text")], "embeddinggemma-2", 2, "query", path
                    )

    def test_vector_length_mismatch_raises_for_gtr(self):
        def fake_post(url, data=None, timeout=60, retries=3):
            return 200, {"vectors": [[1.0, 2.0, 3.0]]}

        with tempfile.TemporaryDirectory() as tmp:
            path = embedding_cache_path(tmp, "gtr-t5-base", 2, "scifact", "docs")
            with mock.patch("run_luxir.http_post", fake_post):
                with self.assertRaises(RuntimeError):
                    embed_identified(
                        LuxirAdmin(), [("d", "text")], "gtr-t5-base", 2, "document", path
                    )


if __name__ == "__main__":
    unittest.main()
