import unittest
from native_cache_warm import summarize
from shard_cache import QUERY_IDS, TEXT_QUERY_IDS


class ColdAfterWarmTests(unittest.TestCase):
    def fixture(self):
        queries = [
            dict(id=qid, **{"class": qid.split("-")[0].upper()},
                 rows=1, answer_fingerprint=qid, cold_ms=3,
                 cache_warm={"bytes": 32, "budget_bytes": 64})
            for qid in QUERY_IDS + TEXT_QUERY_IDS
        ]
        baseline = dict(cache_on={"queries": queries},
                        classes={f"Q{i}": {"on": {"cold_ms": 100}} for i in range(1, 9)})
        return {"queries": queries}, baseline

    def test_all_classes_compared_and_q8_is_not_invented(self):
        report, baseline = self.fixture()
        classes = summarize(report, baseline)
        self.assertEqual(len(classes), 8)
        self.assertEqual(classes["Q6"]["status"], "PASS")
        self.assertEqual(classes["Q8"]["status"], "PENDING")
        self.assertIsNone(classes["Q8"]["edge_target_ms"])
        self.assertEqual(classes["Q1"]["previous_cold_ms"], 100)

    def test_first_query_miss_is_not_hidden_by_warm_iterations(self):
        report, baseline = self.fixture()
        report["queries"][0]["cold_ms"] = 21
        classes = summarize(report, baseline)
        self.assertEqual(classes["Q1"]["status"], "MISS")
        self.assertEqual(classes["Q1"]["slowest_query"], "q1-001")

    def test_answer_mismatch_budget_overflow_and_missing_query_rejected(self):
        import copy
        original, baseline = self.fixture()
        for change in (
            lambda r: r["queries"][0].update(answer_fingerprint="wrong"),
            lambda r: r["queries"][0].update(cache_warm={"bytes": 65, "budget_bytes": 64}),
            lambda r: r["queries"].pop(),
        ):
            report = copy.deepcopy(original)
            change(report)
            with self.assertRaises(ValueError):
                summarize(report, baseline)
