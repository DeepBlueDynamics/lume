#!/usr/bin/env python3
"""Unit tests for Pi ingest benchmark summarizer (summarize_ingest.py).

Verifies:
- Parsing of benchmark CSV files into typed rows.
- Calculation of mean and p95 ingest throughput (rows/s).
- Tracking of peak (max) RSS in KiB and MiB.
- CPU utilization (mean, max, p95).
- Temperature tracking (max, mean, min).
- Decoding of Raspberry Pi `vcgencmd get_throttled` bitmasks.
- Tracking of storage growth (WAL and shards).
- Tracking of ingest_blocked and apply_failures counter deltas.
- Markdown summary generation.
- CLI execution and output file generation.
"""

import io
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

# Import summarizer module
SCRIPT_DIR = Path(__file__).parent.resolve()
sys.path.insert(0, str(SCRIPT_DIR))

import summarize_ingest as summarizer


class TestPiIngestSummarize(unittest.TestCase):
    def setUp(self):
        self.fixture_csv = SCRIPT_DIR / "fixtures" / "mock_pi_ingest.csv"
        self.assertTrue(self.fixture_csv.is_file(), f"Fixture missing: {self.fixture_csv}")

    def test_parse_throttled_bits(self):
        # 0x0 / none
        mask, flags = summarizer.parse_throttled_bits("0x0")
        self.assertEqual(mask, 0)
        self.assertEqual(flags, [])

        mask, flags = summarizer.parse_throttled_bits("n/a")
        self.assertEqual(mask, 0)
        self.assertEqual(flags, [])

        # 0x50000: bits 16 (Under-voltage has occurred) and 18 (Throttling has occurred)
        mask, flags = summarizer.parse_throttled_bits("0x50000")
        self.assertEqual(mask, 0x50000)
        self.assertIn("Under-voltage has occurred", flags)
        self.assertIn("Throttling has occurred", flags)
        self.assertNotIn("Currently throttled", flags)

        # 0x50005: bits 0, 2, 16, 18
        mask, flags = summarizer.parse_throttled_bits("0x50005")
        self.assertEqual(mask, 0x50005)
        self.assertIn("Under-voltage detected", flags)
        self.assertIn("Currently throttled", flags)
        self.assertIn("Under-voltage has occurred", flags)
        self.assertIn("Throttling has occurred", flags)

    def test_calculate_percentile(self):
        # Empty list
        self.assertEqual(summarizer.calculate_percentile([], 95.0), 0.0)

        # Single element
        self.assertEqual(summarizer.calculate_percentile([42.0], 95.0), 42.0)

        # 1 to 10
        vals = [float(x) for x in range(1, 11)]
        # p50 = 5.5
        self.assertAlmostEqual(summarizer.calculate_percentile(vals, 50.0), 5.5)
        # p0 = 1.0, p100 = 10.0
        self.assertAlmostEqual(summarizer.calculate_percentile(vals, 0.0), 1.0)
        self.assertAlmostEqual(summarizer.calculate_percentile(vals, 100.0), 10.0)
        # p95 = 9.55
        self.assertAlmostEqual(summarizer.calculate_percentile(vals, 95.0), 9.55)

    def test_format_bytes(self):
        self.assertEqual(summarizer.format_bytes(512), "512 B")
        self.assertEqual(summarizer.format_bytes(2048), "2.0 KiB")
        self.assertEqual(summarizer.format_bytes(1048576), "1.00 MiB")
        self.assertEqual(summarizer.format_bytes(1073741824), "1.00 GiB")

    def test_parse_benchmark_csv(self):
        rows = summarizer.parse_benchmark_csv(self.fixture_csv)
        self.assertEqual(len(rows), 10)

        first = rows[0]
        self.assertEqual(first["elapsed_sec"], 0.0)
        self.assertEqual(first["pid"], 4242)
        self.assertEqual(first["rss_kb"], 45000.0)
        self.assertEqual(first["records_ingested"], 100000)
        self.assertEqual(first["rows_per_sec"], 0.0)
        self.assertEqual(first["wal_bytes"], 1048576)
        self.assertEqual(first["shard_bytes"], 5242880)
        self.assertEqual(first["temp_c"], 48.5)
        self.assertEqual(first["throttled"], "0x0")

        last = rows[-1]
        self.assertEqual(last["elapsed_sec"], 90.0)
        self.assertEqual(last["records_ingested"], 113500)
        self.assertEqual(last["rows_per_sec"], 140.0)
        self.assertEqual(last["temp_c"], 50.8)

    def test_summarize_rows(self):
        rows = summarizer.parse_benchmark_csv(self.fixture_csv)
        m = summarizer.summarize_rows(rows)

        self.assertEqual(m["samples"], 10)
        self.assertEqual(m["duration_sec"], 90.0)
        self.assertEqual(m["pid"], 4242)
        self.assertEqual(m["records_start"], 100000)
        self.assertEqual(m["records_end"], 113500)
        self.assertEqual(m["records_ingested_total"], 13500)

        # Mean throughput: 13500 records / 90 s = 150.0 rows/s
        self.assertAlmostEqual(m["rows_per_sec_mean"], 150.0, places=1)

        # p95 throughput should be between 150 and 160
        self.assertGreater(m["rows_per_sec_p95"], 150.0)
        self.assertLessEqual(m["rows_per_sec_p95"], 160.0)
        self.assertEqual(m["rows_per_sec_max"], 160.0)
        self.assertEqual(m["rows_per_sec_min"], 140.0)

        # RSS max should be 52000 KiB = ~50.78 MiB
        self.assertEqual(m["rss_max_kb"], 52000.0)
        self.assertAlmostEqual(m["rss_max_mib"], 52000.0 / 1024.0, places=2)

        # Temperature
        self.assertEqual(m["temp_max_c"], 53.0)
        self.assertEqual(m["temp_min_c"], 48.5)

        # Throttling bitmask should capture 0x50000
        self.assertEqual(m["throttled_mask"], 0x50000)
        self.assertIn("Under-voltage has occurred", m["throttled_flags"])
        self.assertIn("Throttling has occurred", m["throttled_flags"])

        # Storage
        self.assertEqual(m["wal_bytes_start"], 1048576)
        self.assertEqual(m["wal_bytes_end"], 1992294)
        self.assertEqual(m["shard_bytes_start"], 5242880)
        self.assertEqual(m["shard_bytes_end"], 6291456)

        # Counters delta
        self.assertEqual(m["ingest_blocked_delta"], 0)
        self.assertEqual(m["apply_failures_delta"], 0)

    def test_generate_markdown(self):
        rows = summarizer.parse_benchmark_csv(self.fixture_csv)
        m = summarizer.summarize_rows(rows)
        md = summarizer.generate_markdown(m)

        self.assertIn("# Pi Ingest Benchmark Summary", md)
        self.assertIn("`4242`", md)
        self.assertIn("**150.0 rows/s**", md)
        self.assertIn(f"**{m['rss_max_mib']:.1f} MiB**", md)
        self.assertIn("53.0°C", md)
        self.assertIn("0x50000", md)
        self.assertIn("Throttling has occurred", md)
        self.assertIn("Under-voltage has occurred", md)

    def test_cli_execution(self):
        with tempfile.NamedTemporaryFile(suffix=".md", delete=False) as tmp:
            tmp_path = Path(tmp.name)

        try:
            cmd = [
                sys.executable,
                str(SCRIPT_DIR / "summarize_ingest.py"),
                str(self.fixture_csv),
                "--output",
                str(tmp_path),
            ]
            proc = subprocess.run(cmd, capture_output=True, text=True, check=True)
            stdout = proc.stdout
            self.assertIn("# Pi Ingest Benchmark Summary", stdout)
            self.assertIn("**150.0 rows/s**", stdout)

            # Check that file was created and matches stdout
            self.assertTrue(tmp_path.is_file())
            content = tmp_path.read_text(encoding="utf-8")
            self.assertEqual(content.strip(), stdout.strip())
        finally:
            if tmp_path.exists():
                tmp_path.unlink()

    def test_empty_rows(self):
        m = summarizer.summarize_rows([])
        self.assertIn("error", m)
        md = summarizer.generate_markdown(m)
        self.assertIn("Error", md)


if __name__ == "__main__":
    unittest.main()
