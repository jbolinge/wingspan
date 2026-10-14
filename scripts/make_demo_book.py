#!/usr/bin/env python3
"""Generate fixtures/demo/: the synthetic `wingspan-devserver serve --demo` book.

Nothing in the output ever existed in a brokerage account. The trades, their
fill prices, the statement marks and the cash balances are all made up here,
from a handful of invented index levels and the same SVI smile the app's own
synthetic price source uses (`crates/wingspan-marketdata/src/fixture.rs`), so
the demo's P&L and greeks come out plausible rather than random.

The book (valued by `--demo` on 2026-08-31):

    Open Trades/Oct Strangle Aug 24 2026      a ~45-DTE short strangle
    Open Trades/Nov Condor Aug 26 2026        a 2-lot iron condor, one expiry out
    Open Trades/XSP Hedge Aug 26 2026         ten long Mini-SPX (XSP) December
                                              puts: a tail hedge, priced as the
                                              SPX put ten times the strike, ÷ 10
    Closed Trades/Sep Put Spread Aug 10 2026 Closed
                                              a put credit spread sold 8/10,
                                              bought back 8/21 via a `# date`
                                              hint (the closing fill sits
                                              outside the ±7-day match window)

Each fill day gets a thinkorswim-shaped daily statement in the same dialect
`fixtures/trades/` exercises (UTF-8 BOM, CRLF, `="ref"` Excel refs,
parenthesised negatives, the stray "Total Cash" row, `DEBIT`/`CREDIT`
continuation rows), so the demo goes through exactly the parsing the real
data does. Execution refs are an obviously synthetic `9500000000001…`
sequence and the account id is the `00000000TEST` placeholder the redaction
guards expect.

To change the demo book, edit `EVENTS`/`SPOTS` below and rerun; the output is
deterministic, and `scripts/test_make_demo_book.py` fails if the committed
fixtures drift from what this script writes.

Python 3.13, stdlib only. Usage:

    python3 scripts/make_demo_book.py [--dest DIR]
"""

from __future__ import annotations

import argparse
import csv
import math
import re
import shutil
import sys
import tempfile
from collections import deque
from dataclasses import dataclass
from datetime import date, datetime, time, timedelta, timezone
from decimal import ROUND_HALF_UP, Decimal
from pathlib import Path
from zoneinfo import ZoneInfo

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_DEST = REPO_ROOT / "fixtures" / "demo"

ACCOUNT = "00000000TESTSCHW"
REF_BASE = 9_500_000_000_000  # refs are REF_BASE + 1, + 2, …
OPENING_CASH = Decimal("124312.55")
DUMMY_EQUITY_VALUE = Decimal("5000.00")
COMMISSION_PER_CONTRACT = Decimal("0.65")
MISC_FEE_PER_CONTRACT = Decimal("0.61")

NEW_YORK = ZoneInfo("America/New_York")
CHICAGO = ZoneInfo("America/Chicago")

# --------------------------------------------------------------- the surface
# A copy of `FixtureConfig::default()` — the demo prices are solved on this
# smile at run time, so fills taken from the same smile a few days earlier
# land where the dashboard expects them.
SPOT_VALUATION = 7750.0
ATM_SHORT = 0.185
ATM_LONG = 0.130
TERM_TAU = 0.60
SVI_B = 0.60
SVI_RHO = -0.50
SVI_SIGMA = 1.50
CARRY = 0.028
RATE = 0.042


def atm_vol(t: float) -> float:
    t = max(t, 0.0)
    return ATM_LONG + (ATM_SHORT - ATM_LONG) * math.exp(-t / TERM_TAU)


def implied_vol(k: float, t: float) -> float:
    """Raw-SVI in standardised moneyness `k = ln(K/F)`; see fixture.rs."""
    t = max(t, 1.0 / 365.0 / 8.0)
    v = atm_vol(t)
    c = -k / (v * math.sqrt(t))
    factor = 1.0 + SVI_B * (-SVI_RHO * c + math.sqrt(c * c + SVI_SIGMA * SVI_SIGMA) - SVI_SIGMA)
    return min(max(v * math.sqrt(max(factor, 0.0)), 0.001), 5.0)


def norm_cdf(x: float) -> float:
    return 0.5 * math.erfc(-x / math.sqrt(2.0))


def theoretical(right: str, spot: float, strike: float, t: float) -> float:
    """Black-76 on the forward, discounted — `bsm::price` in the pricing crate."""
    if t <= 0.0:
        return max(spot - strike, 0.0) if right == "CALL" else max(strike - spot, 0.0)
    forward = spot * math.exp(CARRY * t)
    discount = math.exp(-RATE * t)
    vol = implied_vol(math.log(strike / forward), t)
    sd = vol * math.sqrt(t)
    d1 = (math.log(forward / strike) + 0.5 * sd * sd) / sd
    d2 = d1 - sd
    if right == "CALL":
        return discount * (forward * norm_cdf(d1) - strike * norm_cdf(d2))
    return discount * (strike * norm_cdf(-d2) - forward * norm_cdf(-d1))


