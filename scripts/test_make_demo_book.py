#!/usr/bin/env python3
"""Guards for make_demo_book.py: the committed demo book must be exactly what
the script writes, and it must be recognisably synthetic.

Run with:

    python3 -m unittest scripts/test_make_demo_book.py
"""

from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import check_redaction as cr  # noqa: E402
import make_demo_book as demo  # noqa: E402


def snapshot(root: Path) -> dict[str, bytes]:
    return {str(p.relative_to(root)): p.read_bytes() for p in sorted(root.rglob("*")) if p.is_file()}


class DemoBookTests(unittest.TestCase):
    def test_committed_fixtures_match_a_fresh_build(self):
        # The point of the generator is that nobody hand-edits the demo book;
        # regenerate after changing the script and commit the result.
        with tempfile.TemporaryDirectory() as tmp:
            dest = Path(tmp) / "demo"
            demo.build(dest)
            self.assertEqual(snapshot(demo.DEFAULT_DEST), snapshot(dest), "fixtures/demo is stale: run `python3 scripts/make_demo_book.py`")

    def test_a_folder_that_is_not_a_demo_book_is_refused_with_its_files_intact(self):
        # `--dest` is a path a human typed. Pointed at a real trades folder
        # (no README of ours, an `Open Trades/` full of real orders) the
        # build must refuse, and nothing may be deleted or replaced.
        with tempfile.TemporaryDirectory() as tmp:
            dest = Path(tmp) / "Trades"
            (dest / "Open Trades").mkdir(parents=True)
            (dest / "Open Trades" / "Real Trade Aug 1 2026").write_text("BUY +1 SPX\n")
            with self.assertRaises(demo.NotOurs):
                demo.build(dest)
            self.assertEqual(
                snapshot(dest), {"Open Trades/Real Trade Aug 1 2026": b"BUY +1 SPX\n"}
            )
            # No staging directory left beside it either.
            self.assertEqual([p.name for p in Path(tmp).iterdir()], ["Trades"])
            self.assertEqual(demo.main(["--dest", str(dest)]), 1)
            self.assertTrue((dest / "Open Trades" / "Real Trade Aug 1 2026").is_file())

    def test_a_previous_build_and_an_empty_directory_are_replaced(self):
        with tempfile.TemporaryDirectory() as tmp:
            dest = Path(tmp) / "demo"
            dest.mkdir()
            demo.build(dest)  # empty: fine
            (dest / "Open Trades" / "stale").write_text("left over from an older build\n")
            demo.build(dest)  # ours: replaced wholesale
            self.assertFalse((dest / "Open Trades" / "stale").exists())
            self.assertEqual(snapshot(dest), snapshot(demo.DEFAULT_DEST))
            self.assertEqual([p.name for p in Path(tmp).iterdir()], ["demo"])

    def test_every_statement_uses_the_placeholder_account_and_synthetic_refs(self):
        for path in (demo.DEFAULT_DEST / "Daily Trade Statement").glob("*.csv"):
            text = path.read_text(encoding="utf-8-sig")
            self.assertIn(f"Account Statement for {cr.REPLACEMENT_ACCOUNT}", text)
            self.assertEqual(cr.offending_lines(text), [], path.name)
            for line in text.splitlines():
                if ",TRD," in line:
                    self.assertIn(f'="{demo.REF_BASE // 10}', line)

    def test_a_trade_file_exists_per_event(self):
        for event in demo.EVENTS:
            lines = (demo.DEFAULT_DEST / event.trade_file).read_text(encoding="utf-8").splitlines()
            self.assertIn(event.order_line, lines)

    def test_surface_matches_the_pricing_crate_ballpark(self):
        # A sanity anchor on the embedded surface replica: a 46-DTE ATM SPX
        # option on an 18.5%-ish short-dated vol is worth a few percent of spot.
        t = 46 / 365
        put = demo.theoretical("PUT", 7750.0, 7750.0, t)
        call = demo.theoretical("CALL", 7750.0, 7750.0, t)
        self.assertTrue(150 < put < 250, put)
        self.assertTrue(150 < call < 250, call)
        # Put skew: the smile is steeper on the downside.
        self.assertGreater(demo.implied_vol(-0.1, t), demo.implied_vol(0.1, t))


if __name__ == "__main__":
    unittest.main()
