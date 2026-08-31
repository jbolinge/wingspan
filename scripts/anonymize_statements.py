#!/usr/bin/env python3
"""Anonymise the real example trades folder into fixtures/trades/.

Reads SOURCE (real, private data, never committed) and writes DEST with the
same layout (Open Trades/, Closed Trades/, Daily Trade Statement/):

- Trade files (Open/Closed): copied byte-verbatim (dotfiles skipped).
- Statement CSVs: byte-for-byte format is preserved (UTF-8 BOM, CRLF line
  endings, quoting style, section order, and every dialect oddity) except:
    * the real account number is replaced by 00000000TEST everywhere,
    * every OPTION-related row (option orders/trades/positions, and TRD
      rows for options in Cash Balance) is kept byte-verbatim in every
      field EXCEPT the Cash Balance running BALANCE column, which is
      rescaled (see below) — golden P&L tests depend on the untouched
      fields (AMOUNT, Commissions & Fees, descriptions, OSI codes, ...),
    * everything else (equities/ETF/futures/forex/crypto position and P&L
      sections, non-option trade/order history rows, Account Summary,
      non-option Cash Balance rows) is anonymised: tickers -> DUMMY,
      dollar amounts rescaled by a fixed deterministic factor, position
      sections collapsed to one plausible dummy row.

The Cash Balance running BALANCE column is always recomputed from a fake
opening balance plus each row's *emitted* AMOUNT (the real amount for
option rows, the rescaled amount for non-option rows), so it stays
internally self-consistent even though it is not byte-identical to the
source.

Python 3.13, stdlib only. Usage:

    python3 scripts/anonymize_statements.py [--source DIR] [--dest DIR]

Defaults: SOURCE=~/workspace/example_trades/Trades, DEST=<repo>/fixtures/trades
"""

from __future__ import annotations

import argparse
import csv
import io
import re
import shutil
import sys
from decimal import ROUND_HALF_UP, Decimal
from pathlib import Path

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

# Deterministic rescale factor applied to every non-option dollar amount.
# Picked arbitrarily but fixed, so runs are idempotent/reproducible.
SCALE = Decimal("0.4213")

# Added to every day's rescaled opening Cash Balance to build the fake
# opening balance. Option TRD rows keep their REAL dollar amount (rule: P&L
# golden tests depend on it) while non-option rows are merely rescaled, so
# a same-day option trade can be an order of magnitude larger than the
# rescaled opening float alone - this baseline guarantees headroom so the
# recomputed running balance never goes implausibly low or negative (the
# largest single day's net option cash flow across the fixtures is ~$12k;
# $75,000 comfortably covers that with margin to spare).
BASE_OPENING_BALANCE = Decimal("75000.00")

REPLACEMENT_ACCOUNT = "00000000TEST"

# Matches the account-number prefix in "Account Statement for 00000000SCHW ..."
ACCOUNT_HEADER_RE = re.compile(
    r"^(Account Statement for )(\d+)([A-Z]+ .*)$"
)

BOM = b"\xef\xbb\xbf"

SECTION_HEADERS = {
    "Cash Balance",
    "Futures Statements",
    "Forex Statements",
    "Account Order History",
    "Account Trade History",
    "Equities",
    "Options",
    "Profits and Losses",
    "Account Summary",
}
# "Crypto ... Statements" varies in wording/quoting; matched separately.
CRYPTO_HEADER_RE = re.compile(r"^\"?Crypto\b.*Statements\"?$")

NUMERIC_RE = re.compile(r"^-?\d+(\.\d+)?$")

OPTION_UNDERLYING = "SPX"

# Cash Balance descriptions for option orders always contain the multiplier
# token "SPX 100" (e.g. "BOT +2 BUTTERFLY SPX 100 19 FEB 27 [AM] ...").
OPTION_DESC_RE = re.compile(r"\bSPX 100\b")


# ---------------------------------------------------------------------------
# Small helpers
# ---------------------------------------------------------------------------


def rescale(value: Decimal, scale: Decimal = SCALE) -> Decimal:
    return (value * scale).quantize(Decimal("0.01"), rounding=ROUND_HALF_UP)


