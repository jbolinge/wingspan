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
  `expiry_instant()` (Am → 09:30 ET, Pm → the close: 16:00 ET, or 13:00 ET
  on an early-close day — the day after Thanksgiving (the Friday after the
  fourth Thursday of November), Dec 24 when it falls Monday–Thursday (a
  Friday Dec 24 is the observed Christmas holiday), Jul 3 when it and Jul 4
  are both weekdays). The early-close rule lives in `wingspan_core::calendar`,
  rule-based (no year list), and is the one source the settlement instants,
  the risk graph's `expiry:<date>` lines and the market-data session clock
  all read. It moves only the *instant*; settlement type still comes from
  OPRA codes/order tokens.
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
   value + Σ expired-leg settlement value + Σ order cash flows − fees**;
   **P&L(open)** = Σ (mark − avg cost) × qty × mult over **open** legs;
   realised = total − open + fees (closed lots plus every expired leg's
   `(settlement − avg cost) × qty × mult`, pre-fee). `P&L %` = total ÷ |net
   premium paid| for debit trades, ÷ credit received for credit trades (label
   "% of cost"; `—` when zero). **DIT** = calendar days from `opened_on` (the
   user's own designation) to today or `closed_on`; **DTE** = days to the front
   expiry of open legs.

   **"Today" is the New York calendar date** of the app's injected clock
   (`wingspan_core::book::market_date`), never the UTC date: from 00:00 UTC
   (19:00 CDT / 18:00 CST) a UTC "today" would count every leg expiring that
   day as expired and read every DTE one day short. A leg is expired when its
   expiry date is before today, so on its expiry day it stays an open leg —
   worth intrinsic once past its settlement instant, `DTE 0` — until New
   York's midnight. The book is assembled against that date when it is read
   and **re-read when New York's date rolls past it** (checked on every
   published market snapshot, so on the first refresh after midnight; a
   re-read that fails keeps the previous book and is retried on the next
   refresh). A pinned `--as-of`/demo date never rolls.

   **An expired leg still in the book is worth its settlement**, on the best
   evidence there is: a quote's mark for it, when the snapshot carries one
   (the quote universe holds open legs only, so in practice it does not); else
   intrinsic against the SPX close recorded for its expiry date in the close
   ledger (`closes.json`, the last level seen for that session), through the
   pricing proxy for XSP (`intrinsic(close, 10 K) ÷ 10`) — for a PM-settled
   contract that close *is* the settlement print, so nothing is warned; for an
   AM-settled one the official SET is not available and the close is an
   estimate; else intrinsic against the current SPX level. The two estimates
   raise `expired_leg_unknown_settlement` naming what the leg was valued
   against and asking for the settlement to be recorded as a Closed row in
   the Legs sheet; with no level at all the leg is worth 0 and the warning
   says that. **Once settled, the leg's result is banked**: it is in
   `realized` and `pnl_total`, never in `pnl_open`, and the risk graph and
   what-if carry it the same way — as cash on every include-realized line,
   flat across the price axis, and on no open-only line — so the T+0-at-spot
   invariant holds whatever the leg settled at.
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
- `surface` — one solved IV per quoted contract. A quote whose contract has
  already settled at the snapshot instant (`t = 0`: a PM contract after
  16:00 ET on its expiry day) is **not solved and not warned about** — its
  value is intrinsic, which needs no vol, and its quote stays in the snapshot
  for P&L. A contract with no solved point of its own (an unquoted strike,
  or an XSP leg whose SPXW proxy at 10K is not quoted or did not solve) takes,
  in order: (1) its own point; (2) its **own smile** — same expiry, root,
  settlement and right — with the IV **interpolated linearly in strike**
  between the two quoted strikes that bracket it (`IvSource::Interpolated`,
  warned `vol_interpolated`, info), or past the last quoted strike of a wing
  the nearest one flat; (3) the same on the opposite right; (4) the same date
  on another root/settlement (an SPX AM monthly for an SPXW PM contract on a
  third Friday), same right first; (5) the nearest expiry's ATM vol; else
  `fallback_iv`. Everything after step 2's interpolation is a neighbour or a
  fallback (`vol_fallback`, info). Linear-in-strike is deliberate: strikes
  that bracket a proxy are one grid step (5–25 points) apart, where it and
  total variance in log-strike differ by far less than a quote's spread.
  Each held, unsettled leg with no quote of its own (or of its proxy) is
  flagged `missing_quote` (warn) on its trade row and in Data health, beside
  its `vol_fallback`/`vol_interpolated`, and the row's `staleness_partial`
  says its staleness dates only the quoted legs.