def nickel(value: float) -> Decimal:
    """SPX options quote on a nickel grid."""
    return (Decimal(round(value * 20)) / Decimal(20)).quantize(Decimal("0.01"))


def cent(value: float) -> Decimal:
    """XSP's prices are a tenth of SPX's, so they land on a penny grid."""
    return (Decimal(round(value * 100)) / Decimal(100)).quantize(Decimal("0.01"))


def tos_tick(value: Decimal) -> Decimal:
    """A single-leg fill on the SPX simple-order tick — $0.05 below $3.00,
    $0.10 at or above, halves away from zero — the rule
    `wingspan_core::tos_format::round_tos_price` writes an order at. XSP's
    marks land on a penny grid (a tenth of SPX's), but its fills, like the
    ticket's orders, assume SPX's tick (unverified for XSP)."""
    step = Decimal("0.05") if value < 3 else Decimal("0.10")
    return ((value / step).quantize(Decimal(1), rounding=ROUND_HALF_UP) * step).quantize(Decimal("0.01"))


# `(product, base, ratio)`: `product` is priced as `base` at `ratio` x the
# strike, prices / `ratio` — `wingspan_core::contract::index_proxy`. XSP
# (Mini-SPX) is always PM-settled, root `XSP`.
INDEX_PROXIES: dict[str, int] = {"XSP": 10}


# --------------------------------------------------------------- the book


@dataclass(frozen=True)
class Contract:
    expiry: date
    strike: int
    right: str  # CALL | PUT
    underlying: str = "SPX"  # SPX (AM monthly, root SPX) | XSP (PM, root XSP)

    @property
    def ratio(self) -> int:
        """1 for SPX; 10 for XSP, which is priced as the SPX contract at ten
        times the strike, every price / 10 (`OptionContract::pricing_proxy`)."""
        return INDEX_PROXIES.get(self.underlying, 1)

    @property
    def pm(self) -> bool:
        # Every SPX contract in this book is an AM monthly; XSP is always PM.
        return self.underlying in INDEX_PROXIES

    @property
    def code(self) -> str:
        return f"{self.underlying}{self.expiry:%y%m%d}{self.right[0]}{self.strike}"

    @property
    def exp_text(self) -> str:
        return f"{self.expiry.day} {self.expiry:%b} {self.expiry:%y}".upper()

    def expiry_instant(self) -> datetime:
        # SPX monthlies settle AM: 09:30 New York on the expiry date; XSP at
        # the 16:00 close.
        return datetime.combine(self.expiry, time(16, 0) if self.pm else time(9, 30), tzinfo=NEW_YORK)

    def years_to_expiry(self, now: datetime) -> float:
        seconds = (self.expiry_instant() - now.astimezone(timezone.utc)).total_seconds()
        return max(seconds / (365.0 * 86400.0), 0.0)

    def mark(self, now: datetime, spot: float) -> Decimal:
        t = self.years_to_expiry(now)
        if self.ratio == 1:
            return nickel(theoretical(self.right, spot, float(self.strike), t))
        # The SPX spot, the SPX strike: the proxy's value, then / ratio.
        return cent(theoretical(self.right, spot, float(self.strike * self.ratio), t) / self.ratio)

    def fill(self, now: datetime, spot: float) -> Decimal:
        """What a leg fills at: its mark, which for SPX is already on the
        nickel grid; an XSP mark (a penny grid) snapped to `tos_tick`."""
        mark = self.mark(now, spot)
        return mark if self.ratio == 1 else tos_tick(mark)


@dataclass(frozen=True)
class Leg:
    contract: Contract
    qty: int  # signed


@dataclass(frozen=True)
class Event:
    """One filled order: what the trade file says, and what the statement shows."""

    at: datetime  # fill time, America/Chicago
    trade_file: str  # relative to the book root
    order_line: str  # the TOS order string as written in the trade file
    spread: str  # the statement's `Spread` word
    legs: tuple[Leg, ...]  # in the order TOS lists them
    net_price: Decimal
    pos_effect: str  # TO OPEN | TO CLOSE
    limit_price: Decimal | None = None  # order-history PRICE when it differs
    placed_seconds_before: int = 71


OCT = date(2026, 10, 16)
NOV = date(2026, 11, 20)
SEP = date(2026, 9, 18)
DEC = date(2026, 12, 18)

# Index level on each statement day: at the fill, and at the 15:00 CT close
# the Options section's marks are struck at. Invented; only the shape matters.
SPOTS: dict[date, tuple[float, float]] = {
    date(2026, 8, 10): (7690.0, 7702.0),
    date(2026, 8, 21): (7735.0, 7738.0),
    date(2026, 8, 24): (7760.0, 7764.0),
    date(2026, 8, 26): (7742.0, 7745.0),
}