def parse_money(text: str) -> Decimal | None:
    """Parse a TOS-formatted money field ("$1,234.56", "(1,234.56)", "--", "N/A", "")."""
    t = text.strip()
    if t in ("", "--", "N/A", "~"):
        return None
    neg = False
    if t.startswith("(") and t.endswith(")"):
        neg = True
        t = t[1:-1]
    elif t.startswith("-"):
        neg = True
        t = t[1:]
    t = t.replace("$", "").replace(",", "")
    if t == "":
        return None
    try:
        d = Decimal(t)
    except Exception:
        return None
    return -d if neg else d


def format_like(template: str, value: Decimal) -> str:
    """Format `value` reusing the punctuation style ($ prefix, thousands
    separator, parens-vs-dash negative, decimal places) of `template`."""
    t = template.strip()
    paren = t.startswith("(") and t.endswith(")")
    core = t[1:-1] if paren else (t[1:] if t.startswith("-") else t)
    dollar = "$" in core
    core_num = core.replace("$", "").replace(",", "")
    decimals = len(core_num.split(".", 1)[1]) if "." in core_num else 2
    mag = abs(value)
    s = f"{mag:,.{decimals}f}"
    if dollar:
        s = "$" + s
    if value < 0:
        s = f"({s})" if paren else f"-{s}"
    return s


def csv_fields(line: str) -> list[str]:
    """Split one CSV line (no line terminator) into fields, honoring quotes.
    Used only for rows we synthesise wholesale (dummy rows) or for reading
    values (header lookups, parsing) - never to reconstruct a row that must
    keep untouched fields byte-identical (see split_csv_raw for that)."""
    return next(csv.reader(io.StringIO(line)))


def csv_join(fields: list[str]) -> str:
    """Join fields into a CSV line using minimal quoting (quote iff the
    field contains a comma, quote, or leading/trailing space) - matches the
    quoting style observed throughout the source statements. Only for rows
    synthesised wholesale; see split_csv_raw/format_raw_field for surgical
    edits that must leave untouched fields byte-identical."""
    out = io.StringIO()
    w = csv.writer(out, lineterminator="", quoting=csv.QUOTE_MINIMAL)
    w.writerow(fields)
    return out.getvalue()


def split_csv_raw(line: str) -> list[str]:
    """Split one CSV line into fields, keeping each field's *exact original
    text* including its quote characters (if any) and internal doubled-quote
    escaping untouched. Unlike csv.reader (which strips quoting), this lets
    us patch just the fields that need to change and reassemble the rest
    byte-identical to the source - critical for fields like the Excel-style
    ref `="1007650828803"`, which csv.writer would otherwise mis-requote."""
    fields: list[str] = []
    cur: list[str] = []
    in_quotes = False
    i = 0
    n = len(line)
    while i < n:
        c = line[i]
        if in_quotes:
            if c == '"':
                if i + 1 < n and line[i + 1] == '"':
                    cur.append('""')
                    i += 2
                    continue
                in_quotes = False
                cur.append(c)
                i += 1
                continue
            cur.append(c)
            i += 1
            continue
        if c == '"':
            in_quotes = True
            cur.append(c)
            i += 1
            continue
        if c == ",":
            fields.append("".join(cur))
            cur = []
            i += 1
            continue
        cur.append(c)
        i += 1
    fields.append("".join(cur))
    return fields


def format_raw_field(raw_field: str, value: Decimal) -> str:
    """Reformat one raw (still-quoted) field to a new value, reusing its
    original punctuation style ($ prefix, parens-vs-dash negative, decimal
    places). Requoted iff the *new* text needs it (contains a comma) -
    matches TOS's own convention of only quoting fields whose formatted
    value is >= 1000, rather than mechanically preserving the source row's
    quoting (which was sized for the real, unscaled amount)."""
    quoted = raw_field.startswith('"') and raw_field.endswith('"') and len(raw_field) >= 2
    inner = raw_field[1:-1] if quoted else raw_field
    text = format_like(inner, value)
    return f'"{text}"' if "," in text else text


def patch_raw_line(line: str, patches: dict[int, str]) -> str:
    """Replace specific field indices in `line` with pre-formatted raw text
    (already quoted as needed), leaving every other field byte-identical."""
    raw_fields = split_csv_raw(line)
    for idx, new_raw in patches.items():
        raw_fields[idx] = new_raw
    return ",".join(raw_fields)


