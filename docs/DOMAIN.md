# Wingspan domain rules

What the data means, how a trade is assembled and priced, which provider
answers, and the hand-checked numbers a change must still reproduce. This is
the layer underneath the shipped app: Wingspan is a Tauri desktop application
for Ubuntu 26.04 — see `docs/ARCHITECTURE.md` (crates and the IPC boundary),
`docs/IPC.md` (the frozen contract) and `docs/DESKTOP.md` (install, run,
build). The full parsing grammar lives in `docs/DATA-FORMATS.md`; this
document is the *why* behind it.

## The inputs

The user trades SPX option structures (butterflies, calendars, diagonals,
hedges) in a Schwab account and tracks every leg of every trade on the
**Legs** tab of a Google Sheet (rebuilt from broker statements by separate
tooling). Wingspan reads that tab — over HTTPS from a link-shared sheet, or
from a CSV export of it. One row is one leg of one trade: the trade's name,
an instance ordinal, side, quantity, the OCC symbol, open/close dates and
prices, fees, and optional cross-check columns (`docs/DATA-FORMATS.md` has
the full contract).

- The same book also exists as thinkorswim (TOS) exports — `Open Trades/`,
  `Closed Trades/`, `Daily Trade Statement/` — but **those are now test
  fixtures only**, reachable through `wingspan-cli export-legs`, which
  turns such a folder into a `legs.csv` for the golden tests.

What the app is for:

1. Treat each trade in the Legs table as one trade; show every open trade with
   DIT, P&L, delta, gamma, theta, vega and weighted vega; let the user toggle
   trades in and out of the analysis set.
2. Show overall portfolio stats (all trades, and the selected subset).
3. Use **Schwab's API** for live data (the token path is configurable and the
   file belongs to other tooling), falling back to **yfinance** with a clear
   in-app notification.
4. Draw an **OptionNetExplorer (ONE)-style risk graph** for the selected
   trades only: the expiration line plus five T+x lines starting at T+0.
5. Ignore everything that is not an option trade (equity/ETF rebalances,
   futures/forex/crypto sections).

## Facts learned from the example data

These facts are about the thinkorswim exports, which are now test-only
inputs (`docs/DATA-FORMATS.md`); they still pin the parsers that
`export-legs` and the golden tests depend on.

These drive the design; every one of them is a case some test pins.

- TOS order grammar: `BUY +2 BUTTERFLY SPX 100 19 FEB 27 [AM] 7325/6975/6625
  PUT @22.10 LMT FIXED`. Seen: single (no strategy word), `VERTICAL`,
  `CALENDAR` (`16 APR 27 [AM]/19 FEB 27 [AM] 8200 CALL`), `DIAGONAL`
  (`19 MAR 27 [AM]/19 FEB 27 [AM] 8200/8250 CALL`), `(Weeklys)` root flag,
  `[AM]` settlement flag, signed qty (`SELL -1 VERTICAL …`), a trailing
  `CANCEL AT 8/19/26 14:01:19` clause before `FIXED`. Statement dialect uses
  `BOT`/`SOLD` and ends with the exchange (`CBOE`); order-history rows use
  `WLKLMT`, `MOC`.
- First expiry ↔ first strike ↔ the bought leg in every calendar/diagonal
  (`16 OCT 26/18 SEP 26 5450/5050 PUT` = buy OCT 5450P, sell SEP 5050P).
  Butterfly `A/B/C` = `+n/−2n/+n`.
- Trade files list orders **out of chronological order** and carry no dates;
  the file name carries the trade's open date, which can be a non-trading day
  (`M5 Aug 15 2026` is a Saturday; its fills are 8/14). Closed-trade
  `closed_on` must come from the last matched execution (`Delta 1 Aug 25 2026
  Closed` closed 8/28).
- Statement CSV: UTF-8 BOM, CRLF, sections identified by header names (not
  reliably by blank lines — there is a line with a single space and a stray
  `"Total Cash …"` row that moves between sections), Excel refs
  `="9000000000001"`, quoted thousands, parenthesised negatives, `$` prefixes,
  leading-dot decimals (`.725`), `--`/`N/A` sentinels, multi-row spreads with
  empty continuation `Exec Time`, a `RE #…` replace-order ref shifting columns
  in an *Order History* continuation row, `CANCELED` orders with identical
  legs, `TO OPEN`/`TO CLOSE` effects. Header line dates lie (`since 8/27/26
  through 8/27/26` in a file whose rows are all 8/28) — `as_of` comes from
  row/file dates. Times are **Central** (`business day 19.08 CST`, `15:00:00`
  MOC fills).
