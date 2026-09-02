# Data formats

Wingspan reads three kinds of input, all produced by thinkorswim (TOS). This
document is the parsing spec; `crates/wingspan-core` implements it and
`fixtures/trades/` (anonymised) exercises every case named here, with two
named exceptions: ratio-prefixed butterflies (`1/3/2 ~BUTTERFLY`) and
`BACKRATIO` are covered only by unit tests in `strategy.rs`/`tos_order.rs`,
not by any fixture or assembly/API integration test.

`fixtures/demo/` is a second, wholly synthetic book in the same dialect — the
one `serve --demo` shows — written by `scripts/make_demo_book.py`; it is the
only fixture with an `IRON CONDOR` or a `STRANGLE`.

The fixtures are a **dated** snapshot: `fixtures/trades`' statements end
2026-08-28, `fixtures/demo`'s 2026-08-26, and their expiries follow.
`serve --demo` therefore values them on 2026-08-31
rather than on the host's today (`docs/CONFIG.md`), so the dates in the
examples below stay the dates the app shows.

## Folder layout

```
<root>/
├── Open Trades/<Name> <Month> <Day> <Year>
├── Closed Trades/<Name> <Month> <Day> <Year> Closed
└── Daily Trade Statement/YYYY-MM-DD-AccountStatement.csv
```

Discovery is case-insensitive, descends one directory level if the three
folders are not at the root (so pointing at the parent works), and the three
names are overridable in `[trades]` config. Dotfiles (`.DS_Store`) are skipped.

## Trade files

One file per trade; one TOS order string per line; blank lines and lines
starting with `#` are ignored. Orders may appear **out of chronological
order** and carry **no dates** — the file name carries the trade's *open*
date (the user's designation; it may be a non-trading day, e.g. a Saturday
whose fills are the prior Friday). `Closed` files keep the *open* date in the
name; `closed_on` is derived from the last matched execution.

File-name grammar (parsed right-to-left): `<name…> <Month(full|abbrev)>
<d{1,2}> <yyyy> [Closed]` — everything before the date is the name, spaces
allowed (`Delta 1`).

### Order grammar

```
BUY +2 BUTTERFLY SPX 100 19 FEB 27 [AM] 7325/6975/6625 PUT @22.10 LMT FIXED
SELL -1 VERTICAL SPX 100 (Weeklys) 21 AUG 26 5875/5825 PUT @1.55 LMT
BUY +1 CALENDAR SPX 100 16 APR 27 [AM]/19 FEB 27 [AM] 8200 CALL @15.00 LMT
BUY +1 DIAGONAL SPX 100 19 MAR 27 [AM]/19 FEB 27 [AM] 8200/8250 CALL @-1.20 LMT
BUY +1 SPX 100 18 SEP 26 5050 PUT @4.90 LMT CANCEL AT 8/19/26 14:01:19 FIXED
SELL -1 STRANGLE SPX 100 16 OCT 26 [AM] 8200/7200 CALL/PUT @114.20 LMT
SELL -2 IRON CONDOR SPX 100 20 NOV 26 [AM] 8400/8500/7000/6900 CALL/PUT @22.20 LMT
```

- Side: `BUY|SELL` (order dialect) or `BOT|SOLD` (statement dialect).
- Quantity is **signed in the string**; `SELL` flips template signs **once**
  (no double negation), and the order quantity multiplies the template.
- Strategy word optional (single-leg orders have none). Known templates:
  `Single [+1]`, `VERTICAL [+1,−1]`, `CALENDAR [+1 far, −1 near]`,
  `DIAGONAL [+1 far/k1, −1 near/k2]`, `BUTTERFLY [+1,−2,+1]` (A/B/C strikes =
  +n/−2n/+n), `STRANGLE [+1 call, +1 put]` (`k_call/k_put CALL/PUT`),
  `IRON CONDOR [+1,−1,+1,−1]` over `k1/k2/k3/k4 CALL/PUT` — two verticals, each
  written long-strike-first like `VERTICAL`, so `SELL -1 IRON CONDOR …
  8400/8500/7000/6900 CALL/PUT` is short the 8400 call and 7000 put, long the
  8500 call and 6900 put. Ratio prefixes (`1/3/2 ~BUTTERFLY`) and `BACKRATIO`
  are documented extension rows. Unknown strategy →
  `ParseError{file,line,token}`.