- `forward` — a per-expiry forward calibrated by put-call parity from an ATM
  call/put pair (the market-data layer requests these "calibration contracts"
  alongside the book), falling back to the configured
  `risk_free_rate`/`dividend_yield`. This removes the r/q guesswork that would
  otherwise skew every IV. The pair is the one nearest spot among those
  still live (a pair past its settlement instant is skipped before choosing,
  so a settled SPX AM pair never displaces the SPXW PM pair on the same
  date), and on an exact tie the later (PM) settlement wins.
- `greeks` — bump-and-reprice: delta and gamma per 1 index point, vega per 1.00
  vol point, **theta = reprice at T−1 calendar day** (ONE-style). Position
  greeks are Σ qty × mult × greek. The units are published to the UI as
  tooltips.
- `weighted_vega` — Σ leg vega × (REF_DTE / DTE_leg)^k, `k` = 0.5 by default;
  `REF_DTE` is a **fixed 30 days by default** (`reference = "30d"`, or any
  `<n>d`), with the front expiry of the book or selected set (`front_expiry`)
  as the configurable alternative. Every per-trade row is measured against the
  whole book's reference, so the column sums to the All total; the Selected
  total resolves its own reference (in `front_expiry` mode, the selected set's
  front). The reference in use is always shown next to the column.
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
  The front is the **live** front — the earliest expiry with a leg whose
  settlement instant is still after `now` (falling back to the earliest
  expiry when every leg has settled) — and DTE is counted in New York
  calendar days, so `T+(DTE−1)` is the day before expiry. The sigma bands,
  `front_atm_iv` and the `front_expiry` weighted-vega reference use the same
  live front, measured to the *latest* settlement among the legs on that
  date (an SPX AM and an SPXW/XSP PM leg on one date give one answer in
  either order). `Expiry(D)` is priced at D's PM close whatever legs the
  request holds. With several expiration lines the earliest-dated one is the
  canonical `expiry` line: max profit/loss on the range, PoP and
  `breakevens["expiry"]` are all read from it. The price axis is `points`
  uniform samples plus spot and every proxy strike in range as exact nodes
  (strictly ascending, deduplicated within 1e-9), so a narrow fly's peak,
  its breakevens and the T+0 value at spot are exact rather than
  interpolated.
  Output per date: the curve, breakevens, max profit/loss on the range, and the
  per-trade component curves — plus what-if greeks at `(price, date, vol)` for
  the what-if panel, ±1σ/2σ price bands from the front ATM IV, and
  probability-of-profit at expiry (lognormal).
- **Model legs** (the Model tab) are hypothetical adjustments, never part of
  the book. The app layer (`wingspan-app`'s `service::model_position`) nets
  them per contract and hands the engine one more `Position` with the id
  `model`, so `risk_graph`, `what_if` and `portfolio` price them exactly like
  a trade. **A modeled leg's entry price is the current mark**: its
  `avg_cost` is the model's value of the contract now (`Valuation::value_now`
  — the quote's own mark when the snapshot quotes it, since the solved IV
  reproduces it; else the BSM value at a neighbouring strike's vol, flagged
  `missing_quote`), its `net_cash_flow` is `−Σ qty·mark·mult`, and it has no
  fees. Its P&L today at spot is therefore zero by construction, so the T+0
  invariant above survives any model; it changes the curve's shape only —
  away from spot, at later dates, under a vol shift. A contract is priced off
  its own quote once the SPA has registered it with `set_model_contracts`,
  which adds it to the snapshot's quote universe (book ∪ extras).