def is_option_row(fields: list[str], symbol_idx: int) -> bool:
    if len(fields) <= symbol_idx:
        return False
    sym = fields[symbol_idx].strip()
    if sym == "":
        # Continuation row with no symbol of its own (shouldn't normally
        # happen for the sections we scan since the leg's symbol column is
        # always populated) - treat conservatively as non-option so it gets
        # inspected rather than silently passed through.
        return False
    return sym == OPTION_UNDERLYING


# ---------------------------------------------------------------------------
# Statement anonymisation
# ---------------------------------------------------------------------------


def anon_cash_balance_section(lines: list[str]) -> list[str]:
    """lines includes the 'Cash Balance' header through (not including) the
    next section header / blank separator before it."""
    out = [lines[0]]  # "Cash Balance"
    running_balance: Decimal | None = None
    misc_sum = Decimal("0")
    comm_sum = Decimal("0")
    amount_sum = Decimal("0")

    idx = 1
    # column header line
    out.append(lines[idx])
    header_fields = csv_fields(lines[idx])
    type_i = header_fields.index("TYPE")
    desc_i = header_fields.index("DESCRIPTION")
    misc_i = header_fields.index("Misc Fees")
    comm_i = header_fields.index("Commissions & Fees")
    amt_i = header_fields.index("AMOUNT")
    bal_i = header_fields.index("BALANCE")
    idx += 1

    while idx < len(lines):
        raw = lines[idx]
        if raw.strip() == "":
            out.append(raw)
            idx += 1
            continue
        # csv_fields (quote-stripping) only to *read* values / classify the
        # row; the emitted line is built by patching split_csv_raw's
        # quote-preserving fields so untouched columns stay byte-identical.
        fields = csv_fields(raw)
        raw_fields = split_csv_raw(raw)
        row_type = fields[type_i].strip()
        desc = fields[desc_i]

        if row_type == "BAL":
            real_open = parse_money(fields[bal_i])
            assert real_open is not None
            fake_opening = BASE_OPENING_BALANCE + rescale(real_open)
            running_balance = fake_opening
            patched = patch_raw_line(
                raw, {bal_i: format_raw_field(raw_fields[bal_i], fake_opening)}
            )
            out.append(patched)
            idx += 1
            continue

        if row_type == "TRD":
            assert running_balance is not None
            option_row = OPTION_DESC_RE.search(desc) is not None
            patches: dict[int, str] = {}
            if option_row:
                # Byte-verbatim except BALANCE (recomputed below).
                real_amt = parse_money(fields[amt_i]) or Decimal("0")
                running_balance += real_amt
                misc_sum += parse_money(fields[misc_i]) or Decimal("0")
                comm_sum += parse_money(fields[comm_i]) or Decimal("0")
                amount_sum += real_amt
            else:
                # Non-option: rescale AMOUNT (and Misc/Commissions, which
                # are also dollar amounts); anonymise ticker+price embedded
                # in the description if present.
                real_amt = parse_money(fields[amt_i]) or Decimal("0")
                new_amt = rescale(real_amt)
                patches[amt_i] = format_raw_field(raw_fields[amt_i], new_amt)

                real_misc = parse_money(fields[misc_i])
                new_misc = rescale(real_misc) if real_misc is not None else Decimal("0")
                if real_misc is not None:
                    patches[misc_i] = format_raw_field(raw_fields[misc_i], new_misc)

                real_comm = parse_money(fields[comm_i])
                new_comm = rescale(real_comm) if real_comm is not None else Decimal("0")
                if real_comm is not None:
                    patches[comm_i] = format_raw_field(raw_fields[comm_i], new_comm)

                new_desc = anon_equity_description(desc)
                if new_desc != desc:
                    patches[desc_i] = new_desc

                running_balance += new_amt
                misc_sum += new_misc
                comm_sum += new_comm
                amount_sum += new_amt

            patches[bal_i] = format_raw_field(raw_fields[bal_i], running_balance)
            out.append(patch_raw_line(raw, patches))
            idx += 1
            continue

        if desc.strip() == "TOTAL":
            assert running_balance is not None
            patches = {
                misc_i: format_raw_field(raw_fields[misc_i], misc_sum),
                comm_i: format_raw_field(raw_fields[comm_i], comm_sum),
                amt_i: format_raw_field(raw_fields[amt_i], amount_sum),
                bal_i: format_raw_field(raw_fields[bal_i], running_balance),
            }
            out.append(patch_raw_line(raw, patches))
            idx += 1
            continue

        # Unknown row shape: pass through unchanged (defensive).
        out.append(raw)
        idx += 1

    return out


