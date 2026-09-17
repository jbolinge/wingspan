"""Tests for scripts/compare_contract.py."""

import json
import tempfile
import unittest
from pathlib import Path

from scripts.compare_contract import compare_dirs, compare_values


def diffs(expected, actual, tolerance=1e-5):
    out = []
    compare_values(expected, actual, "", tolerance, out)
    return out


class CompareValues(unittest.TestCase):
    def test_floats_inside_the_tolerance_are_equal(self):
        self.assertEqual(diffs({"g": [-0.055025, 1.0]}, {"g": [-0.055027, 1.0]}), [])

    def test_floats_outside_the_tolerance_are_reported_with_their_path(self):
        out = diffs({"t0_greeks": {"gamma": [0.1, 0.2]}}, {"t0_greeks": {"gamma": [0.1, 0.21]}})
        self.assertEqual(len(out), 1)
        self.assertIn("t0_greeks.gamma[1]", out[0])

    def test_integers_strings_booleans_and_nulls_are_exact(self):
        self.assertTrue(diffs({"n": 3}, {"n": 4}))
        self.assertTrue(diffs({"s": "open"}, {"s": "closed"}))
        self.assertTrue(diffs({"b": True}, {"b": False}))
        self.assertTrue(diffs({"x": None}, {"x": 0}))
        self.assertTrue(diffs({"b": True}, {"b": 1}), "a bool is not a number")
        self.assertEqual(diffs({"n": 3, "s": "a", "b": False, "x": None}, {"n": 3, "s": "a", "b": False, "x": None}), [])

    def test_an_integer_against_a_float_uses_the_tolerance(self):
        self.assertEqual(diffs({"v": 5}, {"v": 5.0}), [])
        self.assertTrue(diffs({"v": 5}, {"v": 5.5}))

    def test_missing_extra_keys_and_length_changes_are_reported(self):
        out = diffs({"a": 1, "b": [1, 2]}, {"b": [1], "c": 2})
        self.assertEqual(len(out), 3, out)
        self.assertTrue(any("a: missing" in line for line in out))
        self.assertTrue(any("c: not in" in line for line in out))
        self.assertTrue(any("b: expected 2 items" in line for line in out))

    def test_a_type_change_is_a_difference(self):
        self.assertTrue(diffs({"v": [1]}, {"v": {"0": 1}}))


class CompareDirs(unittest.TestCase):
    def write(self, root: Path, name: str, value) -> None:
        (root / name).write_text(json.dumps(value), encoding="utf-8")

    def test_added_and_removed_files_are_reported_like_diff_r(self):
        with tempfile.TemporaryDirectory() as e, tempfile.TemporaryDirectory() as a:
            self.write(Path(e), "gone.json", {})
            self.write(Path(a), "new.json", {})
            out = compare_dirs(Path(e), Path(a), 1e-5)
            self.assertEqual(out, ["gone.json: only in the checked-in contract", "new.json: only in the dump"])

    def test_matching_directories_have_no_differences(self):
        with tempfile.TemporaryDirectory() as e, tempfile.TemporaryDirectory() as a:
            for root in (Path(e), Path(a)):
                self.write(root, "health.json", {"status": "ok", "ready": True})
            self.assertEqual(compare_dirs(Path(e), Path(a), 1e-5), [])

    def test_a_difference_names_the_file(self):
        with tempfile.TemporaryDirectory() as e, tempfile.TemporaryDirectory() as a:
            self.write(Path(e), "risk_graph.json", {"pop": 0.61})
            self.write(Path(a), "risk_graph.json", {"pop": 0.62})
            out = compare_dirs(Path(e), Path(a), 1e-5)
            self.assertEqual(len(out), 1)
            self.assertTrue(out[0].startswith("risk_graph.json: pop"), out[0])


if __name__ == "__main__":
    unittest.main()
