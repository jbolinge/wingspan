# Data formats

Wingspan's input is the **Legs table** — a Google-Sheet tab or a CSV with
the same columns — and this document is the parsing spec for it.

The three thinkorswim (TOS) formats documented below are **no longer read
by the application**. Their parsers stay in `wingspan-core` as test-only
code, exercised by `fixtures/trades/` (anonymised) with two named
exceptions: ratio-prefixed butterflies (`1/3/2 ~BUTTERFLY`) and
`BACKRATIO` are covered only by unit tests in `strategy.rs`/`tos_order.rs`,
not by any fixture or assembly/API integration test — and reached from
`wingspan-cli export-legs`, which turns a TOS folder into a `legs.csv` so
the hand-checked numbers in `docs/DOMAIN.md` keep an independent witness
of the Legs path.

The order grammar (§[Order grammar](#order-grammar)) now also runs the
other way: the Model tab's **Orders** block *writes* it. `wingspan_core::
tos_format` groups the modeled legs into the spreads TOS names and renders
each as an order string the trader pastes into thinkorswim
(§[Writing orders](#writing-orders-tos_format)). The parser is still
test-only, but it is now also the formatter's oracle: every line the
formatter writes is parsed back and must return the same legs.

`fixtures/demo/` is a second, wholly synthetic book in the same shape — the
one **Load demo book** shows (and `wingspan-devserver serve --demo` in the
dev harness) — now `fixtures/demo/legs.csv`, emitted by both
`scripts/make_demo_book.py` and `export-legs` (a test asserts they agree)
and loaded through the real CSV path. It is the only fixture with an `IRON
CONDOR` or a `STRANGLE`. The TOS tree that generates it stays on disk and
in the `.deb` for the test-only parsers below.

The fixtures are a **dated** snapshot: `fixtures/trades`' statements end
2026-08-28, `fixtures/demo`'s 2026-08-26, and their expiries follow. The demo
book is therefore valued on 2026-08-31 rather than on the host's today
(`docs/CONFIG.md`), so the dates in the examples below stay the dates the app
shows.

## The Legs table

**Where it comes from.** A Google Sheet shared anyone-with-link → Viewer,
fetched by Rust as
`https://docs.google.com/spreadsheets/d/<id>/gviz/tq?tqx=out:csv&sheet=<tab>`.
When the pasted link carried a gid (`…#gid=123456789`), that gid is kept
with the source and tried once as a fallback — `…/gviz/tq?tqx=out:csv&gid=123456789`
— if the tab name misses, so a tab renamed after the import still resolves.
A link pasted without one has no fallback: nothing infers a gid later,
because a gviz CSV response does not carry one. A published link uses
`/d/e/<pubid>/pub?output=csv&gid=123456789` instead, and **cannot select a
tab by name at all** — with no gid in the link it serves the first tab,
whatever `[trades] tab` says, and the probe's `note` says so.
Accepted user input: an `/edit` link, an export/gviz URL, a published
link, or a bare id — Rust extracts the id and builds the URL itself, so no
user-typed string is ever fetched verbatim. Only `docs.google.com` is ever
requested, and a redirect is followed only back to that same host or to
`accounts.google.com` (the signal a sheet is not shared). Or the same table
as a local CSV file (Google Sheets' own File → Download → CSV of that one
tab); in the desktop app that path has to come from the **Pick a CSV file…**
chooser (`docs/IPC.md`).

**Header matching.** Strip a UTF-8 BOM from the first field; scan the
first five records for the one that resolves every required column
(`sheet_header_offset`, info, when the header is below row 1); column
order is irrelevant; header names are normalised by lowercasing, keeping
ASCII letters, digits and `#`, collapsing every other run of characters to
a single space, and trimming — which is why `#` normalises to `#` rather
than the empty string, and `trade #` does not collide with `trade`. A
duplicated logical column is `sheet_duplicate_column` (info) and the first
wins.

**Required columns** — a missing one **refuses the whole import** with
`sheet_column_missing` and reads zero rows; a blank required cell skips
that one row with `sheet_row_skipped`:

| logical | aliases | why |
|---|---|---|
| `trade` | trade, trade name, name, trade id | groups rows and becomes the trade's slug |
| `side` | side, b s, buy sell, action | Qty is unsigned |
| `qty` | qty, quantity, contracts, size | leg magnitude |
| `occ` | occ symbol, occ, symbol, tos code, tos symbol, option symbol | the contract |

**Expected columns** — absent (or entirely blank) costs one
`sheet_column_absent` and the import is still accepted:

| logical | aliases | absent means |
|---|---|---|
| `opened` | opened, open date, entry date, date opened | falls back to the `Key` column's date, else the row's `closed`, else the load's as-of date |
| `open_price` | open price, entry price, avg open | every leg gets `avg_cost: null` and a cost basis of 0 |

**Optional columns**: `instance_no` (#, no, trade no, trade #); `key`;
`closed` (closed, close date, exit date); `close_price` (close price, exit
price) — a `Closed` date with no close price removes the leg from the
position, banks nothing, and warns `sheet_close_price_missing`; `status`
(cross-check only — a leg is closed iff it has both `closed` and
`close_price`); `fees` (fees, commissions); `mark` (**cross-check and
tests only — marks are never used for pricing**, live providers only);
`structure`/`strategy` (first non-empty value per trade wins; absent means
Wingspan summarises the legs itself, strikes and expiries, with no
strategy name); `expiry` (cross-check; the OCC code wins); `leg`/
`description` (a human label); `pnl`/`p l` (cross-check). `Days`, `Band`
and every unrecognised column are ignored silently.

**Value parsing.** Money goes through the same cleaner the statement
parser uses, so `(12,196.99)`, `$`, thousands separators and the
`--`/`N/A` sentinels all work. Dates accept `%m/%d/%Y`, `%Y-%m-%d`,
`%m/%d/%y` and `%b %d %Y` (Sheets exports a column in its *display*
format, so `7/14/2026` with no leading zeros is the common case). Side:
`buy|b|bot|bought|bto|btc` is a buy, `sell|s|sld|sold|sto|stc` a sell;
anything else skips the row. Qty is unsigned and `0` skips the row; a
signed cell that disagrees with Side warns (`sheet_value_disagrees`) and
**Side wins**. `Key` is split on `|`: field 0 is `rsplit_once(whitespace)`
into (name, ISO date) — names contain spaces, e.g. `Delta 1` — and field 2
parses as `%Y-%m-%d %H:%M:%S` for the event instant. `OCC Symbol` is the
compact TOS code (`SPX270219P6625`, `SPXW260821P5875`), with a fixed-width
21-character OSI string accepted as a fallback; the root and settlement
come from the code, never from the calendar, and a symbol whose root has
no settlement rule (an MES futures row, say) skips with "not an SPX
option".

**Trade identity.** `#` — the Trades-tab ordinal, repeated on every leg
row of that trade — groups rows when present; otherwise the ISO date
inside `Key`; with neither, every row sharing a name is one trade plus
`sheet_trade_instance_unverified`. There are **no gap heuristics**: a
trade that rolls weeks later is still one trade. A trade's open date is
the `Key` date if there is one, else the earliest `Opened`; it is closed
only when every one of its legs is.

**How rows become a position.** One row becomes one opening leg event
and, when it carries both a close date and a close price, one closing
event. Events replay through the same cost book the folder path used,
sorted by `(date, phase, instant, row)` with **opens before closes within
a date regardless of clock time** — otherwise a same-day open-and-close
replays close-first and halves the cost basis. A row with a blank `Open
Price` contributes quantity and zero basis.

**The P&L cross-check.** The sheet's own P&L is post-fee; Wingspan's
`realized` stays pre-fee. A closed leg should satisfy `(close − open) ×
|qty| × 100 × sign − fees`, an open leg `(mark − open) × |qty| × 100 ×
sign − fees`; more than $0.01 apart is `sheet_pnl_disagreement` (info),
never a refusal.

**Example header** — the column order `wingspan-cli export-legs` writes,
which is also what `fixtures/trades/legs.csv` and `fixtures/demo/legs.csv`
carry:

```
#,Trade,Leg,Side,Qty,Opened,Open Price,Closed,Close Price,Mark,Status,Fees,Expiry,OCC Symbol,Structure,Key
```

A tracker's own Legs tab typically carries more than this (`P/L`, `Days`,
`Band`); the extra columns are cross-checks or silently ignored, and the
table above is the contract.

**Warnings.** Warnings are data with a severity and an anchor; malformed
input never panics.

| severity | code | when |
|---|---|---|
| `error` | `sheet_column_missing` | a required column (`Trade`, `Side`, `Qty`, `OCC Symbol`) is not in the header — the import is refused and no rows are read |
| `warn` | `sheet_column_absent` | an expected column (`Opened`, `Open Price`) is missing or entirely blank — the import is accepted, without cost basis or DIT |
| `warn` | `sheet_row_skipped` | one row could not be used: a blank required cell, `Qty: 0`, an unreadable side, or an `OCC Symbol` that is not an SPX option |
| `warn` | `sheet_leg_unpriced` | a leg with no `Open Price` — quantity only, zero cost basis |
| `warn` | `sheet_close_price_missing` | a row with `Closed` but no `Close Price` — the leg leaves the position and banks nothing |
| `warn` | `sheet_trade_instance_unverified` | a trade name repeated with neither `#` nor `Key` to separate the instances — every row sharing the name becomes one trade |
| `info` | `sheet_value_disagrees` | a cross-check column (`Expiry`, `Status`, a signed `Qty`) disagrees with what Wingspan derived; the derived value wins |
| `info` | `sheet_pnl_disagreement` | the sheet's own P&L differs from the identity by more than $0.01 |
| `info` | `sheet_duplicate_column` | the header names the same logical column twice; the first wins |
| `info` | `sheet_header_offset` | the header row was found below row 1 (a title block above it) |

## Folder layout (test-only)

Everything from here to the end of this document describes the
thinkorswim formats the shipped application **no longer reads**. The
parsers stay in `wingspan-core` as test-only code: they back the golden
tests in `crates/wingspan-core/tests/` and the `wingspan-cli export-legs`
fixture tool, which converts a TOS folder into a `legs.csv` the Legs path
can load, so the acceptance numbers in `docs/DOMAIN.md` are reproduced
twice by two independent routes. No user ever points the app at one of
these folders; there is no migration path and none is needed.

```
<root>/
├── Open Trades/<Name> <Month> <Day> <Year>
├── Closed Trades/<Name> <Month> <Day> <Year> Closed
└── Daily Trade Statement/YYYY-MM-DD-AccountStatement.csv
```

Discovery is case-insensitive, descends one directory level if the three
folders are not at the root (so pointing at the parent works), and the three
names are overridable in `[trades]` config. Dotfiles (`.DS_Store`) are skipped.

## Trade files (test-only)

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

### Writing orders (`tos_format`)

The Model tab's Orders block writes the same grammar, and is the one part of
it the shipped app uses. `tos_format::group` nets the modeled legs per
contract, then peels off named spreads in priority order — `IRON CONDOR`,
`BUTTERFLY`, `CONDOR`, `VERTICAL`, `CALENDAR`, `DIAGONAL`, `STRADDLE`,
`STRANGLE` — as many whole units at a time as every leg allows; whatever is
left is written as singles. It also tries `BUTTERFLY` and `CONDOR` before
`IRON CONDOR` (an iron condor can be cut out of the middle of a fly: a call
fly beside a put fly is also two iron condors) and keeps whichever way gives
fewer orders, the flies on a tie. An unbalanced shape is decomposed rather than
written as a ratio order (`+1/−3/+2` is a `BUTTERFLY` plus a `VERTICAL`;
`~BUTTERFLY` and `BACKRATIO` are never written). `tos_format::render`
writes each order:

```
SELL -2 IRON CONDOR SPX 100 20 NOV 26 [AM] 8400/8500/7000/6900 CALL/PUT @22.20 LMT
BUY +1 VERTICAL SPX 100 (Weeklys) 21 AUG 26 5875/5825 PUT @1.55 LMT
BUY +1 SPX 100 18 SEP 26 [AM] 5050 PUT @4.90 LMT
```

- The first strike is the nearer-the-money one — calls ascending, puts
  descending — and calendars/diagonals put the far expiry first, which is
  what TOS prints and makes `BUY` the debit form of every spread (long the
  nearer strike, the wings, the far expiry, the straddle).
- `SPX` and `SPXW` legs never share an order: `(Weeklys)` is per order and
  the parser applies it to every leg. A root the grammar cannot spell (not
  the underlying's own AM root nor its weekly PM root) is written as
  singles and would not parse back to the same root.
- `[AM]` is always written for an AM-settled expiry, so the parser never
  has to infer it (the hand-written example above it omits the flag); the
  day is not zero-padded (`5 MAR 27`); the tail is just `LMT`, with no TIF,
  `FIXED` or exchange.
- `@price` is the per-unit net mark as TOS writes it (side-relative, so a
  sold credit spread is positive and a diagonal bought for a credit
  negative), to the cent for spreads and on the SPX simple-order tick
  ($0.05 below $3.00, $0.10 above) for a single leg.

`STRADDLE`, `STRANGLE`, `CONDOR` and `IRON CONDOR` — the **Documented**
rows of the strategy registry — are now exercised by the formatter's
round-trip tests (a property test over random leg bags included), which
prove the formatter and the parser agree with each other. They remain
unverified against a real TOS export: if thinkorswim ever prints one of
these spreads differently, the registry row and the formatter change
together.

## Account statements (test-only)

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
- This is shared by both paths: the Legs table's `OCC Symbol` column
  parses into the same `OptionContract`.

## Execution matching & quarantine (test-only)

Executions from all statements are indexed by `(strategy, canonical signed
legs, net price)` and claimed by trades (open first, then closed, ordered by
open date): explicit hint first, else nearest `|exec_date − opened_on|`
within ±7 days (`DateOutsideWindow` beyond). An order whose execution is
already claimed by another trade is **quarantined**
(`DuplicateExecutionClaim`) — excluded from legs, cash flow and
reconciliation, and surfaced in the UI with a fix hint (`# date` / `# ref=`
comments). An order with no execution at all is *unmatched*: dated
`opened_on`, zero fees, legs included, warning shown.