def ct(day: date, hh: int, mm: int, ss: int) -> datetime:
    return datetime.combine(day, time(hh, mm, ss), tzinfo=CHICAGO)


EVENTS: tuple[Event, ...] = (
    Event(
        at=ct(date(2026, 8, 10), 9, 3, 12),
        trade_file="Closed Trades/Sep Put Spread Aug 10 2026 Closed",
        order_line="SELL -2 VERTICAL SPX 100 18 SEP 26 [AM] 7400/7350 PUT @9.00 LMT",
        spread="VERTICAL",
        legs=(Leg(Contract(SEP, 7400, "PUT"), -2), Leg(Contract(SEP, 7350, "PUT"), 2)),
        net_price=Decimal("9.00"),
        pos_effect="TO OPEN",
        limit_price=Decimal("8.90"),
    ),
    Event(
        at=ct(date(2026, 8, 21), 14, 22, 40),
        trade_file="Closed Trades/Sep Put Spread Aug 10 2026 Closed",
        order_line="BUY +2 VERTICAL SPX 100 18 SEP 26 [AM] 7400/7350 PUT @6.80 LMT # 2026-08-21",
        spread="VERTICAL",
        legs=(Leg(Contract(SEP, 7400, "PUT"), 2), Leg(Contract(SEP, 7350, "PUT"), -2)),
        net_price=Decimal("6.80"),
        pos_effect="TO CLOSE",
    ),
    Event(
        at=ct(date(2026, 8, 24), 10, 12, 35),
        trade_file="Open Trades/Oct Strangle Aug 24 2026",
        order_line="SELL -1 STRANGLE SPX 100 16 OCT 26 [AM] 8200/7200 CALL/PUT @114.20 LMT",
        spread="STRANGLE",
        legs=(Leg(Contract(OCT, 8200, "CALL"), -1), Leg(Contract(OCT, 7200, "PUT"), -1)),
        net_price=Decimal("114.20"),
        pos_effect="TO OPEN",
    ),
    Event(
        at=ct(date(2026, 8, 26), 13, 41, 8),
        trade_file="Open Trades/Nov Condor Aug 26 2026",
        order_line=(
            "SELL -2 IRON CONDOR SPX 100 20 NOV 26 [AM] 8400/8500/7000/6900 CALL/PUT "
            "@22.20 LMT"
        ),
        spread="IRON CONDOR",
        legs=(
            Leg(Contract(NOV, 8400, "CALL"), -2),
            Leg(Contract(NOV, 8500, "CALL"), 2),
            Leg(Contract(NOV, 7000, "PUT"), -2),
            Leg(Contract(NOV, 6900, "PUT"), 2),
        ),
        net_price=Decimal("22.20"),
        pos_effect="TO OPEN",
        limit_price=Decimal("22.10"),
    ),
    # The Mini-SPX hedge: ten XSP 650 puts are one SPX 6500 put's worth of
    # protection. 18 DEC 26 is a third Friday, so the order carries no
    # `(Weeklys)`; XSP is PM-settled, so it carries no `[AM]` either.
    Event(
        at=ct(date(2026, 8, 26), 14, 5, 17),
        trade_file="Open Trades/XSP Hedge Aug 26 2026",
        order_line="BUY +10 XSP 100 18 DEC 26 650 PUT @XSP_FILL LMT",
        spread="SINGLE",
        legs=(Leg(Contract(DEC, 650, "PUT", "XSP"), 10),),
        net_price=Decimal(0),  # XSP_FILL: filled in below from the pricer
        pos_effect="TO OPEN",
    ),
)


def _priced_singles(events: tuple[Event, ...]) -> tuple[Event, ...]:
    """A single-leg order fills at its own leg's fill price (`Contract.fill`):
    the `@…` and the net
    price of the XSP hedge come from the pricer rather than a literal, so the
    order line, the statement and legs.csv cannot disagree."""
    out = []
    for event in events:
        if event.spread == "SINGLE" and event.net_price == 0:
            leg = event.legs[0]
            fill = leg.contract.fill(event.at, SPOTS[event.at.date()][0])
            event = Event(
                at=event.at,
                trade_file=event.trade_file,
                order_line=event.order_line.replace("XSP_FILL", f"{fill:.2f}"),
                spread=event.spread,
                legs=event.legs,
                net_price=fill,
                pos_effect=event.pos_effect,
                limit_price=event.limit_price,
                placed_seconds_before=event.placed_seconds_before,
            )
        out.append(event)
    return tuple(out)


EVENTS = _priced_singles(EVENTS)


# --------------------------------------------------------------- formatting


