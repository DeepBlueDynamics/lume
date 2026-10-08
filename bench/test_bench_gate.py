import importlib.util
import io
import json
import os
from pathlib import Path
import tempfile
import unittest

# Load bench-gate.py dynamically since the script name contains a hyphen
SCRIPT_PATH = Path(__file__).resolve().parents[1] / "scripts" / "bench-gate.py"
spec = importlib.util.spec_from_file_location("bench_gate", str(SCRIPT_PATH))
bench_gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bench_gate)


class BenchGateTests(unittest.TestCase):
    def sample_run(self, queries=None):
        if queries is None:
            queries = [
                {
                    "id": "q1-001",
                    "class": "Q1",
                    "rows": 1,
                    "answer_fingerprint": "1f440259e69227e8",
                    "cold_ms": 25.0,
                    "p50_ms": 0.55,
                    "p95_ms": 0.95,
                    "description": "Point lookup: true wind speed",
                },
                {
                    "id": "q2-002",
                    "class": "Q2",
                    "rows": 242,
                    "answer_fingerprint": "52a7a90eae451523",
                    "cold_ms": 30.0,
                    "p50_ms": 1.20,
                    "p95_ms": 20.0,
                    "description": "Selective multi-pred",
                },
            ]
        return {
            "date": "2026-10-08",
            "git_sha": "0eff8df",
            "iterations": 7,
            "queries": queries,
        }

    def sample_baseline(self, queries=None):
        if queries is None:
            queries = [
                {
                    "id": "q1-001",
                    "class": "Q1",
                    "rows": 1,
                    "answer_fingerprint": "1f440259e69227e8",
                    "cold_ms": 28.0,
                    "p50_ms": 0.58,
                    "p95_ms": 0.96,
                    "description": "Point lookup: true wind speed",
                },
                {
                    "id": "q2-002",
                    "class": "Q2",
                    "rows": 242,
                    "answer_fingerprint": "52a7a90eae451523",
                    "cold_ms": 32.0,
                    "p50_ms": 1.15,
                    "p95_ms": 20.0,
                    "description": "Selective multi-pred",
                },
            ]
        return {
            "date": "2026-10-01",
            "runner": "ubuntu-22.04",
            "queries": queries,
        }

    def test_pass(self):
        """Clean run with matching rows, fingerprints, and p95 within thresholds passes."""
        run = self.sample_run()
        baseline = self.sample_baseline()
        result = bench_gate.evaluate(run, baseline)
        self.assertTrue(result["passed"])
        self.assertEqual(result["failed_count"], 0)
        self.assertEqual(result["passed_count"], 2)
        self.assertEqual(result["mode"], "comparison")

    def test_regression_past_both_thresholds(self):
        """Warm p95 > baseline * 1.15 AND exceeds baseline by >= 2.0 ms fails."""
        # Baseline p95 is 20.0 ms. Run p95 is 25.0 ms (+25% > 15%, delta = 5.0 ms >= 2.0 ms).
        run_queries = [
            {
                "id": "q2-002",
                "class": "Q2",
                "rows": 242,
                "answer_fingerprint": "52a7a90eae451523",
                "p95_ms": 25.0,
            }
        ]
        base_queries = [
            {
                "id": "q2-002",
                "class": "Q2",
                "rows": 242,
                "answer_fingerprint": "52a7a90eae451523",
                "p95_ms": 20.0,
            }
        ]
        result = bench_gate.evaluate(
            self.sample_run(run_queries),
            self.sample_baseline(base_queries),
            ratio_threshold=1.15,
            noise_floor_ms=2.0,
        )
        self.assertFalse(result["passed"])
        self.assertEqual(result["failed_count"], 1)
        failed_row = result["rows"][0]
        self.assertEqual(failed_row["status"], "FAIL")
        self.assertIn("p95 regression", failed_row["reason"])

    def test_noise_floor(self):
        """Warm p95 > baseline * 1.15 but within absolute noise floor (< 2.0 ms) does NOT fail."""
        # Baseline p95 is 0.50 ms. Run p95 is 0.90 ms (+80% > 15%, but delta = 0.40 ms < 2.0 ms).
        run_queries = [
            {
                "id": "q1-001",
                "class": "Q1",
                "rows": 1,
                "answer_fingerprint": "1f440259e69227e8",
                "p95_ms": 0.90,
            }
        ]
        base_queries = [
            {
                "id": "q1-001",
                "class": "Q1",
                "rows": 1,
                "answer_fingerprint": "1f440259e69227e8",
                "p95_ms": 0.50,
            }
        ]
        result = bench_gate.evaluate(
            self.sample_run(run_queries),
            self.sample_baseline(base_queries),
            ratio_threshold=1.15,
            noise_floor_ms=2.0,
        )
        self.assertTrue(result["passed"])
        self.assertEqual(result["failed_count"], 0)
        row = result["rows"][0]
        self.assertEqual(row["status"], "PASS")
        self.assertIn("Noise floor", row["reason"])

    def test_fingerprint_change(self):
        """Answer fingerprint mismatch fails immediately."""
        run_queries = [
            {
                "id": "q1-001",
                "class": "Q1",
                "rows": 1,
                "answer_fingerprint": "deadbeef12345678",
                "p95_ms": 0.95,
            }
        ]
        base_queries = [
            {
                "id": "q1-001",
                "class": "Q1",
                "rows": 1,
                "answer_fingerprint": "1f440259e69227e8",
                "p95_ms": 0.95,
            }
        ]
        result = bench_gate.evaluate(
            self.sample_run(run_queries),
            self.sample_baseline(base_queries),
        )
        self.assertFalse(result["passed"])
        self.assertEqual(result["failed_count"], 1)
        self.assertEqual(result["rows"][0]["status"], "FAIL")
        self.assertIn("Answer fingerprint changed", result["rows"][0]["reason"])

    def test_row_count_change(self):
        """Row count change fails immediately."""
        run_queries = [
            {
                "id": "q2-002",
                "class": "Q2",
                "rows": 240,
                "answer_fingerprint": "52a7a90eae451523",
                "p95_ms": 20.0,
            }
        ]
        base_queries = [
            {
                "id": "q2-002",
                "class": "Q2",
                "rows": 242,
                "answer_fingerprint": "52a7a90eae451523",
                "p95_ms": 20.0,
            }
        ]
        result = bench_gate.evaluate(
            self.sample_run(run_queries),
            self.sample_baseline(base_queries),
        )
        self.assertFalse(result["passed"])
        self.assertEqual(result["failed_count"], 1)
        self.assertEqual(result["rows"][0]["status"], "FAIL")
        self.assertIn("Row count changed", result["rows"][0]["reason"])

    def test_missing_baseline(self):
        """Missing baseline reports 'no baseline, recording only' and passes (exit 0)."""
        run = self.sample_run()
        result = bench_gate.evaluate(run, baseline_data=None)
        self.assertTrue(result["passed"])
        self.assertEqual(result["mode"], "recording_only")
        self.assertEqual(result["message"], "no baseline, recording only")
        self.assertEqual(result["failed_count"], 0)
        self.assertEqual(len(result["rows"]), 2)

    def test_placeholder_baseline(self):
        """Placeholder baseline reports 'no baseline, recording only' and passes (exit 0)."""
        run = self.sample_run()
        placeholder = {"placeholder": True, "runner": "ubuntu-22.04"}
        result = bench_gate.evaluate(run, baseline_data=placeholder)
        self.assertTrue(result["passed"])
        self.assertEqual(result["mode"], "recording_only")
        self.assertEqual(result["message"], "no baseline, recording only")

    def test_nested_cache_on_format(self):
        """Extracts queries from native_q6 composite report format (data.cache_on.queries)."""
        run = {
            "cache_on": {
                "queries": [
                    {"id": "q1-001", "rows": 1, "answer_fingerprint": "fp1", "p95_ms": 1.0}
                ]
            }
        }
        baseline = {
            "cache_on": {
                "queries": [
                    {"id": "q1-001", "rows": 1, "answer_fingerprint": "fp1", "p95_ms": 1.0}
                ]
            }
        }
        result = bench_gate.evaluate(run, baseline)
        self.assertTrue(result["passed"])
        self.assertEqual(result["passed_count"], 1)

    def test_cli_end_to_end(self):
        """Tests CLI execution with files and step summary."""
        with tempfile.TemporaryDirectory() as tmp_dir:
            tmp = Path(tmp_dir)
            run_file = tmp / "run.json"
            base_file = tmp / "baseline.json"
            summary_file = tmp / "summary.md"

            run_file.write_text(json.dumps(self.sample_run()), encoding="utf-8")
            base_file.write_text(json.dumps(self.sample_baseline()), encoding="utf-8")

            # 1. Clean run -> exit 0
            code = bench_gate.main([
                "--run", str(run_file),
                "--baseline", str(base_file),
                "--summary-file", str(summary_file),
            ])
            self.assertEqual(code, 0)
            self.assertTrue(summary_file.exists())
            self.assertIn("✅ PASS", summary_file.read_text(encoding="utf-8"))

            # 2. Regressing run -> exit 1
            bad_run = self.sample_run([
                {"id": "q1-001", "rows": 1, "answer_fingerprint": "1f440259e69227e8", "p95_ms": 50.0},
                {"id": "q2-002", "rows": 242, "answer_fingerprint": "52a7a90eae451523", "p95_ms": 20.0},
            ])
            run_file.write_text(json.dumps(bad_run), encoding="utf-8")
            code = bench_gate.main([
                "--run", str(run_file),
                "--baseline", str(base_file),
            ])
            self.assertEqual(code, 1)

            # 3. Missing baseline -> exit 0 (recording only)
            missing_base = tmp / "nonexistent.json"
            summary_file.unlink(missing_ok=True)
            code = bench_gate.main([
                "--run", str(run_file),
                "--baseline", str(missing_base),
                "--summary-file", str(summary_file),
            ])
            self.assertEqual(code, 0)
            self.assertIn("RECORDING ONLY", summary_file.read_text(encoding="utf-8"))


if __name__ == "__main__":
    unittest.main()