- Trade files carry **fill** prices that match *Account Trade History* `Net
  Price`; *Order History* `PRICE` is the limit and differs in 4 of 14 cases.
- The account nets across trades: `Delta 1` sold the FEB 6800 put that `M4`
  holds long (`TO CLOSE`, blended `Trade Price` 104.855 on 8/28). Per-trade
  lots are mandatory; reconciliation must sum all trades and compare
  **quantities only**.
- `BSH` opened a leg 7/14 (4.90) and closed it 8/19 (0.57) within the trade →
  realised −$433, plus unrealised on the surviving leg, fees $3.78 → total
  −$613.78. Trades carry realised + unrealised.
- `LPTA … Closed` contains six lines byte-identical to `M1`/`M2`/`M3` orders;
  the statements prove the account never held those extra contracts (7325P is
  +4, not +6). Duplicate claims must be **quarantined**, never netted. LPTA's
  truth is one weekly put round-trip (−$160 − $2.35 fees).
- `SPXW260821P5875` expires on a **third Friday** yet is PM/weekly —
  settlement and root must come from the OPRA code (or the `(Weeklys)`/`[AM]`
  tokens), never from the calendar.
- Statement `Options` rows include positions absent from any trade file (a JUL
  26 box) → *unassigned* warning, not an error. `Trades/` contains a
  `.DS_Store` → skip dotfiles.
- Verified externally: Yahoo serves the `^SPX` options chain (so yfinance can
  supply spot *and* chain, ~15-min delayed, no greeks); Schwab option quotes
  return bid/ask/mark/`volatility`/greeks/`theoreticalOptionValue`/
  `underlyingPrice`. tastytrade's REST `by-type` quotes return bid/ask/mid/
  mark/last as JSON *strings* and carry neither greeks nor an implied
  volatility — those live on the DXLink websocket, which wingspan does not
  speak.

## Domain model and assembly (`wingspan-core`)

Money and strikes are `rust_decimal::Decimal`; dates and zones are `chrono` +
`chrono-tz`.

- `OptionContract { underlying, root (SPX|SPXW|…), expiry, settlement: Am|Pm,
  strike, right, multiplier }` with `to_osi()` (`SPX   270219P07325000`, root
  space-padded to 6), `to_tos_code()`/`from_tos_code()` (`SPXW260821P5875`),
  `expiry_instant()` (Am → 09:30 ET, Pm → 16:00 ET on the expiry date).
  Settlement rules per underlying live in a small table — the extension point
  for other underlyings.
- `Strategy` registry — table-driven `LegTemplate`s: `Single [+1]`, `Vertical
  [+1,−1]`, `Calendar [+1 far, −1 near]`, `Diagonal [+1 far/k1, −1 near/k2]`,
  `Butterfly [+1,−2,+1]`, `Straddle`, `Strangle`, `Condor`, `IronCondor`,
  unbalanced `~BUTTERFLY` ratios and `BACKRATIO`. `SELL` flips signs once (qty
  is already signed in the string — no double negation); order qty multiplies.
  An unknown strategy is a `ParseError { file, line, token }`.
- `Order { raw, side, qty, strategy, legs: Vec<(OptionContract, i32)>,
  net_price, hint: Option<DateHint>, execution: Option<Execution>, status:
  Matched|Unmatched|Quarantined(reason) }`, `Execution { at: DateTime<Tz>,
  ref_no, fills: Vec<Fill{contract, qty, price}>, fees: Fees{commission,
  misc} }`.
- `Trade { id (slug), name, opened_on (file name), closed_on: Option (last
  execution), status: Open|Closed, source_path, orders }`.
- `TradePosition` (derived): netted `legs: Vec<PositionLeg { contract, qty,
  avg_cost: Option }>`, `net_cash_flow`, `fees`, `realized`,
  `closed_contracts`, `expired_legs`, `quarantined_orders`.
- `Warning` — `UnmatchedOrder`, `DateOutsideWindow`, `DuplicateExecutionClaim`,
  `ClosedTradeHasOpenLegs`, `ExpiredLegUnknownSettlement`,
  `UnassignedAccountPosition`, `ReconciliationMismatch`,
  `StatementSectionSkipped`, `StatementDateDisagreement`, `SettlementInferred`,
  and more. **Every warning is data** with a severity and a trade/contract
  anchor, shown in the UI; malformed input never panics.

Assembly, in order:

