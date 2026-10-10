#!/usr/bin/env python3
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import MagicMock, patch

from run_luxir import (
    DEFAULT_FACET_OPS,
    LuxirAdmin,
    MemoryMonitor,
    build_payload,
    get_dir_size,
    index_dataset,
    read_queries,
)


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

    def test_build_payload_bm25_without_ops(self):
        payload = build_payload("bm25", "cancer therapy", limit=50)
        self.assertEqual(payload["query"], {"match": {"text": "cancer therapy"}})
        self.assertEqual(payload["limit"], 50)
        self.assertTrue(payload["get_scores"])
        self.assertNotIn("ops", payload)

    def test_build_payload_bm25_with_facet_ops(self):
        payload = build_payload("bm25", "cancer therapy", limit=50, ops=DEFAULT_FACET_OPS)
        self.assertEqual(payload["query"], {"match": {"text": "cancer therapy"}})
        self.assertIn("ops", payload)
        self.assertIn("category", payload["ops"])
        self.assertIn("tags", payload["ops"])
        self.assertIn("year", payload["ops"])
        self.assertEqual(payload["ops"]["category"]["field_facet"]["field"], "category")
        self.assertEqual(payload["ops"]["year"]["range_facet"]["gap"], 5)

    def test_build_payload_hybrid_with_facet_ops(self):
        vec = [0.1] * 768
        payload = build_payload("hybrid", "cancer therapy", vector=vec, limit=50, ops=DEFAULT_FACET_OPS)
        self.assertIn("ops", payload)
        self.assertIn("hybrid", payload["ops"])
        self.assertIn("category", payload["ops"])
        self.assertIn("tags", payload["ops"])
        self.assertIn("year", payload["ops"])
        self.assertIn("fusion", payload["ops"]["hybrid"])

    def test_create_collection_with_meta_schema(self):
        admin = LuxirAdmin(endpoint="http://dummy:9400")
        posts = []

        def mock_post(url, data=None, **kwargs):
            posts.append((url, data))
            return 200, {"status": "ok"}

        with patch("run_luxir.http_post", side_effect=mock_post):
            admin.create_collection("test_coll", mode="bm25", with_meta=True)

        schema_post = [p for p in posts if p[0].endswith("/_schema")]
        self.assertEqual(len(schema_post), 1)
        fields = schema_post[0][1]["fields"]
        self.assertEqual(fields["year"], {"type": "int"})
        self.assertEqual(fields["category"], {"type": "string"})
        self.assertEqual(fields["n_cites"], {"type": "int"})
        self.assertEqual(fields["published_at"], {"type": "date"})
        self.assertEqual(fields["tags"], {"type": "string", "multi": True})

    def test_index_dataset_with_meta(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            p = Path(tmpdir)
            docs_meta = p / "docs_meta.jsonl"
            docs_meta.write_text(
                json.dumps({
                    "id": "doc1",
                    "text": "sample text",
                    "year": 2021,
                    "category": "biology",
                    "n_cites": 42,
                    "published_at": "2021-06-01T00:00:00Z",
                    "tags": ["tag_01", "tag_02"]
                }) + "\n",
                encoding="utf-8"
            )

            admin = MagicMock()
            mem_monitor = MagicMock()
            mem_monitor.stop.return_value = (1000, 500)

            stats = index_dataset(
                admin=admin,
                dataset_dir=p,
                collection="test_meta",
                mode="bm25",
                mem_monitor=mem_monitor,
                luxir_data_dir=p / "luxir_data",
                batch_size=10,
                with_meta=True
            )

            admin.create_collection.assert_called_once_with("test_meta", mode="bm25", with_meta=True)
            self.assertEqual(admin.update_docs.call_count, 1)
            call_args = admin.update_docs.call_args[0]
            coll_arg, batch_arg = call_args[0], call_args[1]
            self.assertEqual(coll_arg, "test_meta")
            self.assertEqual(len(batch_arg), 1)
            doc = batch_arg[0]
            self.assertEqual(doc["id"], "doc1")
            self.assertEqual(doc["year"], 2021)
            self.assertEqual(doc["category"], "biology")
            self.assertEqual(doc["n_cites"], 42)
            self.assertEqual(doc["published_at"], "2021-06-01T00:00:00Z")
            self.assertEqual(doc["tags"], ["tag_01", "tag_02"])

    def test_index_dataset_with_meta_missing_source_fails(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            p = Path(tmpdir)
            admin = MagicMock()
            mem_monitor = MagicMock()
            with self.assertRaises(FileNotFoundError) as ctx:
                index_dataset(
                    admin=admin,
                    dataset_dir=p,
                    collection="test_meta",
                    mode="bm25",
                    mem_monitor=mem_monitor,
                    luxir_data_dir=p / "luxir_data",
                    with_meta=True
                )
            self.assertIn("docs_meta.jsonl", str(ctx.exception))


if __name__ == "__main__":
    unittest.main()
