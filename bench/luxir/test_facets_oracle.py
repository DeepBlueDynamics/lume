#!/usr/bin/env python3
import json
from pathlib import Path
import tempfile
import unittest

import facets_oracle


class FacetsOracleTests(unittest.TestCase):
    def setUp(self):
        self.docs_meta = {
            "d0": {
                "id": "d0",
                "text": "cancer immunotherapy and genetics",
                "year": 2015,
                "category": "biology",
                "tags": ["dna", "rna"],
                "n_cites": 50,
                "published_at": "2015-06-15T00:00:00Z",
            },
            "d1": {
                "id": "d1",
                "text": "cancer clinical trials therapy",
                "year": 2025,
                "category": "medicine",
                "tags": ["clinical", "dna"],
                "n_cites": 120,
                "published_at": "2025-01-10T00:00:00Z",
            },
            "d2": {
                "id": "d2",
                "text": "astronomy galaxies telescope stars",
                "year": 2022,
                "category": "physics",
                "tags": ["space"],
                "n_cites": 10,
                "published_at": "2022-09-01T00:00:00Z",
            },
            "d3": {
                "id": "d3",
                "text": "cancer epidemiology overview",
                "year": None,
                "category": None,
                "tags": [],
                "n_cites": None,
                "published_at": None,
            },
        }
        self.match_ids = ["d0", "d1", "d3"]

    def test_field_facet(self):
        res = facets_oracle.field_facet(self.match_ids, self.docs_meta, "category")
        self.assertEqual(res["type"], "field")
        self.assertEqual(res["missing"], 1)  # d3 has category None
        buckets = res["buckets"]
        self.assertEqual(len(buckets), 2)
        # biology (1), medicine (1) - sorted by val alphabetically when counts tie
        self.assertEqual(buckets[0], {"val": "biology", "count": 1})
        self.assertEqual(buckets[1], {"val": "medicine", "count": 1})

    def test_field_facet_multi_valued(self):
        res = facets_oracle.field_facet(self.match_ids, self.docs_meta, "tags")
        self.assertEqual(res["type"], "field")
        self.assertEqual(res["missing"], 1)  # d3 has empty tags []
        buckets_map = {b["val"]: b["count"] for b in res["buckets"]}
        self.assertEqual(buckets_map["dna"], 2)  # d0 and d1
        self.assertEqual(buckets_map["rna"], 1)  # d0
        self.assertEqual(buckets_map["clinical"], 1)  # d1
        self.assertNotIn("space", buckets_map)  # d2 not in match_ids

        # Multi-valued sum >= matched docs with tags
        total_bucket_sum = sum(b["count"] for b in res["buckets"])
        self.assertGreaterEqual(total_bucket_sum, len(self.match_ids) - res["missing"])

    def test_range_facet(self):
        # range [2010, 2030) with gap 10 -> [2010, 2020), [2020, 2030)
        res = facets_oracle.range_facet(self.match_ids, self.docs_meta, "year", start=2010, end=2030, gap=10)
        self.assertEqual(res["type"], "range")
        self.assertEqual(res["missing"], 1)  # d3
        self.assertEqual(res["before"], 0)
        self.assertEqual(res["after"], 0)
        self.assertEqual(len(res["buckets"]), 2)
        # d0 is 2015 -> bucket 0
        self.assertEqual(res["buckets"][0], {"from": 2010.0, "to": 2020.0, "count": 1})
        # d1 is 2025 -> bucket 1
        self.assertEqual(res["buckets"][1], {"from": 2020.0, "to": 2030.0, "count": 1})

        # Test before and after counts
        res_narrow = facets_oracle.range_facet(self.match_ids, self.docs_meta, "year", start=2020, end=2025, gap=5)
        # d0 (2015) is before, d1 (2025) is >= 2025 so after, d3 missing
        self.assertEqual(res_narrow["before"], 1)
        self.assertEqual(res_narrow["after"], 1)
        self.assertEqual(res_narrow["missing"], 1)
        self.assertEqual(res_narrow["buckets"][0]["count"], 0)

    def test_query_facet(self):
        docs_text = {docid: doc["text"] for docid, doc in self.docs_meta.items()}
        res_cancer = facets_oracle.query_facet(self.match_ids, docs_text, "cancer")
        self.assertEqual(res_cancer["type"], "query")
        self.assertEqual(res_cancer["count"], 3)  # d0, d1, d3

        res_therapy = facets_oracle.query_facet(self.match_ids, docs_text, "therapy")
        self.assertEqual(res_therapy["count"], 1)  # only d1

    def test_compute_facets_oracle_and_spec_parsing(self):
        requests = [
            "category",
            "tags",
            "year:range(2010,2030,10)",
            "therapy=therapy",
        ]
        results = facets_oracle.compute_facets_oracle(self.match_ids, self.docs_meta, requests)
        self.assertIn("category", results)
        self.assertIn("tags", results)
        self.assertIn("year", results)
        self.assertIn("therapy", results)
        self.assertEqual(results["category"]["type"], "field")
        self.assertEqual(results["year"]["type"], "range")
        self.assertEqual(results["therapy"]["type"], "query")

    def test_normalize_and_assert_facets_equal(self):
        # Lume format
        lume_output = {
            "category": {
                "type": "field",
                "buckets": [{"val": "biology", "count": 1}, {"val": "medicine", "count": 1}],
                "missing": 1,
            },
            "year": {
                "type": "range",
                "buckets": [{"from": 2010.0, "to": 2020.0, "count": 1}, {"from": 2020.0, "to": 2030.0, "count": 1}],
                "before": 0,
                "after": 0,
                "missing": 1,
            },
            "therapy": {
                "type": "query",
                "count": 1,
            },
        }
        # Luxir format
        luxir_output = {
            "category": {
                "buckets": [{"val": "biology", "count": 1}, {"val": "medicine", "count": 1}],
                "missing": 1,
            },
            "year": {
                "buckets": [{"val": [2010, 2020], "count": 1}, {"val": [2020, 2030], "count": 1}],
                "missing": 1,
            },
            "therapy": {
                "count": 1,
            },
        }

        norm_lume = facets_oracle.normalize_lume_facets(lume_output)
        norm_luxir = facets_oracle.normalize_luxir_facets(luxir_output)

        facets_oracle.assert_facets_equal(norm_lume, norm_luxir, check_range_bounds=True)


if __name__ == "__main__":
    unittest.main()
