#!/usr/bin/env python3
"""Unit tests for check_redaction.py's broadened detector.

Run with:

    python3 -m unittest scripts/test_check_redaction.py
"""

from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import check_redaction as cr  # noqa: E402


class OffendingLinesTests(unittest.TestCase):
    def test_flags_an_id_shape_with_no_header_prefix_at_all(self):
        # This is the whole point of this script: redaction.rs only looks
        # right after the literal "Account Statement for " text, so an id
        # repeated in, say, a footer or a differently-worded label would
        # slip past it. This detector is not anchored to that phrase.
        text = "some footer line mentions Account #12345678SCHW again"
        self.assertEqual(cr.offending_lines(text), [1])

    def test_accepts_the_placeholder_account(self):
        text = f"Account Statement for {cr.REPLACEMENT_ACCOUNT}SCHW (Individual)"
        self.assertEqual(cr.offending_lines(text), [])

    def test_ignores_digit_runs_not_shaped_like_an_account_id(self):
        # An OSI code / ref number's digits, on their own (no adjacent
        # uppercase-letter run), must not false-positive.
        text = 'ref="1007650828803" SPXW   270219P07325000'
        self.assertEqual(cr.offending_lines(text), [])

    def test_reports_the_right_line_number_in_a_multiline_file(self):
        text = "line one is clean\n12345678SCHW leaks here\nline three is clean"
        self.assertEqual(cr.offending_lines(text), [2])

    def test_redaction_rs_own_synthetic_fixture_is_excluded_by_path(self):
        # redaction.rs deliberately builds an "12345678"+"SCHW"-shaped id at
        # runtime to test its own (narrower) detector; main() must not flag
        # that file even though the shape-only pattern matches its source.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            excluded = root / "crates" / "wingspan-core" / "tests" / "redaction.rs"
            excluded.parent.mkdir(parents=True)
            excluded.write_text('let real = format!("...{}12345678SCHW...");')
            other = root / "docs" / "leak.md"
            other.parent.mkdir(parents=True)
            other.write_text("Account 87654321SCHW turned up here by mistake")

            offenders = []
            for path in cr.text_files(root):
                relative = path.relative_to(root)
                if str(relative) in cr.EXCLUDED_FILES:
                    continue
                for line in cr.offending_lines(path.read_text()):
                    offenders.append(f"{relative}:{line}")

            self.assertEqual(offenders, [str(Path("docs") / "leak.md") + ":1"])


if __name__ == "__main__":
    unittest.main()