EQUITY_DESC_RE = re.compile(
    r"^(BOT|SOLD)\s+([+-]?\d+)\s+([A-Z][A-Z0-9.]*)\s+@([0-9.]+)$"
)


def anon_equity_description(desc: str) -> str:
    m = EQUITY_DESC_RE.match(desc.strip())
    if not m:
        return desc
    side, qty, _ticker, price = m.groups()
    new_price = rescale(Decimal(price))
    decimals = len(price.split(".", 1)[1]) if "." in price else 2
    return f"{side} {qty} DUMMY @{new_price:.{decimals}f}"


def anon_order_history_section(lines: list[str]) -> list[str]:
    out = [lines[0], lines[1]]  # header + column header
    header_fields = csv_fields(lines[1])
    symbol_i = header_fields.index("Symbol")
    price_i = header_fields.index("PRICE")
    for raw in lines[2:]:
        if raw.strip() == "":
            out.append(raw)
            continue
        fields = csv_fields(raw)
        if is_option_row(fields, symbol_i) or fields[symbol_i].strip() == "":
            out.append(raw)
            continue
        new_fields = list(fields)
        new_fields[symbol_i] = "DUMMY"
        if price_i < len(fields) and NUMERIC_RE.match(fields[price_i].strip()):
            price = Decimal(fields[price_i].strip())
            new_fields[price_i] = format_like(fields[price_i], rescale(price))
        out.append(csv_join(new_fields))
    return out


def anon_trade_history_section(lines: list[str]) -> list[str]:
    out = [lines[0], lines[1]]  # header + column header
    header_fields = csv_fields(lines[1])
    symbol_i = header_fields.index("Symbol")
    price_i = header_fields.index("Price")
    net_price_i = header_fields.index("Net Price")
    for raw in lines[2:]:
        if raw.strip() == "":
            out.append(raw)
            continue
        fields = csv_fields(raw)
        if is_option_row(fields, symbol_i) or fields[symbol_i].strip() == "":
            out.append(raw)
            continue
        new_fields = list(fields)
        new_fields[symbol_i] = "DUMMY"
        for i in (price_i, net_price_i):
            if i < len(fields) and NUMERIC_RE.match(fields[i].strip()):
                p = Decimal(fields[i].strip())
                new_fields[i] = format_like(fields[i], rescale(p))
        out.append(csv_join(new_fields))
    return out


def anon_equities_section(lines: list[str]) -> list[str]:
    out = [lines[0], lines[1]]  # "Equities" + column header
    out.append(
        csv_join(["DUMMY", "Dummy ETF Holding", "+100", "50.0000", "50.00", "$5,000.00"])
    )
    out.append(csv_join(["", "OVERALL TOTALS", "", "", "", "$5,000.00"]))
    return out


def anon_pnl_section(lines: list[str]) -> list[str]:
    out = [lines[0], lines[1]]  # "Profits and Losses" + column header
    out.append(
        csv_join(
            [
                "DUMMY",
                "Dummy ETF Holding",
                "$50.00",
                "+1.00%",
                "$5.00",
                "$50.00",
                "$0.00",
                "$100.00",
                "$5,000.00",
            ]
        )
    )
    out.append(
        csv_join(
            [
                "",
                "OVERALL TOTALS",
                "$50.00",
                "+1.00%",
                "$5.00",
                "$50.00",
                "$0.00",
                "$100.00",
                "$5,000.00",
            ]
        )
    )
    return out


def anon_account_summary_section(lines: list[str]) -> list[str]:
    out = [lines[0]]  # "Account Summary"
    for raw in lines[1:]:
        if raw.strip() == "":
            out.append(raw)
            continue
        fields = csv_fields(raw)
        if len(fields) != 2:
            out.append(raw)
            continue
        label, value = fields
        real = parse_money(value)
        if real is None:
            out.append(raw)  # e.g. "N/A"
            continue
        out.append(csv_join([label, format_like(value, rescale(real))]))
    return out


def anon_total_cash_line(line: str, final_balance: Decimal) -> str:
    # Whole line is a single quoted CSV field: "Total Cash $12,356.25"
    fields = csv_fields(line)
    assert len(fields) == 1
    m = re.match(r"^(Total Cash \$)([0-9,]+\.\d{2})$", fields[0])
    if not m:
        return line
    new_text = f"{m.group(1)}{final_balance:,.2f}"
    return csv_join([new_text])


