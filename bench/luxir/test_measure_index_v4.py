import unittest
from measure_index_v4 import records


class TimingRecordTests(unittest.TestCase):
    def test_only_diagnostic_lines_are_parsed(self):
        self.assertEqual(records('notice\nLUME_TIMING {"phase":"open.read","ms":1.5}\n'),
                         [{"phase": "open.read", "ms": 1.5}])
        self.assertEqual(records("normal error output"), [])


if __name__ == "__main__":
    unittest.main()
