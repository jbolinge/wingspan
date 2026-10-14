#!/usr/bin/env python3
"""Guards for make_demo_book.py: the committed demo book must be exactly what
the script writes, and it must be recognisably synthetic.

Run with:

    python3 -m unittest scripts/test_make_demo_book.py
"""

from __future__ import annotations

import csv
import re
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

    def test_the_legs_csv_is_the_sixteen_column_export(self):
        # Pins the shape of the generated legs.csv on its own terms. The
        # byte-parity check above already proves this file matches the
        # committed fixtures/demo/legs.csv, which `wingspan-cli export-legs`
        # writes from the same book; this test says what that shape must be.
        with tempfile.TemporaryDirectory() as tmp:
            dest = Path(tmp) / "demo"
            demo.build(dest)
            with (dest / "legs.csv").open(newline="", encoding="utf-8") as fh:
                rows = list(csv.reader(fh))

        header, data = rows[0], rows[1:]
        self.assertEqual(
            header,
            ["#", "Trade", "Leg", "Side", "Qty", "Opened", "Open Price", "Closed",
             "Close Price", "Mark", "Status", "Fees", "Expiry", "OCC Symbol", "Structure", "Key"],
        )
        self.assertEqual(len(data), 9)

        structures = {row[14] for row in data}
        self.assertEqual(
            structures,
            {
                "-1x STRG 8200/7200 OCT26",
                "-2x IC 8400/8500/7000/6900 NOV26",
                "10x XSP 650P DEC26",
                "-2x VERT 7400/7350 SEP26 · 2x VERT 7400/7350 SEP26",
            },
        )

        self.assertEqual([row[0] for row in data], ["1", "1", "2", "2", "2", "2", "3", "4", "4"])

        marks = [row[9] for row in data]
        self.assertEqual(sum(1 for m in marks if m != ""), 7)

        fees = [row[11] for row in data]
        self.assertTrue(all(f != "" for f in fees))
        non_zero_fees = sorted(f for f in fees if f != "0.00")
        self.assertEqual(non_zero_fees, ["10.08", "10.08", "12.60", "2.52"])

        # The XSP hedge: its own code, PM, ten contracts long.
        hedge = [row for row in data if row[1] == "XSP Hedge"]
        self.assertEqual(len(hedge), 1)
        self.assertEqual(hedge[0][3:5], ["BUY", "10"])
        self.assertEqual(hedge[0][12:14], ["2026-12-18", "XSP261218P650"])

        key_re = re.compile(r"^[^|]+ \d{4}-\d{2}-\d{2}\|\|\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}$")
        for row in data:
            self.assertRegex(row[15], key_re)

    def test_a_trade_file_exists_per_event(self):
        for event in demo.EVENTS:
            lines = (demo.DEFAULT_DEST / event.trade_file).read_text(encoding="utf-8").splitlines()
            self.assertIn(event.order_line, lines)

    def test_an_xsp_contract_is_priced_as_spx_at_ten_times_the_strike_over_ten(self):
        # `OptionContract::pricing_proxy`: XSP K is SPX 10K, every price / 10,
        # and XSP is PM-settled (16:00 New York), never AM.
        xsp = demo.Contract(demo.DEC, 650, "PUT", "XSP")
        self.assertEqual(xsp.code, "XSP261218P650")
        self.assertEqual(xsp.ratio, 10)
        self.assertEqual(xsp.expiry_instant().hour, 16)
        self.assertEqual(demo.Contract(demo.DEC, 6500, "PUT").expiry_instant().hour, 9)
        at = demo.ct(demo.date(2026, 8, 26), 14, 5, 17)
        t = xsp.years_to_expiry(at)
        proxy = demo.theoretical("PUT", 7742.0, 6500.0, t)
        self.assertEqual(xsp.mark(at, 7742.0), demo.cent(proxy / 10))
        self.assertTrue(1.0 < float(xsp.mark(at, 7742.0)) < 10.0)

    def test_a_single_fills_on_the_spx_order_tick(self):
        # `tos_format::round_tos_price`: $0.05 below $3.00, $0.10 at or above.
        D = demo.Decimal
        self.assertEqual(demo.tos_tick(D("3.83")), D("3.80"))
        self.assertEqual(demo.tos_tick(D("3.85")), D("3.90"))
        self.assertEqual(demo.tos_tick(D("2.97")), D("2.95"))
        self.assertEqual(demo.tos_tick(D("2.98")), D("3.00"))
        self.assertEqual(demo.tos_tick(D("38.25")), D("38.30"))

    def test_the_xsp_hedge_is_ordered_as_xsp_and_fills_at_its_own_mark(self):
        (hedge,) = [e for e in demo.EVENTS if e.trade_file.startswith("Open Trades/XSP Hedge")]
        self.assertEqual(hedge.spread, "SINGLE")
        (leg,) = hedge.legs
        self.assertEqual((leg.contract.underlying, leg.contract.strike, leg.qty), ("XSP", 650, 10))
        mark = leg.contract.mark(hedge.at, demo.SPOTS[hedge.at.date()][0])
        fill = demo.tos_tick(mark)
        self.assertEqual(leg.contract.fill(hedge.at, demo.SPOTS[hedge.at.date()][0]), fill)
        self.assertEqual(hedge.net_price, fill)
        self.assertNotEqual(fill, mark, "the mark is on a penny grid, the fill on the tick")
        # 18 DEC 26 is a third Friday: no `(Weeklys)`, and never `[AM]`.
        self.assertEqual(hedge.order_line, f"BUY +10 XSP 100 18 DEC 26 650 PUT @{fill:.2f} LMT")
        self.assertEqual(demo.order_render(hedge), "10x XSP 650P DEC26")
        # The statement names the XSP symbol on every row that has one.
        text = (demo.DEFAULT_DEST / "Daily Trade Statement" / "2026-08-26-AccountStatement.csv").read_text(
            encoding="utf-8-sig"
        )
        self.assertIn("BOT +10 XSP 100 18 DEC 26 650 PUT", text)
        self.assertIn(",XSP261218P650", text)
        self.assertNotIn("SPX,18 DEC 26,650", text)

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