def money(value: Decimal, dollars: bool = False) -> str:
    """`1,234.50` / `(1,234.50)`, with `$` when the section prints one; a
    value with a thousands separator is quoted like TOS/Excel do."""
    sign = value < 0
    body = f"{abs(value):,.2f}"
    text = f"${body}" if dollars else body
    text = f"({text})" if sign else text
    return f'"{text}"' if "," in body else text


def plain(value: Decimal) -> str:
    """Unquoted, signed, thousands-separated (Cash Balance AMOUNT/BALANCE)."""
    body = f"{abs(value):,.2f}"
    text = f"-{body}" if value < 0 else body
    return f'"{text}"' if "," in body else text


def fee_cell(value: Decimal) -> str:
    return "" if value == 0 else f"{-value:.2f}"


def total_fee(value: Decimal) -> str:
    return f"(${value:,.2f})"


def signed_qty(qty: int) -> str:
    return f"+{qty}" if qty > 0 else str(qty)


def side_word(qty: int, statement: bool = False) -> str:
    if statement:
        return "SOLD" if qty < 0 else "BOT"
    return "SELL" if qty < 0 else "BUY"


def order_qty(event: Event) -> int:
    """The signed order quantity (`SELL -2 …`): the first leg carries it."""
    return event.legs[0].qty


def statement_description(event: Event) -> str:
    """The Cash Balance dialect: `SOLD -1 … @114.20 CBOE` (no `LMT`, no hint)."""
    body = event.order_line.split(" #", 1)[0]
    words = body.split(" ")
    assert words[0] in ("BUY", "SELL"), body
    words[0] = "SOLD" if words[0] == "SELL" else "BOT"
    assert words[-1] == "LMT", body
    words[-1] = "CBOE"
    return " ".join(words)


def net_price_cell(event: Event, index: int) -> str:
    if index == 0:
        return f"{event.net_price:.2f}"
    if index == 1:
        return "CREDIT" if order_qty(event) < 0 else "DEBIT"
    return ""


def contracts_traded(event: Event) -> int:
    return sum(abs(leg.qty) for leg in event.legs)


# --------------------------------------------------------------- statements


@dataclass
class Position:
    qty: int = 0
    cost: Decimal = Decimal(0)  # signed premium paid, for the blended price

    def blended(self) -> str:
        """The `Trade Price` column: two decimals, three when the blend needs them."""
        if self.qty == 0:
            return "0.00"
        price = (self.cost / self.qty / 100).quantize(Decimal("0.001"))
        return f"{price:.2f}" if price == price.quantize(Decimal("0.01")) else f"{price:.3f}"


