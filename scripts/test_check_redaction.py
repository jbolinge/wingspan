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
from unittest import mock

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
        # An OSI code / synthetic ref number's digits, on their own (no
        # adjacent uppercase-letter run), must not false-positive.
        text = 'ref="9000000000032" SPXW   270219P07325000'
        self.assertEqual(cr.offending_lines(text), [])

    def test_flags_a_real_shaped_execution_ref_anywhere(self):
        # 13 digits starting "100" is what every real broker ref looks like;
        # the anonymiser renumbers them to a "9"-prefixed sequence, so one
        # turning up in any context (a doc, a test, a snapshot) is a leak.
        for text in (
            'TRD,="1007650828803",BOT +2 BUTTERFLY',
            ",,,RE #1007359839690,SELL,-1",
            "ref 1007170257443 in prose",
        ):
            self.assertEqual(cr.offending_lines(text), [1], text)

    def test_real_shaped_digits_inside_a_longer_number_are_not_flagged(self):
        for text in (
            "21007650828803",      # 14 digits
            "10076508288031",      # 14 digits
            "1007650828803.5",     # a decimal fraction
            "0.1007650828803",
        ):
            self.assertEqual(cr.offending_lines(text), [], text)

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


class SheetIdShapeTests(unittest.TestCase):
    # Letters only, 44 characters - long enough to look like a real Google
    # file id, and chosen so it does not accidentally also match the
    # account-id shape (no run of 8 digits) or the exec-ref shape (no
    # digits at all).
    REAL_LOOKING_ID = "ZzYyXxWwVvUuTtSsRrQqPpOoNnMmLlKkJjIiHhGgFfEe"

    def test_flags_a_real_looking_sheet_id_in_a_share_link(self):
        self.assertNotEqual(self.REAL_LOOKING_ID, cr.PLACEHOLDER_SHEET_ID)
        self.assertIsNone(cr.ACCOUNT_ID_SHAPE_RE.search(self.REAL_LOOKING_ID))
        self.assertIsNone(cr.EXEC_REF_SHAPE_RE.search(self.REAL_LOOKING_ID))
        text = (
            f"sheet_url: https://docs.google.com/spreadsheets/d/"
            f"{self.REAL_LOOKING_ID}/edit#gid=0"
        )
        self.assertEqual(cr.offending_lines(text), [1])

    def test_accepts_the_placeholder_sheet_id(self):
        text = (
            "https://docs.google.com/spreadsheets/d/"
            f"{cr.PLACEHOLDER_SHEET_ID}/edit#gid=123456789"
        )
        self.assertEqual(cr.offending_lines(text), [])

    def test_flags_a_published_sheet_id_too(self):
        text = (
            "https://docs.google.com/spreadsheets/d/e/"
            f"{self.REAL_LOOKING_ID}/pub?output=csv"
        )
        self.assertEqual(cr.offending_lines(text), [1])

    def test_flags_a_bare_id_with_no_url_around_it(self):
        # How the id is actually stored: `SourceRef::Sheet` serialises the
        # naked id into `<state_dir>/trade_source.json`, which the WDIO
        # redaction sweep runs this script over. A rule anchored on
        # `spreadsheets/d/` would pass it straight through.
        bare = "9QwErTyUiOpAsDfGhJkLzXcVbNm1234567890Qw-Er"
        self.assertEqual(len(bare), 42)
        text = f'{{"kind":"sheet","id":"{bare}","tab":"Legs"}}'
        self.assertEqual(cr.offending_lines(text), [1])
        # And in a config file, unquoted.
        self.assertEqual(cr.offending_lines(f"sheet_url = {bare}"), [1])

    def test_the_bare_placeholder_is_accepted(self):
        text = f'{{"kind":"sheet","id":"{cr.PLACEHOLDER_SHEET_ID}"}}'
        self.assertEqual(cr.offending_lines(text), [])

    def test_a_hex_digest_is_not_a_bare_id(self):
        # A sha256 is 64 lower-case hex characters: long enough for the shape,
        # but with no upper case, so it is not a Drive id. The cache sidecar
        # writes one of these beside every fetch.
        digest = "a" * 32 + "0123456789abcdef" * 2
        self.assertEqual(len(digest), 64)
        self.assertEqual(cr.offending_lines(f'"sha256": "{digest}"'), [])

    def test_lock_files_are_skipped_for_the_bare_id_rule_only(self):
        # npm integrity hashes are base64 and do mix case and digits; a lock
        # file also cannot carry a user's data. Passing no name applies every
        # rule, which is what makes the skip visible here.
        token = "8dyGyWMowPoV1tJ52muDGsDqt2HPT2klrFUiY60MQmQv9q"
        self.assertEqual(cr.offending_lines(f'"integrity": "sha512-{token}"'), [1])
        self.assertEqual(
            cr.offending_lines(f'"integrity": "sha512-{token}"', "web/package-lock.json"),
            [],
        )
        # The URL-shaped rule still applies inside a lock file.
        leak = f"https://docs.google.com/spreadsheets/d/{self.REAL_LOOKING_ID}/edit"
        self.assertEqual(cr.offending_lines(leak, "web/package-lock.json"), [1])

    def test_a_short_id_below_the_floor_is_not_flagged(self):
        # Below the 20-char floor `sheet_url::parse` itself enforces - too
        # short to be a real spreadsheet id, deliberately not flagged.
        text = "https://docs.google.com/spreadsheets/d/abc123/edit"
        self.assertEqual(cr.offending_lines(text), [])


class RootOptionTests(unittest.TestCase):
    # `--root` is what lets CI point this script at an unpacked .deb instead
    # of the repository checkout; `main()` itself, not just `offending_lines`
    # or `text_files`, has to honour it.
    def test_root_scans_the_given_directory_and_flags_a_leak(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "usr" / "lib" / "wingspan").mkdir(parents=True)
            leaky = root / "usr" / "lib" / "wingspan" / "notes.txt"
            leaky.write_text("Account 87654321SCHW turned up here by mistake")

            with mock.patch.object(sys, "argv", ["check_redaction.py", "--root", str(root)]):
                self.assertEqual(cr.main(), 1)

    def test_root_on_a_clean_directory_exits_zero(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "usr" / "lib" / "wingspan").mkdir(parents=True)
            clean = root / "usr" / "lib" / "wingspan" / "notes.txt"
            clean.write_text(f"Account Statement for {cr.REPLACEMENT_ACCOUNT}SCHW (Individual)")

            with mock.patch.object(sys, "argv", ["check_redaction.py", "--root", str(root)]):
                self.assertEqual(cr.main(), 0)


if __name__ == "__main__":
    unittest.main()