1. Index executions from all statements by `(strategy, canonical signed legs,
   net price)`; attach fees from the `Cash Balance` TRD row with the same
   ref/time; attach `TO OPEN`/`TO CLOSE` from order status; drop `CANCELED`.
2. For each trade (open first, then closed; ordered by `opened_on`), for each
   order: candidates are unclaimed executions with the same key; choose by
   explicit hint (a `#` comment, or the `CANCEL AT` timestamp), else the
   nearest `|exec_date − opened_on|` within ±7 days (`DateOutsideWindow`
   beyond). No candidate → if the execution exists but is claimed by another
   trade, **`Quarantined(DuplicateExecutionClaim)`: excluded from legs, cash
   flow and reconciliation**; otherwise `Unmatched` — the order is dated
   `opened_on`, fees 0, legs included, with a warning.
3. Root and settlement come from the matched OPRA code, falling back to the
   `(Weeklys)`/`[AM]` tokens; never from the calendar.
4. Net legs per contract → `TradePosition`. **P&L(total) = Σ open-leg mark
   value + Σ order cash flows − fees**; **P&L(open)** = Σ (mark − avg cost) ×
   qty × mult over open legs; realised = total − open. `P&L %` = total ÷ |net
   premium paid| for debit trades, ÷ credit received for credit trades (label
   "% of cost"; `—` when zero). **DIT** = calendar days from `opened_on` (the
   user's own designation) to today or `closed_on`; **DTE** = days to the front
   expiry of open legs. An expired leg takes its settlement value from provider
   history when available, else 0 plus a warning.
5. Reconciliation: the latest statement's `Options` **quantities** (the blended
   `Trade Price` is ignored — it is a cross-trade average) against Σ legs
   across all trades → per-contract `Match | QtyMismatch | Unassigned |
   Missing`. Per-trade attribution knowingly differs from broker/tax lots, and
   the UI says so.

## Pricing and the risk engine (`wingspan-pricing`)

- `bsm` — European price and greeks (SPX is European; exact under BSM).
  `iv::solve(mark, …)` is Newton plus bracketed bisection, with an intrinsic
  floor, bounds `[0.1%, 500%]` and guards for crossed or zero marks; a
  provider's own IV is used only as the seed and as a diagnostic. **All greeks
  and all curves use our solved IVs so the table and the graph always agree**
  (invariant: the T+0 curve at spot equals dashboard P&L).
- `forward` — a per-expiry forward calibrated by put-call parity from an ATM
  call/put pair (the market-data layer requests these "calibration contracts"
  alongside the book), falling back to the configured
  `risk_free_rate`/`dividend_yield`. This removes the r/q guesswork that would
  otherwise skew every IV.
- `greeks` — bump-and-reprice: delta and gamma per 1 index point, vega per 1.00
  vol point, **theta = reprice at T−1 calendar day** (ONE-style). Position
  greeks are Σ qty × mult × greek. The units are published to the UI as
  tooltips.
- `weighted_vega` — Σ leg vega × (REF_DTE / DTE_leg)^k, `k` = 0.5 by default;
  `REF_DTE` is the **front expiry of the position or selected set**, with
  fixed-days (e.g. 30) as the configurable alternative. The reference in use is
  always shown next to the column.
- `time` — ACT/365 year fraction to `expiry_instant()` with an intraday
  fraction; a `DayCount` trait so trading-day time can be added.
- `risk_graph::compute(legs, snapshot, spec) → RiskGraph`, with `spec = {
  price_range (default [min strike−5%, max strike+5%] ∪ spot±10%; absolute or
  %), points (241), dates: Vec<GraphDate = Today | Offset(days) |
  Expiry(date)>, vol_shift_global, vol_shift_by_expiry, spot_override,
  include_realized: bool (default true, ONE-style) }`. For each date and price:
  legs expired at that date take intrinsic value, the rest are BSM at their own
  IV (plus shifts), sticky-strike (a `VolDynamics` trait, `StickyStrike` today,
  `StickyDelta` a later impl). `pnl = Σ qty·mult·V + net_cash_flow − fees` (or
  open-only when `include_realized = false`). The default date preset is `[T+0,
  T+¼, T+½, T+¾, T+(DTE−1)]` of the DTE to the front expiry, plus
  `Expiry(front)`; any date is editable and later expiries are selectable.
  Output per date: the curve, breakevens, max profit/loss on the range, and the
  per-trade component curves — plus what-if greeks at `(price, date, vol)` for
  the what-if panel, ±1σ/2σ price bands from the front ATM IV, and
  probability-of-profit at expiry (lognormal).