def statement_for(day: date, events: list[Event], refs: dict[Event, int], book: dict[Contract, Position], cash: Decimal) -> tuple[str, Decimal]:
    """Render one daily statement; returns the CSV text and the closing cash."""
    fill_spot, close_spot = SPOTS[day]
    header_day = day - timedelta(days=1)  # the real export's header lies by a day
    lines: list[str] = []
    out = lines.append

    out(f"﻿Account Statement for {ACCOUNT} (Individual) since {us(header_day)} through {us(header_day)}")
    out("")
    out("Cash Balance")
    out("DATE,TIME,TYPE,REF #,DESCRIPTION,Misc Fees,Commissions & Fees,AMOUNT,BALANCE")
    out(f"{us(day)},00:00:00,BAL,,Cash balance at the start of business day {day.day:02d}.{day.month:02d} CST,,,,{plain(cash)}")
    misc_total = comm_total = amount_total = Decimal(0)
    for event in events:
        n = contracts_traded(event)
        misc = MISC_FEE_PER_CONTRACT * n
        comm = COMMISSION_PER_CONTRACT * n
        amount = (-event.net_price * 100 * order_qty(event)).quantize(Decimal("0.01"))
        cash += amount - misc - comm
        misc_total += misc
        comm_total += comm
        amount_total += amount
        out(
            f'{us(day)},{event.at:%H:%M:%S},TRD,="{refs[event]}",{statement_description(event)},'
            f"{fee_cell(misc)},{fee_cell(comm)},{plain(amount)},{plain(cash)}"
        )
    out(f",,,,TOTAL,{total_fee(misc_total)},{total_fee(comm_total)},{money(amount_total, dollars=True)},{money(cash, dollars=True)}")
    out("")
    out("Futures Statements")
    out("Trade Date,Exec Date,Exec Time,Type,Ref #,Description,Misc Fees,Commissions & Fees,Amount,Balance")
    out(f"{us(day)},{us(day)},00:00:00,BAL,--,Futures cash balance at the start of business day {day.day:02d}.{day.month:02d} CST,--,--,--,0.00")
    out("")
    out(" ")
    out(f'"Total Cash {money(cash, dollars=True).strip(chr(34))}"')
    out("")
    out("")
    out("Forex Statements")
    out(",Date,Time,Type,Ref #,Description,Commissions & Fees,Amount,Amount(USD),Balance")
    out("")
    out('"Crypto # (Crypto offered by Charles Schwab Premier Bank, SSB) Statements"')
    out("Trade Date,Exec Date,Exec Time,Type,Ref #,Description,Commissions & Fees,Amount,Balance")
    out("")
    out("Account Order History")
    out("Notes,,Time Placed,Spread,Side,Qty,Pos Effect,Symbol,Exp,Strike,Type,PRICE,,TIF,Status")
    for event in reversed(events):
        placed = event.at - timedelta(seconds=event.placed_seconds_before)
        limit = event.limit_price if event.limit_price is not None else event.net_price
        for i, leg in enumerate(event.legs):
            c = leg.contract
            if i == 0:
                out(
                    f",,{us(placed.date())} {placed:%H:%M:%S},{event.spread},{side_word(leg.qty)},"
                    f"{signed_qty(leg.qty)},{event.pos_effect},{c.underlying},{c.exp_text},{c.strike},{c.right},"
                    f"{limit:.2f},LMT,DAY,FILLED"
                )
            else:
                price = "CREDIT" if (i == 1 and order_qty(event) < 0) else ("DEBIT" if i == 1 else "")
                out(f",,,,{side_word(leg.qty)},{signed_qty(leg.qty)},{event.pos_effect},{c.underlying},{c.exp_text},{c.strike},{c.right},{price},,,")
    out("")
    out("Account Trade History")
    out("Exec Time,Spread,Side,Qty,Symbol,Exp,Strike,Type,Price,Net Price,Opra")
    for event in reversed(events):
        for i, leg in enumerate(event.legs):
            c = leg.contract
            fill = c.fill(event.at, fill_spot)
            when = f"{us(day)} {event.at:%H:%M:%S}" if i == 0 else ""
            spread = event.spread if i == 0 else ""
            out(
                f"{when},{spread},{side_word(leg.qty)},{signed_qty(leg.qty)},{c.underlying},{c.exp_text},{c.strike},"
                f"{c.right},{fill:.2f},{net_price_cell(event, i)},{c.code}"
            )
            book.setdefault(c, Position())
            book[c].qty += leg.qty
            book[c].cost += fill * 100 * leg.qty
    out("")
    out("Equities")
    out("Symbol,Description,Qty,Trade Price,Mark,Mark Value")
    out(f"DUMMY,Dummy ETF Holding,+100,50.0000,50.00,{money(DUMMY_EQUITY_VALUE, dollars=True)}")
    out(f",OVERALL TOTALS,,,,{money(DUMMY_EQUITY_VALUE, dollars=True)}")
    out("Options")
    out("Symbol,Option Code,Exp,Strike,Type,Qty,Trade Price,Mark,Mark Value")
    close = ct(day, 15, 0, 0)
    options_total = Decimal(0)
    for c in sorted((c for c, p in book.items() if p.qty != 0), key=lambda c: (c.expiry, c.right == "CALL", c.strike)):
        p = book[c]
        mark = c.mark(close, close_spot)
        value = (mark * 100 * p.qty).quantize(Decimal("0.01"))
        options_total += value
        out(f"{c.underlying},{c.code},{c.exp_text},{c.strike},{c.right},{signed_qty(p.qty)},{p.blended()},{mark:.2f},{money(value, dollars=True)}")
    out(f",OVERALL TOTALS,,,,,,,{money(options_total, dollars=True)}")
    out("")
    out("Profits and Losses")
    out("Symbol,Description,P/L Open,P/L %,P/L Day,P/L YTD,P/L Diff,Margin Req,Mark Value")
    out(f"DUMMY,Dummy ETF Holding,$50.00,+1.00%,$5.00,$50.00,$0.00,$100.00,{money(DUMMY_EQUITY_VALUE, dollars=True)}")
    out(f",OVERALL TOTALS,$50.00,+1.00%,$5.00,$50.00,$0.00,$100.00,{money(DUMMY_EQUITY_VALUE, dollars=True)}")
    out("Account Summary")
    net_liq = cash + DUMMY_EQUITY_VALUE + options_total
    buying_power = (net_liq - Decimal("40000.00")).quantize(Decimal("0.01"))
    out(f"Net Liquidating Value,{money(net_liq, dollars=True)}")
    for label in ("Stock Buying Power", "Option Buying Power", "Intraday Buying Power"):
        out(f"{label},{money(buying_power, dollars=True)}")
    fees_ytd = sum((MISC_FEE_PER_CONTRACT + COMMISSION_PER_CONTRACT) * contracts_traded(e) for e in EVENTS if e.at.date() <= day)
    out(f"Equity Commissions & Fees YTD,{money(Decimal(fees_ytd), dollars=True)}")
    out("Futures Commissions & Fees YTD,$0.00")
    out("Crypto Trading Fees YTD,N/A")
    out(f"Total Commissions & Fees YTD,{money(Decimal(fees_ytd), dollars=True)}")
    return "\r\n".join(lines) + "\r\n", cash


