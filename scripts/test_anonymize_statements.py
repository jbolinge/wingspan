#!/usr/bin/env python3
"""Unit tests for anonymize_statements.py's fail-closed behaviour.

Every fixture here is synthetic, built in-memory from the column layouts
`fixtures/trades/` already commits (never read from the real, uncommitted
`~/workspace/example_trades`). Run with:

    python3 -m unittest scripts/test_anonymize_statements.py
"""

from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import anonymize_statements as anon  # noqa: E402

ACCOUNT_ID = "87654321"


def build_statement(
    *,
    account_id: str = ACCOUNT_ID,
    extra_cash_row: str | None = None,
    order_history_row: str | None = None,
    trade_history_row: str | None = None,
    futures_amount: str = "0.00",
) -> str:
    """A minimal, fully-recognised statement (`\n` line endings; the caller
    of `anonymize_statement` applies CRLF, matching `process_statements`).
    Every keyword lets one test replace a single row to make it
    unrecognisable, without hand-maintaining the whole statement per test."""
    lines = [
        f"Account Statement for {account_id}SCHW (Individual) since 8/18/26 through 8/18/26",
        "",
        "Cash Balance",
        "DATE,TIME,TYPE,REF #,DESCRIPTION,Misc Fees,Commissions & Fees,AMOUNT,BALANCE",
        '8/18/26,00:00:00,BAL,,Cash balance at the start of business day 18.08 CST,,,,"100,000.00"',
        (
            '8/18/26,09:31:05,TRD,="1007650828803",'
            "BOT +2 BUTTERFLY SPX 100 19 FEB 27 [AM] 7325/6975/6625 PUT @22.10,"
            '-5.15,-5.20,"-4,420.00","95,580.00"'
        ),
    ]
    if extra_cash_row:
        lines.append(extra_cash_row)
    lines += [
        ',,,,TOTAL,-5.15,-5.20,"-4,420.00","95,580.00"',
        "",
        "Account Order History",
        "Notes,,Time Placed,Spread,Side,Qty,Pos Effect,Symbol,Exp,Strike,Type,PRICE,,TIF,Status",
        (
            order_history_row
            or ",,8/18/26 09:31:00,BUTTERFLY,BUY,+2,TO OPEN,SPX,19 FEB 27,7325,PUT,22.10,LMT,DAY,FILLED"
        ),
        "",
        "Account Trade History",
        "Exec Time,Spread,Side,Qty,Symbol,Exp,Strike,Type,Price,Net Price,Opra",
        (
            trade_history_row
            or "8/18/26 09:31:05,BUTTERFLY,BUY,+2,SPX,19 FEB 27,7325,PUT,22.10,22.10,SPX270219P7325"
        ),
        "",
        "Equities",
        "Symbol,Description,Qty,Trade Price,Mark,Mark Value",
        "ABCD,Widget Co,+50,10.0000,10.00,$500.00",
        ",OVERALL TOTALS,,,,$500.00",
        "",
        "Options",
        "Symbol,Option Code,Exp,Strike,Type,Qty,Trade Price,Mark,Mark Value",
        "SPX,SPX270219P7325,19 FEB 27,7325,PUT,-2,22.10,18.55,($3,710.00)",
        "",
        "Profits and Losses",
        "Symbol,Description,P/L Open,P/L %,P/L Day,P/L YTD,P/L Diff,Margin Req,Mark Value",
        "ABCD,Widget Co,$50.00,+1.00%,$5.00,$50.00,$0.00,$100.00,$500.00",
        ",OVERALL TOTALS,$50.00,+1.00%,$5.00,$50.00,$0.00,$100.00,$500.00",
        "",
        "Account Summary",
        "Net Liquidating Value,$95580.00",
        "Cash & Cash Investments,N/A",
        "",
        "Futures Statements",
        "Trade Date,Exec Date,Exec Time,Type,Ref #,Description,Misc Fees,Commissions & Fees,Amount,Balance",
        (
            "8/18/26,8/18/26,00:00:00,BAL,--,Futures cash balance at the "
            f"start of business day 18.08 CST,--,--,--,{futures_amount}"
        ),
        "",
        "Forex Statements",
        ",Date,Time,Type,Ref #,Description,Commissions & Fees,Amount,Amount(USD),Balance",
        "",
        '"Crypto # (Crypto offered by Charles Schwab Premier Bank, SSB) Statements"',
        "Trade Date,Exec Date,Exec Time,Type,Ref #,Description,Commissions & Fees,Amount,Balance",
        "",
        " ",
        '"Total Cash $95,580.00"',
    ]
    return "\n".join(lines)


