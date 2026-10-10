import copy
import tempfile
import unittest
from pathlib import Path
from compare_index_v4 import bitmap_ids, canonical_digest, compare, spelling_values
import json


class IndexParityTests(unittest.TestCase):
    def test_canonical_objects_preserve_arrays_and_values(self):
        self.assertEqual(canonical_digest({"b": 2, "a": [1, 3]}),
                         canonical_digest({"a": [1, 3], "b": 2}))
        self.assertNotEqual(canonical_digest([1, 3]), canonical_digest([3, 1]))
        self.assertNotEqual(canonical_digest({"a": 1}), canonical_digest({"a": 2}))

    def test_spelling_random_ids_are_remapped_without_hiding_changes(self):
        left = {"unique_words": ["sea", "pump"], "vocab_set": ["pump", "sea"],
                "word_lens": [3, 4], "num_words": 2, "avg_word_len": 3.5,
                "trigram_dfs": {"_se": 1},
                "trigram_postings": {"_se": {"containers": {"0": {"Array": [0]}}}}}
        right = copy.deepcopy(left)
        right.update(unique_words=["pump", "sea"], word_lens=[4, 3], vocab_set=["sea", "pump"])
        right["trigram_postings"]["_se"]["containers"]["0"]["Array"] = [1]
        self.assertEqual(spelling_values(left), spelling_values(right))
        right["word_lens"][1] += 1
        self.assertNotEqual(spelling_values(left), spelling_values(right))
        right["unique_words"] = ["sea", "sea"]
        with self.assertRaises(ValueError):
            spelling_values(right)

    def test_bitmap_high_keys_and_bit_positions(self):
        bits = [0] * 1024
        bits[0] = 1 | (1 << 63)
        bits[1023] = 1 << 63
        self.assertEqual(bitmap_ids({"containers": {"1": {"Bitmap": bits},
                                                    "0": {"Array": [4]}}}),
                         [65536, 65599, 131071, 4])
        with self.assertRaises(ValueError):
            bitmap_ids({"containers": {"0": {"Bitmap": [1]}}})

    def test_comparison_only_normalizes_db_path(self):
        with tempfile.TemporaryDirectory() as temporary:
            before, after = (Path(temporary) / name for name in ("old", "new"))
            for index in (before, after):
                index.mkdir()
                (index / "bm25.json").write_text('{"sections": []}')
                (index / "state.json").write_text(json.dumps({"db_dir": str(index), "target_dir": "docs"}))
                (index / "spelling.json").write_text(json.dumps({
                    "unique_words": [], "vocab_set": [], "word_lens": [],
                    "num_words": 0, "trigram_postings": {}, "trigram_dfs": {}, "avg_word_len": 0.0}))
            self.assertTrue(all(row["equal"] for row in compare(before, after)))
            (after / "state.json").write_text(json.dumps({"db_dir": str(after), "target_dir": "changed"}))
            self.assertFalse(compare(before, after)[1]["equal"])
            (after / "state.json").write_text('{"db_dir": "wrong"}')
            with self.assertRaises(ValueError):
                compare(before, after)


if __name__ == "__main__":
    unittest.main()
