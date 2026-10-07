import contextlib
import io
import json
from pathlib import Path
import tempfile
import unittest

from binary_size import compare, QUERY_IDS


class ProfileComparisonTests(unittest.TestCase):
    def report(self, factor=1, fingerprint="same"):
        return {"queries": [
            {"id": qid, "rows": 3, "answer_fingerprint": fingerprint,
             "p50_ms": (i + 1) * factor}
            for i, qid in enumerate(QUERY_IDS)
        ]}

    def write(self, root, name, report):
        path = root / name / "timing" / "cache-on.json"
        path.parent.mkdir(parents=True)
        path.write_text(json.dumps(report), encoding="utf-8")

    def test_every_query_regression_and_answers_are_reported(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.write(root, "fat", self.report())
            self.write(root, "small", self.report(1.05))
            with contextlib.redirect_stdout(io.StringIO()):
                compare(root, "fat")
            rows = json.loads((root / "comparison.json").read_text())["variants"]
            small = next(r for r in rows if r["label"] == "small")
            self.assertEqual(len(small["queries"]), 20)
            self.assertTrue(small["all_values_match"])
            self.assertTrue(small["within_five_percent_per_query"])
            self.assertAlmostEqual(small["max_query_regression_percent"], 5)
            self.assertAlmostEqual(small["median_p50_ms"], 11.025)

    def test_single_regressing_query_is_not_hidden_by_other_speedups(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.write(root, "fat", self.report())
            candidate = self.report(0.5)
            candidate["queries"][0]["p50_ms"] = 1.06
            self.write(root, "abort", candidate)
            with contextlib.redirect_stdout(io.StringIO()):
                compare(root, "fat")
            rows = json.loads((root / "comparison.json").read_text())["variants"]
            abort = next(r for r in rows if r["label"] == "abort")
            self.assertFalse(abort["within_five_percent_per_query"])
            self.assertLess(abort["sum_p50_change_percent"], 0)

    def test_clarified_gate_tolerates_submillisecond_jitter(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            baseline = self.report()
            for q in baseline["queries"]:
                q["p50_ms"] *= 0.01
            self.write(root, "fat", baseline)
            candidate = json.loads(json.dumps(baseline))
            candidate["queries"][0]["p50_ms"] *= 1.2
            self.write(root, "thin", candidate)
            compare(root, "fat", quiet=True)
            rows = json.loads((root / "comparison.json").read_text())["variants"]
            thin = next(r for r in rows if r["label"] == "thin")
            self.assertTrue(thin["passes_clarified_gate"])
            self.assertFalse(thin["within_five_percent_per_query"])
            self.assertEqual(thin["material_query_regressions"], [])

    def test_material_class_regression_is_flagged_despite_aggregate_speedup(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.write(root, "fat", self.report())
            candidate = self.report(0.5)
            for q in candidate["queries"]:
                if q["id"].startswith("q5-"):
                    q["p50_ms"] = 30
            self.write(root, "small", candidate)
            compare(root, "fat", quiet=True)
            rows = json.loads((root / "comparison.json").read_text())["variants"]
            small = next(r for r in rows if r["label"] == "small")
            self.assertLess(small["sum_p50_change_percent"], 5)
            self.assertFalse(small["passes_clarified_gate"])
            self.assertTrue(next(c for c in small["classes"] if c["class"] == "q5")["material_regression"])
            self.assertIn("q5-002", small["material_query_regressions"])

    def test_fingerprint_mismatch_rejected_even_when_counts_match(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.write(root, "fat", self.report())
            self.write(root, "thin", self.report(fingerprint="changed"))
            with self.assertRaisesRegex(RuntimeError, "Query answer changed"):
                compare(root, "fat")

    def test_adjacent_control_is_used_and_checked_against_fixed_answers(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.write(root, "fat", self.report())
            self.write(root, "small", self.report(1.1))
            control = root / "small/timing-fat-control/cache-on.json"
            control.parent.mkdir(parents=True)
            control.write_text(json.dumps(self.report(1.2)), encoding="utf-8")
            with contextlib.redirect_stdout(io.StringIO()):
                compare(root, "fat")
            rows = json.loads((root / "comparison.json").read_text())["variants"]
            small = next(r for r in rows if r["label"] == "small")
            self.assertEqual(small["baseline_kind"], "adjacent fat control")
            self.assertTrue(small["within_five_percent_per_query"])
            self.assertAlmostEqual(small["baseline_median_p50_ms"], 12.6)
            control.write_text(json.dumps(self.report(1.2, fingerprint="changed")), encoding="utf-8")
            with self.assertRaisesRegex(RuntimeError, "Control answer changed"):
                compare(root, "fat")
