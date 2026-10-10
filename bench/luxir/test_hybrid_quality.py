import unittest
from hybrid_quality import document_hits, HIT

class HybridOutputTests(unittest.TestCase):
    def test_hybrid_and_plain_sections_collapse_by_document(self):
        text = """[1] Hybrid Score: 9.5 (BM25: 8, Semantic: 0.3, SKG: 0) | Title (File: /docs/42.txt, Line: 1)
[2] Hybrid Score: 4.2 (BM25: 4, Semantic: 0.2, SKG: 0) | Other (File: /docs/42.txt, Line: 10)
[3] Score: 9.5 | Plain (File: C:\\docs\\7.txt, Line: 1)
[4] Hybrid Score: 1.2e-3 (BM25: 0, Semantic: 0.4, SKG: 0) | Dense (File: /docs/99.txt, Line: 2)"""
        self.assertEqual(len(HIT.findall(text)), 4)
        self.assertEqual(document_hits(text), [("42", 9.5), ("7", 9.5), ("99", .0012)])

    def test_empty_and_bad_output(self):
        self.assertEqual(document_hits("No hits found."), [])
        with self.assertRaises(ValueError):
            document_hits("unrecognized response")
        with self.assertRaises(ValueError):
            document_hits("[1] Hybrid Score: 1 | Title (File: /docs/a.pdf, Line: 1)")

if __name__ == "__main__":
    unittest.main()