- **First expiry ↔ first strike ↔ the bought leg** in every
  calendar/diagonal: `16 OCT 26/18 SEP 26 5450/5050 PUT` = buy OCT 5450P,
  sell SEP 5050P.
- `(Weeklys)` → root `SPXW`; `[AM]` after an expiry → AM settlement.
- `@price` may be negative (net-credit diagonals written as negative debit).
- Tolerant tail: order type (`LMT|MKT|WLKLMT|MOC`), an optional
  `CANCEL AT m/d/yy hh:mm:ss` clause (used as an execution **date hint**),
  `GTC|DAY`, `FIXED`, or a trailing exchange name (`CBOE`).
- Optional trailing `# …` comment: `# 2026-07-14` or `# ref=9000000000001` —
  an explicit matching hint, the user's escape hatch when two orders are
  otherwise indistinguishable.

## Account statements

`Daily Trade Statement/YYYY-MM-DD-AccountStatement.csv`, exported per day an
option order filled. They are the **only** source of per-leg fill prices,
execution times, commissions/fees and end-of-day marks.

Parsing hazards (all real, all tested):

- UTF-8 BOM; CRLF line endings.
- Sections are identified by their **header names** (`Cash Balance`,
  `Account Order History`, `Account Trade History`, `Options`,
  `Account Summary`) — *not* by blank lines: there is a line containing a
  single space, and a stray `"Total Cash …"` row that moves between sections.
  Unrecognised sections are skipped with a `StatementSectionSkipped` warning.
- Numeric forms: Excel refs `="9000000000001"`, quoted thousands (`"1,234.56"`),
  parenthesised negatives `(123.45)`, `$` prefixes, leading-dot decimals
  (`.725`), `--` and `N/A` sentinels.
- Multi-row spreads: continuation rows have an empty `Exec Time`; a `RE #…`
  replace-order reference **shifts columns** in Order History continuation
  rows — parse defensively.
- `CANCELED` orders may have legs identical to filled ones — dropped.
- `TO OPEN`/`TO CLOSE` position effects come from Order History.
- The header line's date range **lies** (`since 8/27/26 through 8/27/26` in a
  file whose rows are all 8/28): `as_of` comes from row dates cross-checked
  with the file name; disagreement → `StatementDateDisagreement` warning.
- All statement times are **Central** (`America/Chicago`, configurable):
  `business day 19.08 CST`, `15:00:00` MOC fills.
- Trade files carry **fill** prices matching *Account Trade History*
  `Net Price`; *Order History* `PRICE` is the limit price and may differ.
- The `Options` position section is **account-level** (netted across trades):
  one trade can sell a contract another holds long (`TO CLOSE`, blended
  `Trade Price`). Reconciliation therefore compares **quantities only** and
  the blended price is ignored. Positions absent from every trade file (e.g.
  a box spread) are an *unassigned* warning, not an error.
- Everything that is not an option row (equities/ETF rebalances,
  futures/forex/crypto sections) is ignored.

## Contract identity

- OSI: root space-padded to 6 + `yymmdd` + `C|P` + strike×1000 zero-padded to
  8: `SPX   270219P07325000`.
- TOS statement code: `SPXW260821P5875`.
- **Settlement/root come from the OPRA code or the `(Weeklys)`/`[AM]`
  tokens — never from the calendar**: `SPXW260821P5875` expires on a third
  Friday yet is PM-settled/weekly.
- Expiry instants: AM → 09:30 ET, PM → 16:00 ET on the expiry date
  (`America/New_York`).

## Execution matching & quarantine

Executions from all statements are indexed by `(strategy, canonical signed
legs, net price)` and claimed by trades (open first, then closed, ordered by
open date): explicit hint first, else nearest `|exec_date − opened_on|`
within ±7 days (`DateOutsideWindow` beyond). An order whose execution is
already claimed by another trade is **quarantined**
(`DuplicateExecutionClaim`) — excluded from legs, cash flow and
reconciliation, and surfaced in the UI with a fix hint (`# date` / `# ref=`
comments). An order with no execution at all is *unmatched*: dated
`opened_on`, zero fees, legs included, warning shown.
