#!/usr/bin/env python3
import json
from pathlib import Path
import tempfile
import unittest

from run_luxir import get_dir_size, read_queries, MemoryMonitor


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


if __name__ == "__main__":
    unittest.main()
