import json
from pathlib import Path
import tempfile
import unittest
import run_lume


class RunTests(unittest.TestCase):
    def test_max_section_score_and_deterministic_document_ties(self):
        text = """[1] Score: 3.0000 | Lines 1-25 (File: /data/b.txt, Line: 1)
[2] Score: 2.0000 [SKG: 0.12] | Lines 26-50 (File: /data/b.txt, Line: 26)
[3] Score: 3.0000 | Lines 1-25 (File: /data/a.txt, Line: 1)
"""
        self.assertEqual(run_lume.document_hits(text), [("a", 3.0), ("b", 3.0)])

    def test_empty_and_invalid_output(self):
        self.assertEqual(run_lume.document_hits("No hits found."), [])
        with self.assertRaises(ValueError):
            run_lume.document_hits("tool failed")

    def test_files_with_meta_writes_manifest(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            root = Path(tmpdir)
            dataset = "testds"
            ds_dir = root / dataset
            ds_dir.mkdir(parents=True)

            docs = [
                {"id": "doc1", "text": "Content of doc 1"},
                {"id": "doc2", "text": "Content of doc 2"},
            ]
            (ds_dir / "docs.jsonl").write_text("".join(json.dumps(d) + "\n" for d in docs), encoding="utf-8")

            docs_meta = [
                {"id": "doc1", "text": "Content of doc 1", "year": 2020, "category": "biology", "tags": ["t1"]},
                {"id": "doc2", "text": "Content of doc 2", "year": 2022, "category": "physics", "tags": ["t2"]},
            ]
            (ds_dir / "docs_meta.jsonl").write_text("".join(json.dumps(d) + "\n" for d in docs_meta), encoding="utf-8")

            run_lume.files(root, dataset, with_meta=True)

            files_dir = ds_dir / "files"
            self.assertTrue((files_dir / "doc1.txt").exists())
            self.assertEqual((files_dir / "doc1.txt").read_text(encoding="utf-8"), "Content of doc 1")

            meta_file = files_dir / "lume.meta.jsonl"
            self.assertTrue(meta_file.exists())
            manifest_lines = [json.loads(line) for line in meta_file.read_text(encoding="utf-8").splitlines()]
            self.assertEqual(len(manifest_lines), 2)
            self.assertEqual(manifest_lines[0], {
                "path": "doc1.txt",
                "fields": {"year": 2020, "category": "biology", "tags": ["t1"]}
            })
            self.assertEqual(manifest_lines[1], {
                "path": "doc2.txt",
                "fields": {"year": 2022, "category": "physics", "tags": ["t2"]}
            })

    def test_files_with_meta_missing_source_fails(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            root = Path(tmpdir)
            dataset = "testds"
            ds_dir = root / dataset
            ds_dir.mkdir(parents=True)
            (ds_dir / "docs.jsonl").write_text(json.dumps({"id": "d1", "text": "t"}) + "\n", encoding="utf-8")
            with self.assertRaises(FileNotFoundError):
                run_lume.files(root, dataset, with_meta=True)


if __name__ == "__main__":
    unittest.main()
