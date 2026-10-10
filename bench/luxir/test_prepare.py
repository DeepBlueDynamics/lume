import json
from pathlib import Path
import tempfile
import unittest
import zipfile
import prepare


class PrepareTests(unittest.TestCase):
    def test_test_split_and_title_normalization(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with zipfile.ZipFile(root / "scifact.zip", "w") as archive:
                archive.writestr("scifact/corpus.jsonl", json.dumps({"_id": "d1", "title": "Title", "text": "Body"}) + "\n")
                archive.writestr("scifact/queries.jsonl", "\n".join(json.dumps(r) for r in [
                    {"_id": "q1", "text": "a\tquery\ntext"}, {"_id": "train", "text": "not test"}]) + "\n")
                archive.writestr("scifact/qrels/test.tsv", "query-id\tcorpus-id\tscore\nq1\td1\t1\n")
            report = prepare.prepare("scifact", root)
            self.assertEqual(report["documents"], 1)
            self.assertEqual(report["test_queries"], 1)
            self.assertEqual(json.loads((root / "scifact/docs.jsonl").read_text()), {"id": "d1", "text": "Title\n\nBody"})
            self.assertEqual((root / "scifact/queries.tsv").read_text(), "q1\ta query text\n")
            self.assertEqual((root / "scifact/qrels.tsv").read_text(), "q1\td1\t1\n")


if __name__ == "__main__":
    unittest.main()