def anon_passthrough_section(lines: list[str], final_balance: Decimal | None) -> list[str]:
    """Copy a section through unchanged, except for the stray "Total Cash"
    row, which can land inside Futures Statements' or the Crypto section's
    captured line range depending on the statement (its position between
    Futures/Forex/Crypto varies) and must always be rescaled."""
    out = []
    for raw in lines:
        if final_balance is not None and re.match(r'^"Total Cash \$', raw.strip()):
            out.append(anon_total_cash_line(raw, final_balance))
        else:
            out.append(raw)
    return out


def anonymize_statement(text: str) -> str:
    """text has \n line endings (CRLF already normalised by the caller);
    returns text with \n endings; caller re-applies CRLF."""
    raw_lines = text.split("\n")

    # Header line: account number.
    m = ACCOUNT_HEADER_RE.match(raw_lines[0])
    if not m:
        raise ValueError(f"unrecognised header line: {raw_lines[0]!r}")
    raw_lines[0] = f"{m.group(1)}{REPLACEMENT_ACCOUNT}{m.group(3)}"

    out: list[str] = [raw_lines[0]]
    i = 1
    final_cash_balance: Decimal | None = None

    while i < len(raw_lines):
        line = raw_lines[i]
        stripped = line.strip()

        if stripped in SECTION_HEADERS or CRYPTO_HEADER_RE.match(stripped):
            # Collect this section's lines (header through line before the
            # next section header or crypto header or EOF).
            section_name = stripped if stripped in SECTION_HEADERS else "Crypto"
            j = i + 1
            while j < len(raw_lines):
                s = raw_lines[j].strip()
                if s in SECTION_HEADERS or CRYPTO_HEADER_RE.match(s):
                    break
                j += 1
            section_lines = raw_lines[i:j]

            if section_name == "Cash Balance":
                result = anon_cash_balance_section(section_lines)
                # Track final running balance for the "Total Cash" line and
                # for Account Order History downstream anonymisation is not
                # needed there, but we need it for the stray Total Cash row.
                # Recover it from the BAL/TOTAL processing: recompute by
                # re-parsing the emitted section (cheap and robust).
                out.extend(result)
                final_cash_balance = _extract_final_balance(result)
            elif section_name == "Account Order History":
                out.extend(anon_order_history_section(section_lines))
            elif section_name == "Account Trade History":
                out.extend(anon_trade_history_section(section_lines))
            elif section_name == "Equities":
                out.extend(anon_equities_section(section_lines))
            elif section_name == "Options":
                out.extend(section_lines)  # byte-verbatim
            elif section_name == "Profits and Losses":
                out.extend(anon_pnl_section(section_lines))
            elif section_name == "Account Summary":
                out.extend(anon_account_summary_section(section_lines))
            else:
                # Futures/Forex/Crypto Statements: no data rows to anonymise
                # in the source fixtures; copy through (rewriting the stray
                # "Total Cash" row wherever it lands - see helper docstring).
                out.extend(anon_passthrough_section(section_lines, final_cash_balance))

            i = j
            continue

        # Stray "Total Cash $X" row, on the rare chance it sits outside any
        # recognised section's captured line range.
        if re.match(r'^"Total Cash \$', stripped):
            assert final_cash_balance is not None
            out.append(anon_total_cash_line(line, final_cash_balance))
            i += 1
            continue

        # Blank lines, the single-space line, and anything else: pass through.
        out.append(line)
        i += 1

    return "\n".join(out)


def _extract_final_balance(cash_section_lines: list[str]) -> Decimal:
    """The last non-blank row's BALANCE column (BAL/TRD/TOTAL rows all
    populate it; the last row of the day is always the source of truth,
    matching the "Total Cash" stray row in the original statements)."""
    header_fields = csv_fields(cash_section_lines[1])
    bal_i = header_fields.index("BALANCE")
    last: Decimal | None = None
    for raw in cash_section_lines[2:]:
        if raw.strip() == "":
            continue
        fields = csv_fields(raw)
        v = parse_money(fields[bal_i])
        if v is not None:
            last = v
    assert last is not None
    return last


# ---------------------------------------------------------------------------
# Driver
# ---------------------------------------------------------------------------