## Providers (`wingspan-marketdata`)

Two capabilities, so partial providers compose:

```rust
#[async_trait] pub trait SpotSource        { async fn spot(&self, underlying) -> Result<UnderlyingQuote>; fn health(&self) -> Health; }
#[async_trait] pub trait OptionQuoteSource { async fn quotes(&self, contracts: &[OptionContract]) -> Result<Vec<OptionQuote>>; fn health(&self) -> Health; }
// OptionQuote { contract, bid, ask, last, mark, iv_hint, greeks_hint, as_of }; Health = Ready | Degraded(reason) | Unavailable(reason)
```

- **Schwab** (`SpotSource + OptionQuoteSource`) — `GET
  https://api.schwabapi.com/marketdata/v1/quotes?symbols=$SPX,SPX%20%20%20270219P07325000,…`
  (OSI symbols, chunked ≤ 50, 429 backoff, a request budget). The token file
  sits at `[schwab] token_path` (or behind the `token_config`/
  `token_config_key` indirection): JSON with `access_token`, `refresh_token`,
  `token_type`, `access_expires_at`, `refresh_created_at`, `account_number`,
  loaded through a small `TokenStore` adapter. **The token file is read-only to
  Wingspan**: it is produced, refreshed and owned by other applications, so the
  app receives only a path, opens it read-only, and never writes, creates,
  renames, truncates, chmods, locks, copies or backs up anything in that
  directory — there is no `TokenStore` write method. An access-token refresh
  (`POST /v1/oauth/token`, refresh grant, basic auth) is preceded by a re-read
  of the file, because a sibling application may already have refreshed it, and
  its result — access token, expiry, any rotated refresh token — is held
  **only in process memory**. The in-memory token is therefore ahead of the
  file from the first refresh onward, and a re-read is adopted only on real
  evidence the file moved forward (a later `refresh_created_at`, or a refresh
  token the file did not previously carry). `[schwab] refresh = true|false`
  decides only whether Wingspan may run the grant itself. Health reports the
  refresh token's age from `refresh_created_at`, with a hard warning from day 6
  of its 7-day life; a token file that is not `0600` is a warning naming the
  other owners, never an error and never a `chmod`. `account_number` is a
  secret — redacted from logs and from the resolved config. Interactive login
  is out of scope (`docs/SCHWAB.md`). Extension points: `/chains`, and Accounts
  & Trading `positions` for live reconciliation.
- **Tastytrade** (`SpotSource + OptionQuoteSource`) — `GET
  https://api.tastyworks.com/market-data/by-type?index=SPX&equity-option=SPXW%20%20260918P05900000&…`
  (repeated query keys, never comma-joined; the same 21-character OCC symbols
  `OptionContract::to_osi()` already builds, chunked ≤ 100 — tastytrade's own
  cap — with jittered 429 backoff and its own request budget). Both SPX and
  SPXW go under `equity-option`; only the cash index goes under `index`, bare.
  The dxfeed *streamer* spelling is DXLink's and is never constructed here.
  Auth is an **OAuth2 personal grant**: a `read`-scope client secret and a
  refresh token that tastytrade neither rotates nor expires, both read by
  *variable name* from the environment or `[tastytrade] env_file`, so **there
  is no token file** — nothing on disk to read and nothing to write. The
  15-minute access JWT lives only in process memory; the exchange draws from
  the same request budget as the quotes. A failed exchange arms a **cooldown**
  (60 s, doubling to 15 min, cleared by any success) because repeated failed
  logins earn an ~8-hour IP block during which requests time out rather than
  answering 401. An auth-shaped rejection jumps straight to the cap and marks
  the provider `Unavailable`; a token-endpoint *timeout* is treated as a
  possible block too, but takes the ordinary doubling ladder and leaves
  `auth_failed` clear, because silence is not proof of a bad credential.
  **REST carries no greeks and no implied volatility** — they
  live on the DXLink `Greeks` event — so `iv_hint`/`greeks_hint` are always
  `None` and `wingspan-pricing` solves both. `[tastytrade] sandbox` points at
  the certification host, which answers 502 for every `/market-data` call and
  is therefore permanently `Degraded`; `[tastytrade] delayed` is the user's own
  statement that the account is not entitled to real-time data, because nothing
  on the wire says so. **Not in the default chain**: it is off by default and
  reaches the ladder only when `[tastytrade] enabled = true` *and*
  `"tastytrade"` is named in `[market] spot_sources`/`option_sources`
  (`docs/TASTYTRADE.md`).