class AnonymizeStatementTests(unittest.TestCase):
    def test_well_formed_statement_anonymises_cleanly(self):
        out, account_id = anon.anonymize_statement(build_statement())
        self.assertEqual(account_id, ACCOUNT_ID)
        self.assertNotIn(ACCOUNT_ID, out)
        self.assertIn(anon.REPLACEMENT_ACCOUNT, out)

    def test_unknown_cash_balance_row_type_is_rejected_not_passed_through(self):
        # Regression test: an ACH/wire/journal row used to be emitted
        # byte-verbatim (with its real, unscaled dollar amount) because
        # unrecognised Cash Balance TYPEs fell through to a silent
        # pass-through branch.
        text = build_statement(
            extra_cash_row=(
                '8/18/26,11:00:00,ACH,,ACH DEPOSIT FROM BANK XXXX1234,,,'
                '"50,000.00","145,580.00"'
            )
        )
        with self.assertRaises(anon.UnrecognisedRow):
            anon.anonymize_statement(text)

    def test_empty_symbol_order_history_row_is_rejected(self):
        # Regression test: `is_option_row` returning False for an empty
        # Symbol used to fall into the same `or fields[symbol_i] == ""`
        # branch as a genuine option row, so it was passed through
        # unanonymised instead of being inspected as its own comment said.
        text = build_statement(
            order_history_row=(
                ",,8/18/26 09:31:00,SINGLE,BUY,+50,TO OPEN,,,,,10.00,LMT,DAY,FILLED"
            )
        )
        with self.assertRaises(anon.UnrecognisedRow):
            anon.anonymize_statement(text)

    def test_empty_symbol_trade_history_row_is_rejected(self):
        text = build_statement(
            trade_history_row="8/18/26 09:31:05,SINGLE,BUY,+50,,,,,10.00,10.00,"
        )
        with self.assertRaises(anon.UnrecognisedRow):
            anon.anonymize_statement(text)

    def test_nonzero_futures_balance_is_rejected_not_leaked(self):
        # Regression test: Futures/Forex/Crypto sections used to be copied
        # through wholesale with no check that they still carried no real
        # dollar figures.
        text = build_statement(futures_amount="1234.56")
        with self.assertRaises(anon.UnrecognisedRow):
            anon.anonymize_statement(text)

    def test_zero_futures_balance_still_passes_through(self):
        # The check above must not be so strict that the ordinary, all-zero
        # case (every committed fixture) stops working.
        out, _ = anon.anonymize_statement(build_statement(futures_amount="0.00"))
        self.assertIn("Futures Statements", out)

    def test_account_id_resurfacing_anywhere_is_caught(self):
        # Output-side backstop: even if some future section change managed
        # to re-emit the real id, the final check must still catch it.
        text = build_statement()
        patched = text.replace(
            "Cash balance at the start of business day 18.08 CST",
            f"Cash balance at the start of business day 18.08 CST (re: {ACCOUNT_ID})",
        )
        with self.assertRaises(ValueError):
            anon.anonymize_statement(patched)

    def test_rerunning_over_already_anonymised_output_is_not_an_error(self):
        # A benign sanity re-run (--source pointed at the script's own
        # output) parses "00000000" out of the 00000000TEST placeholder.
        # That is not a real account id, so neither the in-statement
        # self-check nor the on-disk sweep may treat it as a leak.
        already = build_statement(account_id=anon.REPLACEMENT_ACCOUNT_DIGITS)
        out, account_id = anon.anonymize_statement(already)
        self.assertEqual(account_id, anon.REPLACEMENT_ACCOUNT_DIGITS)
        self.assertIn(anon.REPLACEMENT_ACCOUNT, out)
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "statement.csv").write_text(out)
            anon.assert_account_id_absent(root, account_id)  # must not raise

    def test_mismatched_account_ids_across_statements_are_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            src = Path(tmp) / "src"
            dst = Path(tmp) / "dst"
            src.mkdir()
            for name, acct in (("a.csv", "11111111"), ("b.csv", "22222222")):
                text = build_statement(account_id=acct).replace("\n", "\r\n")
                (src / name).write_bytes(anon.BOM + text.encode("utf-8"))
            with self.assertRaises(ValueError):
                anon.process_statements(src, dst)

    def test_assert_account_id_absent_catches_a_leak_on_disk(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "leaky.txt").write_text(f"account {ACCOUNT_ID} was here")
            with self.assertRaises(ValueError):
                anon.assert_account_id_absent(root, ACCOUNT_ID)

    def test_assert_account_id_absent_is_quiet_when_clean(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "clean.txt").write_text("nothing to see here")
            anon.assert_account_id_absent(root, ACCOUNT_ID)  # must not raise