def us(day: date) -> str:
    return f"{day.month}/{day.day}/{day:%y}"


# --------------------------------------------------------------- legs.csv
# Mirrors `wingspan_core::legs::from_book`'s execution-tape replay cell by
# cell (`docs/DATA-FORMATS.md`'s 16-column header): a FIFO pairing of each
# trade's fills, oldest first, per contract, is enough to reproduce the same
# rows `wingspan-cli export-legs` would emit from this same demo book — the
# two are asserted byte-identical by `test_make_demo_book.py`.

LEGS_HEADER: tuple[str, ...] = (
    "#", "Trade", "Leg", "Side", "Qty", "Opened", "Open Price", "Closed",
    "Close Price", "Mark", "Status", "Fees", "Expiry", "OCC Symbol", "Structure", "Key",
)

SPREAD_ABBREV = {"VERTICAL": "VERT", "STRANGLE": "STRG", "IRON CONDOR": "IC"}

# `Open Trades/Oct Strangle Aug 24 2026` -> ("Oct Strangle", date(2026, 8, 24));
# a closed-trade file's trailing " Closed" is stripped along with the date.
_TRADE_FILE_RE = re.compile(r"^(?P<name>.+) (?P<mon>[A-Z][a-z]{2}) (?P<day>\d{1,2}) (?P<year>\d{4})(?: Closed)?$")
_MONTH_NUM = {m: i for i, m in enumerate(
    ("Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"), start=1
)}


def parse_trade_file(rel: str) -> tuple[str, date]:
    base = rel.split("/", 1)[1]
    m = _TRADE_FILE_RE.match(base)
    assert m, base
    return m.group("name"), date(int(m.group("year")), _MONTH_NUM[m.group("mon")], int(m.group("day")))


@dataclass
class TradeGroup:
    name: str
    open_date: date
    is_open: bool
    events: list[Event]


def trade_groups() -> list[TradeGroup]:
    """The book's trades, in `book.trades` order: open trades first by open
    date, then closed trades by open date — same as `legs::from_book`."""
    by_file: dict[str, TradeGroup] = {}
    for event in EVENTS:
        group = by_file.get(event.trade_file)
        if group is None:
            name, open_date = parse_trade_file(event.trade_file)
            group = TradeGroup(name=name, open_date=open_date, is_open=event.trade_file.startswith("Open Trades/"), events=[])
            by_file[event.trade_file] = group
        group.events.append(event)
    return sorted(by_file.values(), key=lambda g: (0 if g.is_open else 1, g.open_date))


def trade_fee_total(events: list[Event]) -> Decimal:
    return sum(((MISC_FEE_PER_CONTRACT + COMMISSION_PER_CONTRACT) * contracts_traded(e) for e in events), Decimal(0))


def order_render(event: Event) -> str:
    """One order's `Structure` fragment: `-2x IC 8400/8500/7000/6900 NOV26`,
    or `10x XSP 650P DEC26` for a single XSP order — a proxied product is
    named before its strikes (`position::one_order_summary`)."""
    first_qty = event.legs[0].qty
    sign = "-" if first_qty < 0 else ""
    strikes: list[int] = []
    for leg in event.legs:
        if not strikes or strikes[-1] != leg.contract.strike:
            strikes.append(leg.contract.strike)
    tag = f"{event.legs[0].contract.expiry:%b}{event.legs[0].contract.expiry:%y}".upper()
    joined = "/".join(str(s) for s in strikes)
    underlying = event.legs[0].contract.underlying
    product = "" if underlying == "SPX" else f"{underlying} "
    if event.spread == "SINGLE":
        return f"{sign}{abs(first_qty)}x {product}{joined}{event.legs[0].contract.right[0]} {tag}"
    return f"{sign}{abs(first_qty)}x {product}{SPREAD_ABBREV[event.spread]} {joined} {tag}"


def structure_for(events: list[Event]) -> str:
    return " · ".join(order_render(e) for e in events)


@dataclass
class OpenLot:
    qty: int  # signed
    opened_on: date
    open_price: Decimal
    instant: datetime  # naive Chicago wall clock, the opening fill's


@dataclass
class LegCsvRow:
    contract: Contract
    qty: int  # signed opening direction, also on a closed row
    opened_on: date
    open_price: Decimal
    closed_on: date | None
    close_price: Decimal | None
    status: str
    mark: Decimal | None
    instant: datetime


