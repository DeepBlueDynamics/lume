"""Integration test executing Grafana agents dashboard queries against a live Lume OTLP instance."""
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import unittest
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
DASHBOARD_PATH = Path(__file__).resolve().parent / "lume-agents-dashboard.json"
METRICS_FIXTURE = ROOT / "tests/golden/otlp/metrics.json"
LOGS_FIXTURE = ROOT / "tests/golden/otlp/logs.json"


def find_lume_binary():
    """Locate a compiled lume executable, checking cargo target paths and PATH."""
    candidates = [
        os.environ.get("CARGO_BIN_EXE_lume"),
        str(ROOT / ".lanes/w3/target/debug/lume"),
        str(ROOT / "target/debug/lume"),
        shutil.which("lume"),
    ]
    for c in candidates:
        if c and os.path.isfile(c) and os.access(c, os.X_OK):
            return c
    return None


def expand_grafana_macros(sql: str) -> str:
    """Expand Grafana PostgreSQL macros to standard SQL executable by Lume."""
    def replace_time_group(m):
        col, interval = m.group(1), m.group(2)
        unit = interval[-1]
        val = int(interval[:-1])
        multiplier = {"s": 1, "m": 60, "h": 3600, "d": 86400}[unit]
        secs = val * multiplier
        return f'floor(extract(epoch from {col})/{secs})*{secs} AS "time"'

    # $__timeGroupAlias(column, interval) -> floor(extract(epoch from col)/sec)*sec AS "time"
    expanded = re.sub(
        r"\$__timeGroupAlias\(\s*(\w+)\s*,\s*(\d+[smhd])\s*\)",
        replace_time_group,
        sql,
    )
    # $__timeFilter(column) -> column BETWEEN '2020-01-01 00:00:00' AND '2020-01-01 01:00:00'
    expanded = re.sub(
        r"\$__timeFilter\(\s*(\w+)\s*\)",
        r"\1 BETWEEN '2020-01-01 00:00:00' AND '2020-01-01 01:00:00'",
        expanded,
    )
    # ${q:sqlstring} -> 'service.rs'
    expanded = re.sub(r"\$\{q:sqlstring\}", "'service.rs'", expanded)
    expanded = re.sub(r"\$\{q\}|\$q\b", "'service.rs'", expanded)
    return expanded


class TestAgentsDashboardSql(unittest.TestCase):
    proc = None
    tmp_dir = None
    server_url = None
    lume_bin = None

    @classmethod
    def setUpClass(cls):
        cls.lume_bin = find_lume_binary()
        if not cls.lume_bin:
            raise unittest.SkipTest("No built lume binary found; skipping live SQL execution")

        if not METRICS_FIXTURE.exists() or not LOGS_FIXTURE.exists():
            raise unittest.SkipTest("Golden OTLP fixtures missing")

        cls.tmp_dir = tempfile.mkdtemp(prefix="otlp-dash-test-")
        command = [cls.lume_bin, "ti", "otlp", "--store", cls.tmp_dir, "--port", "0"]
        cls.proc = subprocess.Popen(
            command,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )

        # Read first line: "Lume MCP HTTP server listening on http://127.0.0.1:<port>"
        line = cls.proc.stdout.readline()
        if not line:
            stderr = cls.proc.stderr.read()
            cls.tearDownClass()
            raise RuntimeError(f"lume ti otlp failed to start: {stderr}")

        cls.server_url = line.strip().split()[-1]
        assert cls.server_url.startswith("http://127.0.0.1:"), f"Unexpected url: {cls.server_url}"

        # Ingest metrics fixture
        with open(METRICS_FIXTURE, "rb") as f:
            req = urllib.request.Request(
                f"{cls.server_url}/v1/metrics",
                data=f.read(),
                headers={"Content-Type": "application/json"},
            )
            with urllib.request.urlopen(req, timeout=10) as resp:
                assert resp.status == 200, f"Metrics ingest failed with {resp.status}"

        # Ingest logs fixture
        with open(LOGS_FIXTURE, "rb") as f:
            req = urllib.request.Request(
                f"{cls.server_url}/v1/logs",
                data=f.read(),
                headers={"Content-Type": "application/json"},
            )
            with urllib.request.urlopen(req, timeout=10) as resp:
                assert resp.status == 200, f"Logs ingest failed with {resp.status}"

    @classmethod
    def tearDownClass(cls):
        if cls.proc:
            if cls.proc.stdout:
                cls.proc.stdout.close()
            if cls.proc.stderr:
                cls.proc.stderr.close()
            cls.proc.kill()
            cls.proc.wait()
            cls.proc = None
        if cls.tmp_dir and os.path.exists(cls.tmp_dir):
            shutil.rmtree(cls.tmp_dir, ignore_errors=True)

    def test_every_panel_raw_sql_executes_without_error(self):
        """Every panel's rawSql executes against /ti/query without error after macro expansion."""
        with open(DASHBOARD_PATH, "r", encoding="utf-8") as f:
            dashboard = json.load(f)

        panels = dashboard.get("panels", [])
        self.assertGreaterEqual(len(panels), 4, "Dashboard must have at least 4 panels")

        tokens_row_count = 0
        docs_row_count = 0

        for panel in panels:
            title = panel.get("title", f"id={panel.get('id')}")
            for target in panel.get("targets", []):
                raw_sql = target.get("rawSql", "")
                self.assertTrue(bool(raw_sql.strip()), f"Panel '{title}' target has empty rawSql")

                expanded_sql = expand_grafana_macros(raw_sql)
                req = urllib.request.Request(
                    f"{self.server_url}/ti/query",
                    data=json.dumps({"sql": expanded_sql}).encode("utf-8"),
                    headers={
                        "Content-Type": "application/json",
                        "Accept": "application/json",
                    },
                )
                try:
                    with urllib.request.urlopen(req, timeout=10) as resp:
                        self.assertEqual(resp.status, 200, f"Panel '{title}' query failed: HTTP {resp.status}")
                        data = json.loads(resp.read().decode("utf-8"))
                except urllib.error.HTTPError as e:
                    self.fail(
                        f"Panel '{title}' query returned HTTP {e.code}: {e.read().decode('utf-8')}\n"
                        f"Expanded SQL: {expanded_sql}"
                    )

                rows = data.get("rows", [])
                if "token" in title.lower():
                    tokens_row_count += len(rows)
                if "doc" in title.lower() or "log" in title.lower():
                    docs_row_count += len(rows)

        self.assertGreaterEqual(tokens_row_count, 1, "Tokens panel must return at least one row")
        self.assertGreaterEqual(docs_row_count, 1, "Docs panel must return at least one row")

    def test_lume_ti_query_cli_on_ingested_store(self):
        """lume ti query CLI executes the expanded docs query cleanly against the store."""
        sql = expand_grafana_macros(
            "SELECT title, vessel AS entity, ts_start FROM docs WHERE match(body, ${q:sqlstring}) ORDER BY ts_start DESC LIMIT 100"
        )
        res = subprocess.run(
            [self.lume_bin, "ti", "query", sql, "--store", self.tmp_dir, "--json"],
            capture_output=True,
            text=True,
            timeout=10,
        )
        self.assertEqual(res.returncode, 0, f"lume ti query CLI failed: {res.stderr}")
        data = json.loads(res.stdout)
        self.assertGreaterEqual(len(data.get("rows", [])), 1)


if __name__ == "__main__":
    unittest.main()