- `chain::chain_analytics` prices an option chain for the Model tab the same
  way: our IVs solved from the chain's quotes against the **book's** spot and
  calibrated forwards (never a forward of the chain's own), and per-contract
  greeks from the same `greeks::contract_greeks` as a `Leg` row — so a
  contract reads the same on the chain as in the positions table.

## XSP (Mini-SPX)

**An XSP contract is one tenth of an SPX contract.** XSP (Cboe Mini-SPX) is
always **PM-settled**, its OPRA root is `XSP` (there is no `XSPW`) and its
multiplier is 100, so `XSP261218P650` is `XSP` / PM / 650 put / 18 DEC 26 —
settlement from the root, as everywhere, never from the calendar. A Legs row
whose `OCC Symbol` is an XSP code is read like any other
(`docs/DATA-FORMATS.md`).

An XSP contract is **priced as its SPXW proxy**: the SPXW contract on the
same expiry and right at **ten times the strike** (`XSP 650 P` → `SPXW 6500
P`; a fractional `652.5` → `6525`), with every *price* — bid, ask, mark,
last, model value — **× 0.1** and the *vol* unchanged.
`OptionContract::pricing_proxy()` returns that pair (`(contract, 1.0)` for
SPX), and `wingspan_core::contract::index_proxy` is the one-row table it
reads (the extension point for another fractional index). BSM homogeneity
makes this exact: `price(S/10, K) = price(S, 10K)/10`. Because nothing
quotes XSP in this version, **no provider is ever asked for an `XSP…`
symbol**: the market service's quote universe (the book's and the model's)
holds proxies only, the snapshot holds only SPX quotes, and the app scales
the proxy's quote when it shows an XSP leg's bid/ask. A pricing warning on
that proxy quote (an IV that would not solve, or was clamped) is a warning on
the XSP leg too: its trade row and Data health list it under the XSP code,
`… (via SPXW261218P6500)`. When a statement-derived surface holds both an
SPXW contract's own mark and an XSP mark × 10 for it, the SPXW mark wins.

Greeks follow from bumping the SPX spot through the proxied value, so in the
engine's SPX-point units an XSP leg's greeks are exactly 0.1 × the proxy's —
right for net exposure: ten XSP puts hedge like one SPX put, in the
positions table, the portfolio cards, the risk graph (whose axis stays in
SPX points; an XSP strike never drags it to 650) and the model ticket. The
Model tab's chain can be switched to **XSP**, which shows the SPXW chain ÷ 10
with greeks in the XSP option's own points (delta as the proxy's, gamma × 10,
theta and vega ÷ 10 — so against those own-terms greeks an XSP leg's
SPX-point exposure is delta × 0.1, gamma × 0.01, theta × 1, vega × 1), on
whole-dollar strikes only (XSP lists no `.5` strikes; `docs/IPC.md` §The XSP
chain);
an XSP leg modeled there writes `XSP 100 …` order lines (`(Weeklys)` off a
third Friday, never `[AM]`; the spelling is still unverified against a real
TOS paste). A single-leg XSP limit price is rounded to the cent — Cboe lists
Mini-SPX options at a $0.01 minimum increment for every premium — where an
SPX/SPXW single keeps the $0.05 (below $3) / $0.10 tick; multi-leg orders
round to the cent on either. A structure label Wingspan writes itself names
the product before an XSP order's or group's strikes (`10x XSP 650P DEC26`),
so a tenth-sized strike never reads as an SPX one. Not built: quoting XSP symbols from a provider,
an XSP config section, and an XSP-point axis on the risk graph.

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
  ~15-minute delay is reported as `Degraded`. It also lists and quotes the
  option chain (the Model tab) through two more additive request fields on the
  same one-request-one-response bridge, `list_expirations` and `chain`; Yahoo
  sends a whole expiry per call, so the strike window is ignored, and the
  volume and open interest Yahoo reports ride along on those quotes. **Root
  inference caveat:** Yahoo's expiration list is bare dates — no root, no
  settlement — so the *listing* is the one place a root is inferred from the
  calendar: every Yahoo date is listed as `SPXW`/PM `weekly` (`monthly` when it
  is a third Friday) **plus** an `SPX`/AM `monthly` entry on third Fridays. A
  monthly that the exchange moved to the Thursday for a Friday holiday is
  therefore listed as an `SPXW` weekly and gets no `SPX` section. The inference
  stops at the listing: every contract that is *quoted* takes its root,
  settlement, expiry, strike and right from its own `contractSymbol` (Yahoo
  omits the OSI padding — `SPXW261016C06500000` — and both forms are read) and
  is kept only when that root is the one the section asked for, so a guessed
  `SPX` section on a date where Yahoo carries only `SPXW` symbols comes back
  empty with a sentence saying so, never weeklies under the monthly's name. A
  `[yfinance] script` override older than these fields is named in the error
  rather than silently listing nothing.
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
  of the rows it last read (`trade_source_cache.csv` and
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
