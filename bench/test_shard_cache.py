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

    def test_class_only_reports_are_timings_not_value_checks(self):
        from shard_cache import markdown
        a = {"classes": {"Q5": {"cold_ms": 4301, "p50_ms": 4275}}}
        b = {"queries": [], "classes": {"Q5": {"cold_ms": 1567, "p50_ms": 296}}}
        rows = compare(a, b)
        self.assertEqual(rows[0]["id"], "Q5")
        self.assertIsNone(rows[0]["values_match"])
        self.assertIn("not checked (class summary)", markdown(rows))
        with self.assertRaises(ValueError):
            compare(a, {"classes": {}})
        with self.assertRaises(ValueError):
            compare({"queries": []}, {"queries": []})
        with self.assertRaises(ValueError):
            markdown([])

    def test_cache_on_report_requires_fingerprints_and_checks_budget(self):
        from shard_cache import cache_on_summary, cache_on_markdown
        rows = cache_on_summary(self.report())
        self.assertIn("| q1 |", cache_on_markdown(rows))
        with self.assertRaises(ValueError):
            cache_on_summary({"classes": {"Q1": {"p50_ms": 1}}})
        with self.assertRaises(ValueError):
            cache_on_summary(self.report(answer_fingerprint=None))
        with self.assertRaises(ValueError):
            cache_on_summary(self.report(cache_stats={"used_bytes": 65, "budget_bytes": 64}))
        with self.assertRaises(ValueError):
            cache_on_summary({"queries": rows * 2})
