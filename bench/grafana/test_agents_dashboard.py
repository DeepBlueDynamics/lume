"""Unit tests for lume-agents-dashboard.json verifying structure, datasources, SQL, and secrets."""
import json
from pathlib import Path
import re
import unittest

DASHBOARD_PATH = Path(__file__).resolve().parent / "lume-agents-dashboard.json"


class TestAgentsDashboard(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.assertTrue(DASHBOARD_PATH.exists(), f"Dashboard JSON missing at {DASHBOARD_PATH}")
        with open(DASHBOARD_PATH, "r", encoding="utf-8") as f:
            cls.raw_content = f.read()
            cls.dashboard = json.loads(cls.raw_content)

    def test_json_parses(self):
        """The dashboard JSON parses and contains required top-level Grafana fields."""
        self.assertIsInstance(self.dashboard, dict)
        self.assertIn("title", self.dashboard)
        self.assertIn("uid", self.dashboard)
        self.assertIn("panels", self.dashboard)
        self.assertIsInstance(self.dashboard["panels"], list)
        self.assertGreaterEqual(len(self.dashboard["panels"]), 4)

    def test_every_panel_targets_lume_datasource_uid(self):
        """Every panel and target points to the lume datasource uid ('lume-ti')."""
        panels = self.dashboard.get("panels", [])
        self.assertGreaterEqual(len(panels), 4, "Dashboard must have at least 4 panels")
        for panel in panels:
            title = panel.get("title", f"id={panel.get('id')}")
            # Panel-level datasource
            ds = panel.get("datasource")
            self.assertIsNotNone(ds, f"Panel '{title}' missing datasource")
            if isinstance(ds, dict):
                self.assertEqual(ds.get("uid"), "lume-ti", f"Panel '{title}' datasource uid must be 'lume-ti'")
            else:
                self.assertEqual(ds, "lume-ti", f"Panel '{title}' datasource must be 'lume-ti'")

            # Target-level datasources
            targets = panel.get("targets", [])
            self.assertGreater(len(targets), 0, f"Panel '{title}' must have at least one query target")
            for target in targets:
                tds = target.get("datasource")
                self.assertIsNotNone(tds, f"Target in '{title}' missing datasource")
                if isinstance(tds, dict):
                    self.assertEqual(tds.get("uid"), "lume-ti", f"Target in '{title}' datasource uid must be 'lume-ti'")
                else:
                    self.assertEqual(tds, "lume-ti", f"Target in '{title}' datasource must be 'lume-ti'")

    def test_every_raw_sql_references_telemetry_agents_or_docs(self):
        """Every panel's rawSql queries either telemetry_agents or docs table."""
        panels = self.dashboard.get("panels", [])
        for panel in panels:
            title = panel.get("title", f"id={panel.get('id')}")
            targets = panel.get("targets", [])
            for target in targets:
                raw_sql = target.get("rawSql", "")
                self.assertTrue(bool(raw_sql.strip()), f"Panel '{title}' target has empty rawSql")
                references_table = bool(re.search(r"\b(telemetry_agents|docs)\b", raw_sql))
                self.assertTrue(
                    references_table,
                    f"Panel '{title}' rawSql must reference 'telemetry_agents' or 'docs'. Got: {raw_sql}"
                )

        # Specifically check panel queries match D50 requirements
        all_sql = "\n".join(
            target.get("rawSql", "")
            for p in panels
            for target in p.get("targets", [])
        )
        self.assertIn("claude_code.token.usage", all_sql)
        self.assertIn("@last", all_sql)
        self.assertIn("claude_code.cost.usage", all_sql)
        self.assertIn("sessions", all_sql.lower())
        self.assertIn("match(body, '$q')", all_sql)
        self.assertIn("ts_start", all_sql)

    def test_no_hardcoded_secrets(self):
        """Dashboard JSON must contain no embedded credentials, passwords, or secret tokens."""
        lower = self.raw_content.lower()
        self.assertNotIn("bearer ", lower)
        self.assertNotIn("hyp_agent", lower)
        self.assertNotIn("scram-sha-256", lower)
        self.assertNotIn('"password":', self.raw_content)
        self.assertNotIn("lume_pg_password", lower)


if __name__ == "__main__":
    unittest.main()