def replay_trade(events: list[Event]) -> list[LegCsvRow]:
    """FIFO-pair one trade's fills per contract, oldest first — the same
    replay `legs::from_book` does over the matched execution tape."""
    tape: list[tuple[datetime, Contract, int, Decimal]] = []
    for event in sorted(events, key=lambda e: e.at):
        fill_spot, _ = SPOTS[event.at.date()]
        for leg in event.legs:
            tape.append((event.at, leg.contract, leg.qty, leg.contract.fill(event.at, fill_spot)))

    open_lots: dict[Contract, deque[OpenLot]] = {}
    rows: list[LegCsvRow] = []
    for at, contract, qty, price in tape:
        remaining = qty
        queue = open_lots.setdefault(contract, deque())
        while remaining != 0 and queue and (queue[0].qty > 0) != (remaining > 0):
            lot = queue.popleft()
            closing = min(abs(remaining), abs(lot.qty))
            closed_signed = closing * (1 if lot.qty > 0 else -1)
            rows.append(LegCsvRow(
                contract=contract, qty=closed_signed, opened_on=lot.opened_on, open_price=lot.open_price,
                closed_on=at.date(), close_price=price, status="Closed", mark=None, instant=lot.instant,
            ))
            remaining -= closing * (1 if remaining > 0 else -1)
            lot_left = lot.qty - closed_signed
            if lot_left != 0:
                lot.qty = lot_left
                queue.appendleft(lot)
        if remaining != 0:
            queue.append(OpenLot(qty=remaining, opened_on=at.date(), open_price=price, instant=at.replace(tzinfo=None)))

    close_at = ct(date(2026, 8, 26), 15, 0, 0)
    _, last_close_spot = SPOTS[date(2026, 8, 26)]
    for contract, queue in open_lots.items():
        for lot in queue:
            rows.append(LegCsvRow(
                contract=contract, qty=lot.qty, opened_on=lot.opened_on, open_price=lot.open_price,
                closed_on=None, close_price=None, status="Open", mark=contract.mark(close_at, last_close_spot),
                instant=lot.instant,
            ))

    rows.sort(key=lambda r: (r.opened_on, (r.contract.expiry, r.contract.strike, r.contract.right), r.qty))
    return rows


def money_cell(value: Decimal | None) -> str:
    """`format!("{:.2}", v)` in the Rust exporter — no thousands separator,
    no `$`, unlike this script's own statement-formatting `money()`."""
    return "" if value is None else f"{value:.2f}"


def date_cell(value: date | None) -> str:
    return "" if value is None else f"{value:%Y-%m-%d}"


def legs_csv_rows() -> list[list[str]]:
    """The whole `legs.csv`, header included, cell-for-cell what
    `wingspan-cli export-legs` writes from this same demo book."""
    out: list[list[str]] = [list(LEGS_HEADER)]
    for index, group in enumerate(trade_groups(), start=1):
        fee_total = trade_fee_total(group.events)
        structure = structure_for(group.events)
        for row_index, row in enumerate(replay_trade(group.events)):
            side = "BUY" if row.qty > 0 else "SELL"
            fees = fee_total if row_index == 0 else Decimal(0)
            key = "|".join((f"{group.name} {group.open_date:%Y-%m-%d}", "", f"{row.instant:%Y-%m-%d %H:%M:%S}"))
            out.append([
                str(index), group.name, "", side, str(abs(row.qty)),
                date_cell(row.opened_on), money_cell(row.open_price),
                date_cell(row.closed_on), money_cell(row.close_price),
                money_cell(row.mark), row.status, money_cell(fees),
                date_cell(row.contract.expiry), row.contract.code, structure, key,
            ])
    return out


# --------------------------------------------------------------- driver

README = """\
# fixtures/demo

The bundled synthetic book behind **Load demo book** in the desktop app (and
`wingspan-devserver serve --demo` in the dev harness). It installs with the
`.deb` at `/usr/lib/wingspan/fixtures/demo`. **Every trade, fill, mark and
balance in here is invented** — generated by `scripts/make_demo_book.py` from
a made-up index path and the app's own synthetic volatility surface; none of
it comes from a brokerage account. Regenerate after editing the script:

    python3 scripts/make_demo_book.py

`scripts/test_make_demo_book.py` (run by `just redaction-guard` and CI) fails
when these files drift from what the script writes.

`legs.csv` is the Legs-table form of the same book — the one the app actually
loads — and is written twice over: by this script and by `wingspan-cli
export-legs fixtures/demo --out fixtures/demo/legs.csv`, byte for byte the
same file. The thinkorswim tree beside it feeds the test-only TOS parsers and
that exporter (`docs/DATA-FORMATS.md`).

The dated snapshot is valued on 2026-08-31 (`docs/CONFIG.md`):

- `Open Trades/Oct Strangle Aug 24 2026` — `SELL -1 STRANGLE … 16 OCT 26
  8200/7200 CALL/PUT @114.20`, a ~45-DTE short strangle.
- `Open Trades/Nov Condor Aug 26 2026` — `SELL -2 IRON CONDOR … 20 NOV 26
  8400/8500/7000/6900 CALL/PUT @22.20`, a 100-wide iron condor one expiry out.
- `Open Trades/XSP Hedge Aug 26 2026` — `BUY +10 XSP 100 18 DEC 26 650 PUT`,
  a Mini-SPX tail hedge. XSP is one tenth of SPX: the app prices each put as
  the SPXW 6500 put ÷ 10, so ten of them hedge like one SPX put.
- `Closed Trades/Sep Put Spread Aug 10 2026 Closed` — a 2-lot 7400/7350 put
  credit spread sold on 8/10 and bought back on 8/21; the closing order carries
  a `# 2026-08-21` hint because its fill sits outside the ±7-day match window.

The statements use the same thinkorswim dialect as `fixtures/trades/` (the
anonymised real data the tests run against): UTF-8 BOM, CRLF, `="ref"` Excel
refs (an obviously synthetic `9500000000001…` sequence), parenthesised
negatives, the stray "Total Cash" row, `DEBIT`/`CREDIT` continuation rows and
the `00000000TEST` placeholder account id the redaction guards look for.
"""