- **YFinance** (`SpotSource + OptionQuoteSource`) — `bridge/yfinance_bridge.py`
  (a pinned `yfinance` in a venv; `[yfinance] python` configurable) run as a
  subprocess with a JSON request on stdin; it returns `^SPX` spot and the
  requested contracts from the `^SPX` chain (bid/ask/last/IV). Timeouts and
  clear reasons (`python not found`, `yfinance missing`, `Yahoo error`); the
  ~15-minute delay is reported as `Degraded`.
- **CachedSnapshot** (both) — the last good snapshot, persisted to
  `<state_dir>/snapshot.json` and served with its age when everything live
  fails. It also seeds the service at boot so a cold start has a market before
  the first fetch returns; the seed is relabelled on the way in (both sources
  become `cache`, the same `cache: cached snapshot from … (N old)` reason the
  ladder would add is appended) so a stale seed can never show as `LIVE`
  should that first refresh fail. The seed keeps its stored `as_of` — the one
  snapshot whose `as_of` is not *now* — which is what makes its age honest,
  and the `cache` labels also stop it being written back to disk as fresh.
- **StatementIv** (`OptionQuoteSource`) — IVs calibrated from the latest
  statement's `Mark` column (with that day's spot from the cache or Yahoo
  history), held sticky-strike; combined with any `SpotSource` it yields an
  honest offline risk graph. *That day's* spot is load-bearing: the chain hands
  the live level to `observe_spot` before asking for quotes, so calibrating at
  today's level would make the sticky-strike reprice an identity and the index
  move would vanish. The app keeps a `date -> close` ledger
  (`<state_dir>/closes.json`, one entry per session, filed by the print's own
  session date) and refuses to solve the surface when it has no level for the
  statement's date — the rung goes dark with that sentence rather than
  guessing.
- **Fixture** (both) — a deterministic synthetic surface for tests, the demo
  book and Playwright.
- **FallbackChain** — a configured order per capability (`spot = ["schwab",
  "yfinance", "cache"]`, `options = ["schwab", "yfinance", "cache",
  "statement"]`; `"tastytrade"` is a legal name in either, named first to lead); `MarketSnapshot { as_of, spot_source, option_source,
  degraded: Vec<reason>, spot, quotes, forwards }` in an `arc_swap`, refreshed
  in the background (15 s in RTH, 5 min otherwise, configurable) and broadcast
  on change. Off hours, spot is the prior close and the banner says so. The
  source and the reasons ride on every snapshot so the banner is always
  truthful, and each quote's `as_of` drives a staleness chip on the P&L cell.
  The snapshot's own `as_of` is always *now*, even when every rung fell
  through to the cache, so it is a refresh time and never a data age: the
  wire adds `data_as_of` (the oldest of the spot and the quotes) and the
  status pill ages `STALE · cached …` and `OFFLINE · statement marks …` off
  that, so a three-day-old book cannot read as "cached 3s".

## Fixtures and privacy

The real statements contain the account number and the net-liq.
`fixtures/trades/` is an **anonymised** copy generated by
`scripts/anonymize_statements.py`: the account id becomes `00000000TEST`,
balances are rescaled, the equities and P&L sections are replaced by a single
dummy row, the options sections are kept verbatim, and broker execution refs
are renumbered into a synthetic `9000000000001…` sequence. The example folder
itself is never committed. `fixtures/demo/` is a second, wholly synthetic book
in the same dialect.

`fixtures/trades/legs.csv` and `fixtures/demo/legs.csv` are the Legs-table
forms of those two books, generated by `wingspan-cli export-legs` from the
matched **execution tape** (oldest fill first, FIFO-paired per contract),
and `fixtures/sheets/legs-sample.csv` is a small anonymised Legs fixture
covering the parser's edge cases. None of them carries a real spreadsheet
id, a real gid or a real account number — the redaction guard scans them.

## Acceptance numbers

The hand-checked oracle. `wingspan-cli validate [SOURCE] [--tab NAME]
[--as-of YYYY-MM-DD] [--legs] [--all-warnings]` prints these, and the core
tests pin them. `SOURCE` is a Legs CSV path, a Google Sheet link or bare id,
a TOS trades folder (fixture tooling only, the same way `export-legs` is —
it is what keeps the numbers below reproducible on the folder path), or
omitted — in which case `validate` reads the source the app itself is
using (`<state_dir>/trade_source.json`, else `[trades] sheet_url`, else
`[trades] csv_path`) and refuses with one sentence when there is none.
There is deliberately no `--csv` flag: `risk --csv` already means
machine-readable output, and a bare positional argument does the job for
either shape of source. On a Legs book, `validate`'s marks come from the
sheet's own `Mark` column (a cross-check input, never pricing); on a
folder book, from the newest statement:

- **6 open / 2 closed** trades in `fixtures/trades`.
- **BSH** — a Sep put bought 7/14 and closed 8/19 inside the trade, leaving
  the Oct leg: total **−$613.78**, of which **−$433** is realised, fees $3.78.
- **M4** = a `+2/−4/+2` FEB butterfly plus an APR/FEB diagonal.
- **Delta 1** = **+$135.20**.
- **LPTA** — one weekly put round trip, **−$160** plus **−$2.35** fees
  (**−$162.35** total), with its six byte-identical duplicate orders
  quarantined rather than netted.
- An **unassigned** JUL 26 box warning from the statement's `Options` section.
- Quantity reconciliation matches the 2026-08-28 statement exactly.

Every number above is reproduced **twice**, once through the TOS folder
path in the core golden tests and once through `fixtures/trades/legs.csv`
on the Legs path, and a test asserts the two agree.

Pricing has its own oracles: textbook BSM values, `proptest` parity and
monotonicity, IV round-trip, bumped against analytic greeks, forward
calibration, weighted vega under both references, the curve invariants (T+0 at
spot equals P&L; the expiry curve is piecewise-linear only when all legs expire
that date; long-vega curves rise with a vol shift), and a golden CSV of the six
curves for the fixture snapshot.

## Decisions still cited in code

Five labelled decisions are referenced by name from source, tests, the
justfile and the release checklist. This is what they mean.

- **D23 — Clear stops showing the trades; it deletes nothing of yours.**
  Clearing forgets the pointer in `<state_dir>/trade_source.json` (mode
  `0600`, putting that source at the front of `recent`), deletes Wingspan's own cached copy
  of the fetched bytes (`trade_source_cache.csv` and
  `trade_source_cache.json`, both mode `0600`) and drops the cached market
  snapshot, whose quote universe was that book's own contracts. **No byte
  of your sheet, and no byte of any CSV file you imported, is read, moved,
  renamed or removed.** Cited by `web/src/components/ClearTradesControl.tsx`,
  its test, and `web/e2e/clear-trades.spec.ts`.
- **R2 — never recursively delete a user path.** `remove_dir_all` is banned
  outright across the application code by `clippy.toml`'s `disallowed-methods`,
  so a future refactor cannot reintroduce one by accident. Cited by
  `clippy.toml`, `crates/wingspan-cli/src/dump_contract.rs` (whose `--out`
  deletes only the `*.json` a previous dump left behind),
  `crates/wingspan-cli/tests/cli.rs` and `crates/wingspan-app/tests/trade_source.rs`.
- **R6 — the risk graph must stay responsive under a drag.** Dragging the
  price-range or vol slider re-solves and redraws the whole curve set, and that
  has to feel immediate. It is benchmarked by hand on a real display; the
  release checklist in `docs/DESKTOP.md` carries it as an owed item, because a
  headless box cannot measure it.
- **R8 — no build of Wingspan opens a remote-inspector socket.**
  `WEBKIT_INSPECTOR_SERVER` and `WEBKIT_INSPECTOR_HTTP_SERVER` are cleared
  before WebKitGTK reads them, with the single exception of
  `TAURI_WEBVIEW_AUTOMATION=true`, without which the WebDriver smoke cannot
  drive the shipped binary. Cited by `web/src-tauri/src/main.rs`; the rule and
  its exception are spelled out in `docs/DESKTOP.md` §13.
- **R11 — nothing from `fixtures/trades` reaches the installer.** The
  anonymised book is for tests only; the `.deb` carries the synthetic
  `fixtures/demo` and nothing else. Enforced as a content grep over the
  unpacked package, byte for byte the same in `just deb-gates` and CI's
  `desktop` job. Cited by the `justfile`.

## Extension points

`docs/ARCHITECTURE.md` has the table: a strategy is one row in the leg-template
table, a market-data provider is one trait impl plus a config entry in the
fallback chain, a statistic is one function plus a column definition, vol
dynamics is one `VolDynamics` impl, an underlying is one row in the
settlement-rules table, a graph line type is one `GraphDate` variant plus a
legend entry, and a trade source (e.g. a broker API) is one impl of
`TradeSource` plus one `SourceRef`/`SourceKind` variant.
