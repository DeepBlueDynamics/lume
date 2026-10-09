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


if __name__ == "__main__":
    unittest.main()