def build_source_tree(root: Path, *, trade_file_body: str) -> Path:
    """A minimal synthetic SOURCE folder in the layout `run()` expects."""
    src = root / "Trades"
    (src / "Open Trades").mkdir(parents=True)
    (src / "Closed Trades").mkdir(parents=True)
    (src / "Daily Trade Statement").mkdir(parents=True)
    (src / "Open Trades" / "SPX 19 FEB 27.csv").write_text(trade_file_body)
    text = build_statement().replace("\n", "\r\n")
    (src / "Daily Trade Statement" / "2026-08-18.csv").write_bytes(
        anon.BOM + text.encode("utf-8")
    )
    return src


class RunStagingTests(unittest.TestCase):
    """`run()` must publish to DEST only after the on-disk sweep passes."""

    def test_clean_source_is_published_to_dest(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            src = build_source_tree(root, trade_file_body="Qty,Symbol\n+2,SPX\n")
            dest = root / "fixtures" / "trades"
            open_count, closed_count, stmt_count = anon.run(src, dest)
            self.assertEqual((open_count, closed_count, stmt_count), (1, 0, 1))
            self.assertTrue((dest / "README.md").is_file())
            self.assertTrue((dest / "Open Trades" / "SPX 19 FEB 27.csv").is_file())
            published = (dest / "Daily Trade Statement" / "2026-08-18.csv").read_text(
                encoding="utf-8-sig"
            )
            self.assertNotIn(ACCOUNT_ID, published)
            self.assertIn(anon.REPLACEMENT_ACCOUNT, published)

    def test_account_id_in_a_byte_verbatim_trade_file_leaves_dest_untouched(self):
        # Regression test for the residue this backstop exists to catch:
        # trade files are copied byte-verbatim, and the default DEST is the
        # committed fixtures dir, so a bare account id in one of them must
        # abort *before* anything is published - not after. Nothing
        # downstream would catch it: a bare id matches neither redaction.rs
        # (needs the "Account Statement for " prefix) nor check_redaction.py
        # (needs 8 digits followed by 2+ uppercase letters).
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            src = build_source_tree(
                root, trade_file_body=f"Qty,Symbol,Note\n+2,SPX,acct {ACCOUNT_ID}\n"
            )
            dest = root / "fixtures" / "trades"
            with self.assertRaises(ValueError):
                anon.run(src, dest)
            self.assertFalse(
                dest.exists(), "DEST must not exist after a failed anonymisation"
            )

    def test_unrecognised_row_leaves_dest_untouched(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            src = build_source_tree(root, trade_file_body="Qty,Symbol\n+2,SPX\n")
            bad = build_statement(
                extra_cash_row=(
                    '8/18/26,11:00:00,ACH,,ACH DEPOSIT FROM BANK XXXX1234,,,'
                    '"50,000.00","145,580.00"'
                )
            ).replace("\n", "\r\n")
            (src / "Daily Trade Statement" / "2026-08-18.csv").write_bytes(
                anon.BOM + bad.encode("utf-8")
            )
            dest = root / "fixtures" / "trades"
            with self.assertRaises(anon.UnrecognisedRow):
                anon.run(src, dest)
            self.assertFalse(dest.exists())

    def test_staging_directory_is_removed_after_a_failure(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            src = build_source_tree(
                root, trade_file_body=f"Qty,Symbol,Note\n+2,SPX,acct {ACCOUNT_ID}\n"
            )
            before = set(Path(tempfile.gettempdir()).glob("wingspan-anonymize-*"))
            with self.assertRaises(ValueError):
                anon.run(src, root / "dest")
            after = set(Path(tempfile.gettempdir()).glob("wingspan-anonymize-*"))
            self.assertEqual(before, after, "staging dir must not survive a failure")


if __name__ == "__main__":
    unittest.main()
