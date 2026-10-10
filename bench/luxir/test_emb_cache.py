#!/usr/bin/env python3
import json
from pathlib import Path
import tempfile
import unittest

from emb_cache import load_embedding_cache, parse_cache_name


def _write(path, rows):
    path.parent.mkdir(parents=True, exist_ok=True)
    lines = [json.dumps({"id": doc_id, "vector": vector}) for doc_id, vector in rows]
    path.write_text("\n".join(lines) + ("\n" if lines else ""), encoding="utf-8")


class EmbCacheTests(unittest.TestCase):
    def test_parse_model_dims_dataset_and_kind(self):
        self.assertEqual(
            parse_cache_name("embeddinggemma-2-768-scifact-docs.jsonl"),
            ("embeddinggemma-2", 768, "scifact", "docs")
        )
        self.assertEqual(
            parse_cache_name(Path("/data/embeddings/gtr-t5-base-768-trec-covid-queries.jsonl")),
            ("gtr-t5-base", 768, "trec-covid", "queries")
        )

    def test_parse_rejects_bad_filenames(self):
        for name in (
            "embeddinggemma-2-768-scifact-docs.npy",
            "embeddinggemma-2-768-scifact.jsonl",
            "other-768-scifact-docs.jsonl",
            "embeddinggemma-2-wide-scifact-docs.jsonl",
            "embeddinggemma-2-768-docs.jsonl",
        ):
            with self.assertRaises(ValueError):
                parse_cache_name(name)

    def test_load_returns_id_vector_map(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "embeddinggemma-2-2-scifact-docs.jsonl"
            _write(path, [("a", [0.1, 0.2]), ("b", [0.3, 0.4])])
            loaded = load_embedding_cache(path, ["b", "a"], model="embeddinggemma-2", dims=2)
            self.assertEqual(loaded, {"a": [0.1, 0.2], "b": [0.3, 0.4]})

    def test_partial_cache_names_missing_ids(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "embeddinggemma-2-2-scifact-queries.jsonl"
            _write(path, [("q1", [1.0, 0.0])])
            with self.assertRaises(ValueError) as caught:
                load_embedding_cache(path, ["q1", "q2", "q3"])
            message = str(caught.exception)
            self.assertIn("partial embedding cache", message)
            self.assertIn("embeddinggemma-2", message)
            self.assertIn("missing 2 of 3 ids", message)
            self.assertIn("q2", message)
            self.assertIn("q3", message)

    def test_vector_length_must_match_filename_dims(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "gtr-t5-base-768-scifact-docs.jsonl"
            _write(path, [("d1", [1.0, 2.0])])
            with self.assertRaises(ValueError) as caught:
                load_embedding_cache(path, ["d1"])
            self.assertIn("!= dims 768", str(caught.exception))
            self.assertIn("d1", str(caught.exception))

    def test_model_and_dims_arguments_must_match_filename(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "gtr-t5-base-2-scifact-docs.jsonl"
            _write(path, [("d1", [1.0, 2.0])])
            with self.assertRaises(ValueError) as caught:
                load_embedding_cache(path, ["d1"], model="embeddinggemma-2")
            self.assertIn("model is gtr-t5-base", str(caught.exception))
            self.assertIn("expected embeddinggemma-2", str(caught.exception))
            with self.assertRaises(ValueError) as caught:
                load_embedding_cache(path, ["d1"], dims=768)
            self.assertIn("dims are 2", str(caught.exception))
            self.assertIn("expected 768", str(caught.exception))

    def test_duplicate_id_and_bad_line(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "embeddinggemma-2-1-scifact-docs.jsonl"
            path.write_text(
                json.dumps({"id": "a", "vector": [1.0]}) + "\n"
                + json.dumps({"id": "a", "vector": [2.0]}) + "\n",
                encoding="utf-8"
            )
            with self.assertRaises(ValueError) as caught:
                load_embedding_cache(path, ["a"])
            self.assertIn("duplicate id a", str(caught.exception))

            path.write_text("{not json}\n", encoding="utf-8")
            with self.assertRaises(ValueError) as caught:
                load_embedding_cache(path, ["a"])
            self.assertIn("line 1", str(caught.exception))

    def test_missing_file(self):
        missing = Path("/workspace/lume/.lanes/w5/.test-tmp-absent/embeddinggemma-2-768-scifact-docs.jsonl")
        with self.assertRaises(ValueError) as caught:
            load_embedding_cache(missing, ["a"])
        self.assertIn("not found", str(caught.exception))


if __name__ == "__main__":
    unittest.main()
