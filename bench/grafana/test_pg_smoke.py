"""Verify the shell harness with a local stub; SQL execution is covered by ti_http."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class PgSmokeTests(unittest.TestCase):
    def test_twenty_sql_cases_psql_describe_and_negative_scram(self):
        with tempfile.TemporaryDirectory() as directory:
            tmp = Path(directory)
            executable = tmp / "psql"
            executable.write_text("""#!/usr/bin/env python3
import json, os, sys
with open(os.environ["SMOKE_LOG"], "a", encoding="utf-8") as stream:
    stream.write(json.dumps(sys.argv[1:]) + "\\n")
if os.environ.get("PGPASSWORD") == "lume-deliberately-invalid-smoke-password":
    print("SCRAM authentication failed", file=sys.stderr)
    sys.exit(1)
""", encoding="utf-8")
            executable.chmod(0o700)
            log = tmp / "calls.jsonl"
            env = dict(os.environ, PATH=str(tmp) + os.pathsep + os.environ["PATH"],
                       SMOKE_LOG=str(log), PGPASSWORD="SMOKE_SECRET")
            run = subprocess.run(["bash", str(ROOT / "tests/pg_smoke.sh"),
                                  "127.0.0.2:5864", "grafana", "ti"],
                                 env=env, capture_output=True, text=True, timeout=30)
            self.assertEqual(run.returncode, 0, run.stdout + run.stderr)
            self.assertNotIn("SMOKE_SECRET", run.stdout + run.stderr)
            calls = [json.loads(line) for line in log.read_text().splitlines()]
            self.assertEqual(len(calls), 22)  # \d + 20 SQL + auth rejection.
            self.assertEqual(calls[0][-1], r"\d telemetry")
            self.assertEqual(run.stdout.count("Smoke "), 20)
            sql = "\n".join(call[-1] for call in calls)
            self.assertIn("floor(extract(epoch from ts)/60)*60", sql)
            self.assertIn("BETWEEN (now() - INTERVAL '24 hours') AND now()", sql)
            self.assertNotIn("$__timeFilter", sql)
            self.assertIn("PASS: 20 SQL cases", run.stdout)


if __name__ == "__main__":
    unittest.main()