# The first line of the README this script writes, and the proof that a
# directory is one of its own builds. `--dest` is a path a human typed, and
# `build` replaces the whole directory, so it must never be pointed at a real
# trades folder by a typo or a mis-tabbed shell completion.
README_SIGNATURE = "# fixtures/demo\n"


class NotOurs(ValueError):
    """`dest` holds files this script did not write."""


def is_ours(dest: Path) -> bool:
    """Whether `build` may replace `dest`: missing, empty, or a previous
    build of this script (a `README.md` opening with `README_SIGNATURE`).
    A real trades folder has an `Open Trades/` and no such README, so it is
    refused rather than deleted."""
    if not dest.exists():
        return True
    if not dest.is_dir():
        return False
    if not any(dest.iterdir()):
        return True
    try:
        return (dest / "README.md").read_text(encoding="utf-8").startswith(README_SIGNATURE)
    except OSError:
        return False


def build(dest: Path) -> None:
    # Decided once, before anything is written or removed: the guard stands
    # ahead of the delete, so a directory cannot become "ours" by being
    # written into, and a real folder is refused with its contents intact.
    if not is_ours(dest):
        raise NotOurs(
            f"{dest} already holds files and is not a demo book this script wrote "
            f"(no README.md opening with {README_SIGNATURE.strip()!r}); it would be deleted "
            "and replaced, so point --dest at a new or empty directory (or at fixtures/demo)"
        )

    events_by_day: dict[date, list[Event]] = {}
    for event in EVENTS:
        events_by_day.setdefault(event.at.date(), []).append(event)
    refs = {event: REF_BASE + i + 1 for i, event in enumerate(sorted(EVENTS, key=lambda e: e.at))}

    # A private staging directory beside `dest` (rename is only atomic within
    # a filesystem), created fresh rather than reused - nothing pre-existing
    # is ever removed to make room for it.
    dest.parent.mkdir(parents=True, exist_ok=True)
    staging = Path(tempfile.mkdtemp(prefix=dest.name + ".staging-", dir=dest.parent))
    for sub in ("Open Trades", "Closed Trades", "Daily Trade Statement"):
        (staging / sub).mkdir(parents=True)

    files: dict[str, list[str]] = {}
    for event in EVENTS:
        files.setdefault(event.trade_file, []).append(event.order_line)
    for rel, lines in files.items():
        (staging / rel).write_text("\n".join(lines) + "\n", encoding="utf-8")

    book: dict[Contract, Position] = {}
    cash = OPENING_CASH
    for day in sorted(events_by_day):
        text, cash = statement_for(day, sorted(events_by_day[day], key=lambda e: e.at), refs, book, cash)
        (staging / "Daily Trade Statement" / f"{day:%Y-%m-%d}-AccountStatement.csv").write_bytes(text.encode("utf-8"))

    with (staging / "legs.csv").open("w", newline="", encoding="utf-8") as fh:
        csv.writer(fh, lineterminator="\n", quoting=csv.QUOTE_MINIMAL).writerows(legs_csv_rows())

    (staging / "README.md").write_text(README, encoding="utf-8")

    # Checked twice: once before any work, and again here, immediately ahead
    # of the only irreversible step - deleting is worth a second look.
    if not is_ours(dest):
        shutil.rmtree(staging)
        raise NotOurs(f"{dest} changed underneath this run and is no longer a demo book")
    if dest.exists():
        shutil.rmtree(dest)
    staging.rename(dest)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--dest", type=Path, default=DEFAULT_DEST)
    args = parser.parse_args(argv)
    try:
        build(args.dest)
    except NotOurs as exc:
        print(f"error: {exc}", file=sys.stderr)
        print("error: nothing was written or deleted", file=sys.stderr)
        return 1
    print(f"wrote the demo book to {args.dest}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