def process_trade_dir(src: Path, dst: Path) -> int:
    """Copy trade files byte-verbatim, skipping dotfiles. Returns count copied."""
    count = 0
    dst.mkdir(parents=True, exist_ok=True)
    for entry in sorted(src.iterdir()):
        if entry.name.startswith("."):
            continue
        if not entry.is_file():
            continue
        shutil.copyfile(entry, dst / entry.name)
        count += 1
    return count


def process_statements(src: Path, dst: Path) -> int:
    count = 0
    dst.mkdir(parents=True, exist_ok=True)
    for entry in sorted(src.iterdir()):
        if entry.name.startswith("."):
            continue
        if not entry.is_file():
            continue
        raw = entry.read_bytes()
        has_bom = raw.startswith(BOM)
        body = raw[len(BOM):] if has_bom else raw
        text = body.decode("utf-8")
        assert "\r\n" in text, f"{entry}: expected CRLF line endings"
        assert "\n" not in text.replace("\r\n", ""), f"{entry}: unexpected bare LF"
        normalized = text.replace("\r\n", "\n")
        anonymized = anonymize_statement(normalized)
        out_text = anonymized.replace("\n", "\r\n")
        out_bytes = (BOM if has_bom else b"") + out_text.encode("utf-8")
        (dst / entry.name).write_bytes(out_bytes)
        count += 1
    return count


README = """# fixtures/trades

Anonymised copy of the example TOS trades folder, generated by
`scripts/anonymize_statements.py` from the real (never-committed)
`~/workspace/example_trades/Trades/` folder. Used by tests and
`wingspan serve --demo`.

What was anonymised, and what was deliberately left alone:

- **Trade files** (`Open Trades/`, `Closed Trades/`): copied byte-verbatim.
  They never contain the account number or any dollar balances.
- **Statement CSVs** (`Daily Trade Statement/`): the file *format* (UTF-8
  BOM, CRLF line endings, quoting style, section order, and every dialect
  oddity - the stray "Total Cash" row, the single-space line, `="..."`
  Excel refs, etc.) is preserved exactly.
  - The real account number is replaced by `00000000TEST` everywhere.
  - Every OPTION-related row (option orders/trades, the `Options` position
    section, and option TRD rows in `Cash Balance`) is kept byte-verbatim
    in every field that P&L golden tests depend on: AMOUNT, Commissions &
    Fees, descriptions, OSI/OPRA codes. The one exception is the running
    `BALANCE` column in `Cash Balance`, which is always recomputed (see
    below), so it is not byte-identical even on an all-options day.
  - Everything else is anonymised: equity/ETF ticker symbols become
    `DUMMY`, dollar amounts are rescaled by a fixed factor, and the
    Equities / Profits and Losses position sections are collapsed to a
    single plausible dummy row (Futures/Forex/Crypto sections carry no
    data rows in the source and are copied through). Account Summary
    (net-liq, buying power, YTD fees) is rescaled the same way.
  - The `Cash Balance` running `BALANCE` column is recomputed from a fake
    opening balance (a fixed baseline plus the real opening balance
    rescaled, so there's always headroom for a same-day option trade,
    which keeps its real, unscaled dollar size) plus each row's *emitted*
    amount: the real amount for option rows, the rescaled amount for
    non-option rows. It stays internally self-consistent even though it
    mixes real and rescaled figures.

Never copy `~/workspace/example_trades` here un-anonymised - re-run the
script instead:

    python3 scripts/anonymize_statements.py
"""


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    repo_root = Path(__file__).resolve().parent.parent
    default_source = Path.home() / "workspace" / "example_trades" / "Trades"
    default_dest = repo_root / "fixtures" / "trades"
    parser.add_argument("--source", type=Path, default=default_source)
    parser.add_argument("--dest", type=Path, default=default_dest)
    args = parser.parse_args()

    source: Path = args.source
    dest: Path = args.dest

    if not source.is_dir():
        print(f"error: source dir not found: {source}", file=sys.stderr)
        return 1

    open_count = process_trade_dir(source / "Open Trades", dest / "Open Trades")
    closed_count = process_trade_dir(source / "Closed Trades", dest / "Closed Trades")
    stmt_count = process_statements(
        source / "Daily Trade Statement", dest / "Daily Trade Statement"
    )

    (dest / "README.md").write_text(README, encoding="utf-8")

    print(
        f"wrote {open_count} open trade file(s), {closed_count} closed trade "
        f"file(s), {stmt_count} statement(s) to {dest}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
