import datetime
import json
from pathlib import Path
import tempfile
import unittest
import prepare


class MetadataTests(unittest.TestCase):
    def test_deterministic_metadata_and_valid_ranges(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            dataset = root / "scifact"
            dataset.mkdir()
            original = "".join(json.dumps({"id": str(i), "text": "paper"}) + "\n" for i in range(50))
            (dataset / "docs.jsonl").write_text(original)
            info = prepare.metadata("scifact", root)
            first = (dataset / "docs_meta.jsonl").read_bytes()
            prepare.metadata("scifact", root)
            self.assertEqual(first, (dataset / "docs_meta.jsonl").read_bytes())
            self.assertEqual((dataset / "docs.jsonl").read_text(), original)
            self.assertEqual(info["seed"], 42)
            self.assertTrue(info["synthetic"])
            for row in map(json.loads, first.splitlines()):
                self.assertTrue(1990 <= row["year"] <= 2024)
                self.assertIn(row["category"], info["categories"])
                self.assertTrue(0 <= row["n_cites"] <= 1000)
                self.assertTrue(1 <= len(row["tags"]) <= 3)
                self.assertEqual(len(row["tags"]), len(set(row["tags"])))
                self.assertTrue(set(row["tags"]) <= set(info["tags"]))
                stamp = datetime.datetime.fromisoformat(row["published_at"])
                self.assertEqual(stamp.year, row["year"])
                self.assertEqual(stamp.utcoffset(), datetime.timedelta(0))


if __name__ == "__main__":
    unittest.main()
