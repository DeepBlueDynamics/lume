import unittest
from shard_cache import compare, QUERY_IDS


class CacheComparisonTests(unittest.TestCase):
    def report(self, **changes):
        row = dict(id="q1", rows=1, answer_fingerprint="abc", cold_ms=10,
                   p50_ms=8, cache_stats={"used_bytes": 32, "budget_bytes": 64})
        row.update(changes)
        return {"queries": [row]}

    def test_values_and_row_counts_are_checked(self):
        self.assertTrue(compare(self.report(), self.report(p50_ms=1))[0]["values_match"])
        self.assertFalse(compare(self.report(), self.report(answer_fingerprint="def"))[0]["values_match"])
        self.assertFalse(compare(self.report(), self.report(rows=2))[0]["values_match"])

    def test_missing_queries_and_budget_overflow_fail(self):
        with self.assertRaises(ValueError):
            compare(self.report(), {"queries": []})
        with self.assertRaises(ValueError):
            compare(self.report(), self.report(cache_stats={"used_bytes": 65, "budget_bytes": 64}))

    def test_twenty_unique_queries(self):
        self.assertEqual(len(QUERY_IDS), 20)
        self.assertEqual(len(set(QUERY_IDS)), 20)
