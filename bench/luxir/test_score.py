import math
from pathlib import Path
import tempfile
import unittest
import score


class ScoreTests(unittest.TestCase):
    def test_hand_computed_metrics_include_missing_queries(self):
        qrels = {"q1": {"a": 2, "b": 1, "c": 0}, "q2": {"z": 1}}
        metrics, rows = score.evaluate(qrels, {"q1": ["c", "b", "a"]})
        dcg = 1 / math.log2(3) + 2 / math.log2(4)
        ideal = 2 + 1 / math.log2(3)
        self.assertAlmostEqual(rows["q1"]["ndcg_10"], dcg / ideal)
        self.assertEqual(rows["q1"]["recall_100"], 1)
        self.assertEqual(rows["q1"]["mrr_10"], .5)
        self.assertAlmostEqual(metrics["ndcg_10"], dcg / ideal / 2)
        self.assertEqual(metrics["recall_100"], .5)
        self.assertEqual(metrics["mrr_10"], .25)

    def test_depth_limits(self):
        docs = ["x" + str(i) for i in range(100)] + ["a"]
        result, _ = score.evaluate({"q": {"a": 1}}, {"q": docs})
        self.assertEqual(result, {"ndcg_10": 0, "recall_100": 0, "mrr_10": 0})

    def test_percentiles(self):
        self.assertEqual(score.percentile([1, 2, 3, 4], .5), 2.5)
        self.assertAlmostEqual(score.percentile([1, 2, 3, 4], .95), 3.85)

    def test_summary_reads_shared_contract(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "runs").mkdir()
            for dataset in score.DATASETS:
                (root / dataset).mkdir()
                (root / dataset / "qrels.tsv").write_text("q\ta\t1\n")
                name = "lume-bm25-" + dataset
                (root / "runs" / (name + ".trec")).write_text("q Q0 a 1 2 test\n")
                (root / "runs" / (name + ".lat.jsonl")).write_text('{"qid":"q","ms":3}\n')
            report = score.summarize(root)
            self.assertEqual(len(report["results"]), 2)
            self.assertTrue(all(row["ndcg_10"] == 1 for row in report["results"]))
            self.assertIn("not run", score.table(report))

    def test_incomplete_latency_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "lat.jsonl"
            path.write_text('{"qid":"q","ms":3}\n')
            with self.assertRaises(ValueError):
                score.read_latency(path, {"q", "missing"})

    def test_duplicate_document_run_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "run.trec"
            path.write_text("q Q0 a 1 2 test\nq Q0 a 2 1 test\n")
            with self.assertRaises(ValueError):
                score.read_run(path)


if __name__ == "__main__":
    unittest.main()
