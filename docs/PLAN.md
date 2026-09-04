# Wingspan — Options Position Dashboard & Risk Analyzer

## Context

The user trades SPX option structures (butterflies, calendars, diagonals, hedges) in a Schwab/thinkorswim account and tracks each *trade* as a plain-text file of thinkorswim (TOS) order strings, organised in a folder like `example_trades/Trades/`:

- `Open Trades/<Name> <Month> <Day> <Year>` — one file per live trade (e.g. `M1 July 14 2026`), one TOS order string per line. A trade may contain several spread orders placed on different days (e.g. `BSH` bought a Sep put on 7/14 then rolled it into an Oct/Sep diagonal on 8/19).
- `Closed Trades/<Name> <Date> Closed` — same format, for finished trades (the date is the *open* date).
- `Daily Trade Statement/YYYY-MM-DD-AccountStatement.csv` — TOS account-statement exports for each day an option order filled. They are the only source of **per-leg fill prices, execution times, commissions/fees and end-of-day marks**, and their `Options` section is an account-level (netted across trades) position list to reconcile against.

There is no existing app. The user wants a polished, production-ready, extensible, well-tested **web-based dashboard** with the core logic in **Rust** that:

1. Treats each file in *Open Trades* as one trade; shows every open trade with DIT, P&L, delta, gamma, theta, vega and weighted vega; lets the user toggle trades in/out of the analysis set.
2. Shows overall portfolio stats (all trades and the selected subset).
3. Uses **Schwab's API** for live data (token exists only on the production machine; token path configurable), falling back to **yfinance** with a clear in-app notification.
4. Draws an **OptionNetExplorer (ONE)-style risk graph** for the selected trades only: the expiration line plus five T+x lines starting at T+0.
5. Ignores everything that is not an option trade (equity/ETF rebalances, futures/forex/crypto sections).

Also: a new working directory + git repo with a proper `.gitignore`, a **private GitHub remote** (`gh` is authenticated as `jbolinge` with `repo` scope), and this plan saved into the repo.

### Facts learned from the example data (they drive the design)

- TOS order grammar: `BUY +2 BUTTERFLY SPX 100 19 FEB 27 [AM] 7325/6975/6625 PUT @22.10 LMT FIXED`. Seen: single (no strategy word), `VERTICAL`, `CALENDAR` (`16 APR 27 [AM]/19 FEB 27 [AM] 8200 CALL`), `DIAGONAL` (`19 MAR 27 [AM]/19 FEB 27 [AM] 8200/8250 CALL`), `(Weeklys)` root flag, `[AM]` settlement flag, signed qty (`SELL -1 VERTICAL …`), a trailing `CANCEL AT 8/19/26 14:01:19` clause before `FIXED`. Statement dialect uses `BOT`/`SOLD` and ends with the exchange (`CBOE`); order-history rows use `WLKLMT`, `MOC`.
- First expiry ↔ first strike ↔ the bought leg in every calendar/diagonal (`16 OCT 26/18 SEP 26 5450/5050 PUT` = buy OCT 5450P, sell SEP 5050P). Butterfly `A/B/C` = `+n/−2n/+n`.
- Trade files list orders **out of chronological order** and carry no dates; the file name carries the trade's open date, which can be a non-trading day (`M5 Aug 15 2026` is a Saturday; its fills are 8/14). Closed-trade `closed_on` must come from the last matched execution (`Delta 1 Aug 25 2026 Closed` closed 8/28).
- Statement CSV: UTF-8 BOM, CRLF, sections identified by header names (not reliably by blank lines — there is a line with a single space and a stray `"Total Cash …"` row that moves between sections), Excel refs `="9000000000001"`, quoted thousands, parenthesised negatives, `$` prefixes, leading-dot decimals (`.725`), `--`/`N/A` sentinels, multi-row spreads with empty continuation `Exec Time`, a `RE #…` replace-order ref shifting columns in an *Order History* continuation row, `CANCELED` orders with identical legs, `TO OPEN`/`TO CLOSE` effects. Header line dates lie (`since 8/27/26 through 8/27/26` in a file whose rows are all 8/28) — `as_of` comes from row/file dates. Times are **Central** (`business day 19.08 CST`, `15:00:00` MOC fills).
- Trade files carry **fill** prices that match *Account Trade History* `Net Price`; *Order History* `PRICE` is the limit and differs in 4 of 14 cases.
- The account nets across trades: `Delta 1` sold the FEB 6800 put that `M4` holds long (`TO CLOSE`, blended `Trade Price` 104.855 on 8/28). Per-trade lots are mandatory; reconciliation must sum all trades and compare **quantities only**.
- `BSH` opened a leg 7/14 (4.90) and closed it 8/19 (0.57) within the trade → realised −$433, plus unrealised on the surviving leg, fees $3.78 → total −$613.78. Trades carry realised + unrealised.
- `LPTA … Closed` contains six lines byte-identical to `M1`/`M2`/`M3` orders; the statements prove the account never held those extra contracts (7325P is +4, not +6). Duplicate claims must be **quarantined**, never netted. LPTA's truth is one weekly put round-trip (−$160 − $2.35 fees).
- `SPXW260821P5875` expires on a **third Friday** yet is PM/weekly — settlement/root must come from the OPRA code (or the `(Weeklys)`/`[AM]` tokens), never from the calendar.
- Statement `Options` rows include positions absent from any trade file (a JUL 26 box) → *unassigned* warning, not an error. `Trades/` contains a `.DS_Store` → skip dotfiles.
- Machine: Rust 1.98, Node 24 (via nvm — `source ~/.nvm/nvm.sh` in non-interactive shells), Python 3.13 (no `yfinance` yet), Playwright installed, `gh` authenticated. The workspace root has an unrelated Playwright `.gitignore`/`.github` — leave them alone; the project is its own repo under `~/workspace/wingspan/`.
- Verified externally: Yahoo serves the `^SPX` options chain (so yfinance can supply spot *and* chain, ~15-min delayed, no greeks); Schwab option quotes return bid/ask/mark/`volatility`/greeks/`theoreticalOptionValue`/`underlyingPrice`.

## Name & location

**`wingspan`** — butterflies have wings; the risk graph spans price. Directory `/home/jbolinge/workspace/wingspan/`, GitHub `jbolinge/wingspan` (private), binary `wingspan`.

## Architecture — Cargo workspace + Vite SPA, single release binary

```
wingspan/
├── Cargo.toml                  # workspace; shared [workspace.dependencies]; [workspace.lints] (clippy pedantic subset)
├── rust-toolchain.toml, .editorconfig, deny.toml, justfile, CLAUDE.md, README.md, wingspan.example.toml
├── crates/
│   ├── wingspan-core/          # domain model, TOS order parser, statement parser, assembly, P&L, reconciliation
│   ├── wingspan-pricing/       # BSM, IV solver, forward calibration, greeks (bump-and-reprice), weighted vega, risk-graph engine
│   ├── wingspan-marketdata/    # SpotSource/OptionQuoteSource traits, Schwab, yfinance bridge, cached-snapshot, statement-IV, fixture, fallback chain
│   └── wingspan-server/        # bin `wingspan`: clap CLI, config, axum API, SSE, file watcher, embedded SPA (feature `embed-ui`)
├── web/                        # React 19 + TypeScript + Vite + Tailwind v4 + ECharts + TanStack Query + Zustand
├── bridge/yfinance_bridge.py   # Python sidecar (stdin/stdout JSON), requirements.txt (pinned yfinance)
├── fixtures/trades/            # anonymised copy of the example folder (see “Fixtures” below) — tests + demo mode
├── docs/PLAN.md (this file), ARCHITECTURE.md, CONFIG.md, SCHWAB.md, DATA-FORMATS.md, DEPLOY.md, wingspan.service
└── .github/workflows/ci.yml
```

Data flow: `trades dir ─parse→ Trades ─match/net→ Positions ─price(snapshot)→ Stats & Curves ─JSON/SSE→ SPA`. Everything below the API is pure, synchronous, unit-testable Rust; I/O (files, HTTP, Python) sits at the edges behind traits.

### 1. `wingspan-core` — domain & parsing

Types (`rust_decimal` for money/strikes; `chrono` + `chrono-tz`):

- `OptionContract { underlying, root (SPX|SPXW|…), expiry, settlement: Am|Pm, strike, right, multiplier }` with `to_osi()` (`SPX   270219P07325000`, root space-padded to 6), `to_tos_code()`/`from_tos_code()` (`SPXW260821P5875`), `expiry_instant()` (Am → 09:30 ET, Pm → 16:00 ET on the expiry date; documented in DATA-FORMATS.md). Settlement rules per underlying live in a small table (extension point for other underlyings).
- `Strategy` registry — table-driven `LegTemplate`s: `Single [+1]`, `Vertical [+1,−1]`, `Calendar [+1 far, −1 near]`, `Diagonal [+1 far/k1, −1 near/k2]`, `Butterfly [+1,−2,+1]`; `Straddle`, `Strangle`, `Condor`, `IronCondor`, unbalanced `~BUTTERFLY` ratios and `BACKRATIO` as documented rows to add. `SELL` flips signs once (qty is already signed in the string — no double negation); order qty multiplies. Unknown strategy → `ParseError { file, line, token }`.
- `Order { raw, side, qty, strategy, legs: Vec<(OptionContract, i32)>, net_price, hint: Option<DateHint>, execution: Option<Execution>, status: Matched|Unmatched|Quarantined(reason) }`, `Execution { at: DateTime<Tz>, ref_no, fills: Vec<Fill{contract, qty, price}>, fees: Fees{commission, misc} }`.
- `Trade { id (slug), name, opened_on (file name), closed_on: Option (last execution), status: Open|Closed, source_path, orders }`.
- `TradePosition` (derived): netted `legs: Vec<PositionLeg { contract, qty, avg_cost: Option }>`, `net_cash_flow`, `fees`, `realized`, `closed_contracts`, `expired_legs`, `quarantined_orders`.
- `Warning` enum — `UnmatchedOrder`, `DateOutsideWindow`, `DuplicateExecutionClaim`, `ClosedTradeHasOpenLegs`, `ExpiredLegUnknownSettlement`, `UnassignedAccountPosition`, `ReconciliationMismatch`, `StatementSectionSkipped`, `StatementDateDisagreement`, `SettlementInferred`, … Every warning is data with a severity and a trade/contract anchor; shown in the UI; never a panic.

Parsers:

- `tos_order::parse(line)` — hand-written tokenizer: side (`BUY|SELL|BOT|SOLD`), signed qty, optional strategy (incl. `1/3/2 ~BUTTERFLY` ratio prefix), symbol, multiplier, optional `(Weeklys)`, `/`-separated expiries each with optional `[AM]`, `/`-separated strikes, right, `@price` (negative allowed), then a tolerant tail (`LMT|MKT|WLKLMT|MOC`, `CANCEL AT m/d/yy hh:mm:ss` → `DateHint`, `GTC|DAY`, `FIXED`, exchange). Optional trailing `# …` comment on trade-file lines (`# 2026-07-14`, `# ref=9000000000001`) parsed as an explicit hint — the user's escape hatch when two orders are otherwise indistinguishable.
- `trade_file::parse(path)` — file name grammar parsed right-to-left: `<name…> <Month(full|abbrev)> <d{1,2}> <yyyy> [Closed]`; remainder is the name (`Delta 1`). Dotfiles and unparseable names are skipped with a warning. Blank/`#` lines ignored.
- `statement::parse(path)` — split on known section-header names (`Cash Balance`, `Account Order History`, `Account Trade History`, `Options`, `Account Summary`; all others skipped), then per-section `csv` (flexible) readers → `Statement { as_of (from row dates ⨯ file name, disagreement → warning), cash_trades, executed_orders (grouped multi-row spreads: per-leg price + OPRA), order_status (ref → Filled/Canceled, Pos Effect; parsed defensively because continuation rows drift), option_positions, account_summary }`. Numeric cleaner handles `$`, `,`, `(…)`, `.725`, `--`, `N/A`, `="…"`. Non-option rows dropped. Times parsed in `statement_timezone` (default `America/Chicago`).
- `discovery::find_root(path)` — locates the folder containing (case-insensitive) `Open Trades`/`Closed Trades`/`Daily Trade Statement`, descending one level if needed (`example_trades/` → `example_trades/Trades/`); names overridable in config.

Assembly (`assembler.rs`):

1. Index executions from all statements by `(strategy, canonical signed legs, net price)`; attach fees from the `Cash Balance` TRD row with the same ref/time; attach `TO OPEN/TO CLOSE` from order status; drop `CANCELED`.
2. For each trade (open first, then closed; ordered by `opened_on`), for each order: candidates = unclaimed executions with the same key; choose by explicit hint (comment / `CANCEL AT` timestamp) else nearest `|exec_date − opened_on|` within ±7 days (`DateOutsideWindow` if beyond). No candidate → if the execution exists but is claimed by another trade → **`Quarantined(DuplicateExecutionClaim)`: excluded from legs, cash flow and reconciliation**; else `Unmatched`: order dated `opened_on`, fees 0, legs included (with warning).
3. Root/settlement: from the matched OPRA code; fallback `(Weeklys)`/`[AM]` tokens; never from the calendar.
4. Net legs per contract → `TradePosition`. **P&L(total) = Σ open-leg mark value + Σ order cash flows − fees**; **P&L(open)** = Σ (mark − avg cost) × qty × mult over open legs; realised = total − open. `P&L %` = total ÷ |net premium paid| for debit trades, ÷ credit received for credit trades (label “% of cost”; `—` when zero). **DIT** = calendar days from `opened_on` (the user's own designation) to today / `closed_on`; **DTE** = days to the front expiry of open legs. Expired legs → settlement value from provider history if available, else 0 + warning.
5. Reconciliation: latest statement `Options` **quantities** (blended `Trade Price` is ignored — it is a cross-trade average) vs Σ legs across all trades → per-contract `Match | QtyMismatch | Unassigned | Missing`. The app's per-trade attribution knowingly differs from broker/tax lots; say so in the UI.

### 2. `wingspan-pricing` — models & risk engine

- `bsm` — European price/greeks (SPX is European; exact under BSM). `iv::solve(mark, …)` Newton + bracketed bisection, intrinsic floor, bounds `[0.1%, 500%]`, guards for crossed/zero marks; provider IV used only as the seed and a diagnostic. **All greeks and curves use our solved IVs so the table and graph always agree** (invariant: T+0 curve at spot == dashboard P&L).
- `forward` — per-expiry forward calibrated by put-call parity from an ATM call/put pair (the market-data layer requests these “calibration contracts” alongside the book); fallback to configured `risk_free_rate`/`dividend_yield`. Removes the r/q guesswork that would otherwise skew every IV.
- `greeks` — bump-and-reprice: delta/gamma per 1 index point, vega per 1.00 vol-point, **theta = reprice at T−1 calendar day** (ONE-style). Position greeks = Σ qty × mult × greek. Units table published to the UI as tooltips.
- `weighted_vega` — Σ leg vega × (REF_DTE / DTE_leg)^k, `k` = 0.5 default; `REF_DTE` = **front expiry of the position/selected set (user-confirmed default)**, with fixed-days (e.g. 30) as the configurable alternative; the reference in use is always shown next to the column.
- `time` — ACT/365 year fraction to `expiry_instant()` with intraday fraction; `DayCount` trait so trading-day time can be added.
- `risk_graph::compute(legs, snapshot, spec) → RiskGraph` with `spec = { price_range (default [min strike−5%, max strike+5%] ∪ spot±10%; absolute or %), points (241), dates: Vec<GraphDate = Today | Offset(days) | Expiry(date)>, vol_shift_global, vol_shift_by_expiry, spot_override, include_realized: bool (default true — ONE-style, user-confirmed) }`. For each date/price: legs expired at that date → intrinsic; others BSM at their own IV (+shifts), sticky-strike (a `VolDynamics` trait with `StickyStrike` now, `StickyDelta` later). `pnl = Σ qty·mult·V + net_cash_flow − fees` (or open-only when `include_realized=false`). Default date preset: `[T+0, T+¼, T+½, T+¾, T+(DTE−1)]` of DTE-to-front-expiry plus `Expiry(front)`; any dates editable; later expiries selectable. Output per date: curve, breakevens, max profit/loss on range, plus per-trade component curves; also what-if greeks at `(price, date, vol)` for the what-if panel, ±1σ/2σ price bands from the front ATM IV, and probability-of-profit at expiry (lognormal).

### 3. `wingspan-marketdata` — providers

Two capabilities so partial providers compose:

```rust
#[async_trait] pub trait SpotSource        { async fn spot(&self, underlying) -> Result<UnderlyingQuote>; fn health(&self) -> Health; }
#[async_trait] pub trait OptionQuoteSource { async fn quotes(&self, contracts: &[OptionContract]) -> Result<Vec<OptionQuote>>; fn health(&self) -> Health; }
// OptionQuote { contract, bid, ask, last, mark, iv_hint, greeks_hint, as_of }; Health = Ready | Degraded(reason) | Unavailable(reason)
```

- **Schwab** (`SpotSource + OptionQuoteSource`) — `GET https://api.schwabapi.com/marketdata/v1/quotes?symbols=$SPX,SPX%20%20%20270219P07325000,…` (OSI symbols, chunked ≤ 50, 429 backoff, request budget). Token file (user-confirmed schema) at configurable `schwab.token_path` (or the `token_config`/`token_config_key` indirection): JSON with `access_token`, `refresh_token`, `token_type`, `access_expires_at`, `refresh_created_at`, `account_number` — loaded via a small `TokenStore` adapter (this schema natively; a schwab-py-format adapter as a second impl for portability). App key/secret from env (names configurable). **The token file is read-only to wingspan** (user decision 2026-09-02): it is produced, refreshed and owned by three other applications on the production server, so the app receives only a path, opens it read-only, and never writes, creates, renames, truncates, chmods, locks, copies or backs up anything in that directory — there is no `TokenStore` write method. Access-token refresh via `POST /v1/oauth/token` (refresh grant, basic auth) is preceded by a re-read of the file (a sibling application may already have refreshed it, or a login may have replaced it) and its result — access token, expiry, any rotated refresh token — is held **only in process memory**; the in-memory token is therefore ahead of the file from the first refresh onward, and a re-read is adopted only on real evidence the file moved forward (later `refresh_created_at`, or a refresh token the file did not previously carry). `[schwab] refresh = true|false` decides only whether wingspan may run the grant itself. Health reporting refresh-token age from `refresh_created_at` with a hard warning from day 6 (7-day life); a not-`0600` token file is a warning naming the other owners, never an error or a `chmod`. `account_number` is treated as a secret (redacted in logs/`/config`). Interactive login is out of scope (documented in SCHWAB.md; `wingspan schwab login` is a future extension). Extension points: `/chains`, Accounts & Trading `positions` for live reconciliation.
- **YFinance** (`SpotSource + OptionQuoteSource`) — `bridge/yfinance_bridge.py` (pinned `yfinance` in a venv, `[yfinance] python` configurable) run as a subprocess with a JSON request on stdin; returns `^SPX` spot and the requested contracts from the `^SPX` chain (bid/ask/last/IV). Timeout, clear reasons (`python not found`, `yfinance missing`, `Yahoo error`), ~15-min delay reported as `Degraded`.
- **CachedSnapshot** (`both`) — last good snapshot persisted to `$XDG_STATE_HOME/wingspan/snapshot.json`; served with its age when everything live fails.
- **StatementIv** (`OptionQuoteSource`) — IVs calibrated from the latest statement's `Mark` column (with that day's spot from the cache or Yahoo history), held sticky-strike; combined with any `SpotSource` it yields an honest offline risk graph. *That day's* spot is load-bearing: the chain hands the live level to `observe_spot` before asking for quotes, so calibrating at today's level makes the sticky-strike reprice an identity and the index move vanishes. The server keeps a `date -> close` ledger (`$XDG_STATE_HOME/wingspan/closes.json`, one entry per session, filed by the print's own session date) and refuses to solve the surface when it has no level for the statement's date — the rung goes dark with that sentence rather than guessing.
- **Fixture** (`both`) — deterministic synthetic surface for tests, `--demo`, Playwright.
- **FallbackChain** — configured order per capability (`spot = ["schwab","yfinance","cache"]`, `options = ["schwab","yfinance","cache","statement"]`); `MarketSnapshot { as_of, spot_source, option_source, degraded: Vec<reason>, spot, quotes, forwards }` in `arc_swap`; background refresh (15 s in RTH / 5 min otherwise, configurable) broadcasts changes. Off-hours: spot is the prior close and the banner says so. Source + reasons ride on every snapshot so the banner is always truthful, and per-quote `as_of` drives a staleness chip on each P&L cell.

### 4. `wingspan-server` — API & CLI

- Config via `figment`: `wingspan.toml` (`--config`, `$WINGSPAN_CONFIG`, `./wingspan.toml`, `~/.config/wingspan/wingspan.toml`) + `WINGSPAN_*` env. Sections: `[trades] root, open_dir, closed_dir, statements_dir, statement_timezone`, `[server] bind (default 127.0.0.1:8787), allow_remote, auth_token, lan_only, lan_networks, trusted_proxies, allowed_hosts`, `[schwab] token_path, app_key_env, app_secret_env, refresh`, `[yfinance] python, enabled`, `[market] spot_sources, option_sources, refresh_secs_open/closed, state_dir`, `[pricing] risk_free_rate, dividend_yield, weighted_vega { reference = "front_expiry" (default) | "<n>d", exponent }`, `[risk_graph] points, range_pct, tplus_preset`.
- Routes `/api/v1`: `health`, `trades` (open + stats), `trades/closed`, `trades/{id}`, `trades/root` (`GET` which book is loaded, `DELETE` clear), `POST trades/upload`, `portfolio?ids=`, `POST risk-graph`, `POST what-if`, `market/snapshot`, `reconciliation`, `warnings`, `config` (resolved, secrets redacted), `POST reload`, `events` (SSE: `snapshot`, `trades_changed`, `provider`). Errors as `application/problem+json`; `tracing` logs; `tower-http` compression. Non-loopback bind refused unless `allow_remote` **and a gate** are set (the page shows the whole book) — see the LAN bullet below.
- **A LAN bind behind HAProxy is a first-class deployment** (user decision 2026-09-02): the production instance binds beyond loopback and is reached through a LAN-only HAProxy that terminates TLS. Since the SPA cannot send a bearer token, `auth_token` is no longer the only gate a remote bind may have: `[server] lan_only = true` refuses every request — `/api`, SSE and the SPA alike — whose resolved client address is outside `[server] lan_networks` (empty = the built-in private set; `100.64.0.0/10` deliberately excluded and documented). A non-loopback bind needs `allow_remote` **plus** `lan_only` or a non-empty `auth_token`. The client address is the TCP peer (IPv4-mapped IPv6 canonicalised) unless that peer is in `[server] trusted_proxies`, in which case `X-Forwarded-For` is walked right-to-left skipping further trusted proxies; a trusted proxy that forwards nothing usable, or a chain holding a non-address, is refused rather than believed (403 `outside-lan` / `missing-forwarded-for` / `untrusted-proxy`). The gate is the outermost layer. Consequence: the `Host` guard now applies on **every** bind (a rebound name satisfies an address gate perfectly), so the LAN address or the proxy's public name goes in `[server] allowed_hosts`.
- **Trades are upload-only and transient on the server** (user decision 2026-09-02): the SPA reads the user's folder in the browser and `POST /trades/upload` copies it to `<state_dir>/uploads/current`, `GET /trades/root` reports it as `"source": "uploaded"`, and `DELETE /trades/root` (the UI's *clear trades*) deletes the whole `uploads` tree, the `trades_root.json` pointing at it and the cached market snapshot derived from it — nothing taken from the files survives on disk. The refresh loop runs on its own timer with the loaded book's contracts in hand, so the cache carries a generation that `forget()` bumps and a fetch already on the wire drops its write rather than putting the file back. There is no route that lists or picks a folder on the server. `[trades] root` stays a deployer/CLI setting (`validate`, `serve --root/--demo`, dev) and is not changeable at runtime; with neither it nor an upload the book is empty (`"source": "empty"`) rather than a start-up error. **Superseded 2026-09-03 — see "2026-09-03 — pivot to a Tauri desktop app" below:** wingspan becomes a desktop app that reads a folder the user chooses on their own computer, in place; uploads are gone and "clear trades" now means "stop showing them", never "delete them".
- `notify` watcher (debounced) on the trades dir → reload → SSE.
- SPA embedded via `rust-embed` behind feature `embed-ui` (default on for release; CI builds `web/dist` first); `--dev` proxies to Vite.
- CLI: `wingspan serve [--demo]`, `wingspan validate <dir>` (trades, netted legs, P&L, warnings, reconciliation table — the user's sanity check), `wingspan risk <trade>` (curve table/CSV), `wingspan schwab status`, `wingspan doctor` (config, dirs, python/yfinance, token file, network).

### 5. `web/` — UI

Stack: React 19, TypeScript strict, Vite, Tailwind v4, Apache ECharts, TanStack Query, Zustand (selection + graph settings persisted in `localStorage`), Vitest + Testing Library, Playwright. Dark trading-terminal theme with light toggle, dense but legible, colour-blind-safe palette (ONE-like preset).

- **Status bar** — provider pill `LIVE · Schwab` / `DELAYED · yfinance (Schwab: token file missing)` / `STALE · cached 2 h` / `OFFLINE · statement marks 2026-08-28` with reason tooltip; SPX spot/change; as-of + staleness; reload; theme.
- **Positions table** — ☐, Name, Opened, DIT, DTE, Structure (`2× BF 7325/6975/6625 FEB27 · 1× DIAG 8200/8250 MAR/FEB27`), Cost, Value, P&L open, P&L total, %, Δ, Γ, Θ, V, W.V (reference shown in header), staleness dot, ⚠. Expandable rows → legs (qty, avg cost, mark, bid/ask, IV, greeks, DTE) and quarantined/unmatched orders. Select all/none/invert; sortable; units tooltips.
- **Portfolio cards** — Selected vs All: P&L, Δ, Γ, Θ, V, W.V, count.
- **Risk graph** — expiration line thick, T+0 solid, T+x dashed graded; zero line; spot marker; ±1σ/2σ bands; breakeven markers; crosshair readout with P&L/Δ/Θ per line at cursor; controls: price range (abs/%), y-axis lock, global + per-expiry vol sliders, date list editor (presets + arbitrary dates incl. later expiries), per-line toggles, per-trade overlay, include-realised toggle, price-slice table, max P/L and PoP readouts; **what-if panel** (price / date / vol) that recomputes the greeks readout, not just the chart.
- **Closed trades** tab — realised P&L, fees, duration, totals/win-rate.
- **Data health** tab — warnings (with quarantined orders and how to fix via `#` hints), reconciliation table, statement coverage, provider diagnostics, resolved config.

### 6. Fixtures & privacy

The real statements contain the account number and net-liq. `fixtures/trades/` will be an **anonymised** copy generated by `scripts/anonymize_statements.py` (account id → `00000000TEST`, balances rescaled, equities/P&L sections replaced by a single dummy row, options sections kept verbatim, broker execution refs renumbered into a synthetic `9000000000001…` sequence) — user-confirmed. The example folder itself is never committed.

### 7. Testing

- core: every order line in the example files as parser fixtures; strategy tables; statement golden tests (`insta`) over all statements; assembly cases — BSH roll (−$613.78 total, −$433 realised), Delta 1 vs M4 netting (+$135.20), M5 fills before the filename date, LPTA quarantine (2 legs, −$160 −$2.35), unmatched order, closed trade with residual legs, `CANCELED` 57.30 ignored, `RE #` row, lying header (`as_of` = 8/28), Central-time parsing, SPXW third-Friday settlement, leading-dot decimals, signed-qty no-double-negation, quantity-only reconciliation with blended `Trade Price`, dotfile skip, root discovery.
- pricing: textbook BSM values, `proptest` parity/monotonicity, IV round-trip, bumped vs analytic greeks, forward calibration, weighted vega both references, curve invariants (T+0@spot == P&L; expiry curve piecewise-linear only when all legs expire that date; long-vega curves rise with vol shift), **golden CSV of the six curves** for the fixture snapshot.
- marketdata: `wiremock` Schwab (auth header, OSI encoding, chunking, 401→re-read→in-memory refresh→retry, 429 backoff, and proof the token file is untouched: identical bytes, identical mtime, no new directory entries, plus a read-only token dir and a sibling application's refresh picked up on re-read), fake python bridge, fallback ordering/reasons per capability, cache persistence, staleness.
- server: API integration tests over `fixtures/trades` + `Fixture`; SSE; config precedence; bind policy.
- web: Vitest (formatters, aggregation, store); Playwright e2e on `wingspan serve --demo` (toggle → table/cards/chart update; banner states; theme) with **one screenshot diff of the risk chart**.
- CI: `cargo fmt --check`, `clippy -D warnings`, `cargo test --locked`, `cargo deny`, MSRV check, web typecheck/lint/test/build, Playwright (browsers installed in the job). `just ci` mirrors it locally.

### 8. Production / deployment

`cargo build --release --features embed-ui` → one `wingspan` binary (x86_64 Linux) + `wingspan.toml`. `docs/DEPLOY.md`: first-run checklist (`wingspan doctor`), pointing `[schwab] token_path` at the read-only token file another application already produces and refreshes, venv for yfinance, `wingspan.service` (systemd; loopback bind, or a LAN bind behind HAProxy with `lan_only`), log/state dirs. Tokens never logged; token-file permissions checked (warn if world-readable).

`.gitignore`: `/target`, `web/node_modules`, `web/dist`, `/wingspan.toml`, `*.local.toml`, `.env*`, `**/token*.json`, `.venv/`, `__pycache__/`, Playwright artefacts, `.DS_Store`, editor files — never a bare `*.toml`.

Extensibility documented in `ARCHITECTURE.md`: strategy (one table row), provider (one trait impl + config entry), statistic (one function + column def), vol dynamics (one trait impl), underlying (settlement-rules row), graph line type.

## Implementation phases (each ends with green tests + a commit)

0. **Scaffold** — `~/workspace/wingspan`, `git init`, `.gitignore`, workspace + crate skeletons, `web/` scaffold, CI, README, `docs/PLAN.md` (this plan), anonymised `fixtures/trades`; `gh repo create jbolinge/wingspan --private --source . --push`.
1. **Core parsing** — contracts, TOS order parser, trade-file loader + name grammar, statement parser, discovery; fixture tests.
2. **Assembly** — execution index, matching + hints + quarantine, netting, P&L, reconciliation, warnings; `wingspan validate`.
3. **Pricing** — BSM, IV, forward calibration, bumped greeks, weighted vega, risk-graph engine, what-if; property + golden tests.
4. **Market data** — traits, Fixture, CachedSnapshot, StatementIv, Schwab (wiremock), yfinance bridge, FallbackChain, refresh loop.
5. **Server** — config, routes, SSE, watcher, embedding, bind policy, `doctor`; integration tests.
6. **UI** — status bar, positions table, portfolio cards, risk graph + what-if, closed/health tabs, theming.
7. **E2E & polish** — Playwright + screenshot diff, docs (CONFIG/SCHWAB/DATA-FORMATS/ARCHITECTURE/DEPLOY), README screenshots, release build, `v0.1.0`.
8. **Review** — `/code-review`, `/security-review`, fix findings.

## Verification (end-to-end)

1. `just ci` locally: fmt, clippy, all Rust tests, web lint/typecheck/tests, e2e.
2. `wingspan validate example_trades` prints 6 open / 2 closed trades; BSH = +1 OCT 5450P only (total −$613.78); M4 = +2/−4/+2 FEB fly + APR/FEB diag; Delta 1 = +$135.20; LPTA = 2-leg round trip with 6 quarantined orders flagged; unassigned JUL 26 box warning; quantity reconciliation matches the 2026-08-28 statement exactly.
3. `wingspan serve --demo` → `http://127.0.0.1:8787`: six open trades with DIT/DTE/P&L/greeks; toggling changes cards and graph; expiration + 5 T+x lines; what-if panel updates greeks; banner shows the fixture source. Playwright screenshots reviewed.
4. yfinance installed, no Schwab token → `DELAYED · yfinance (Schwab: …)`; network off → `STALE`/`OFFLINE` with age; P&L cells show staleness.
5. Schwab verified via wiremock here and `wingspan schwab status` + `doctor` on the production machine.

## 2026-09-03 — pivot to a Tauri desktop app

Wingspan pivots from a LAN-served web app to a Tauri 2 desktop application, targeting Ubuntu 26.04 as the only supported platform. The production deployment this plan described above — a non-loopback bind reached through a LAN-only HAProxy, gated by `[server] lan_only`/`auth_token`/`trusted_proxies` — is retired along with the `[server]` config section and the bearer token entirely; there is no longer a remote client to authenticate, because the app and the browser surface it presents both run on the user's own machine.

Trades are read in place. Instead of uploading a folder's contents to the server, the user picks a folder with the operating system's own folder chooser, and wingspan reads statements and trade files directly from wherever that folder lives — the files are never copied anywhere. A `notify`-based watcher observes the chosen folder with a 750 ms debounce, with an optional poll-based backend available for network-mounted folders where inotify does not fire reliably. The choice persists across launches in `<state_dir>/trades_root.json`, which also keeps a short list of up to five recently used folders so switching between books is a click rather than a re-browse. Clearing trades in the UI now means exactly what a user would expect and nothing more: the app stops displaying the loaded book. It does not touch the folder on disk. This is the owner's explicit framing (decision D23) and it is enforced structurally, not just by convention — `clippy.toml` bans `remove_dir_all` outright across the application code, with exactly one named exception, `migrate::delete_discarded_uploads`, carrying an explicit `#[allow]` at its call site. That one exception exists for the upgrade path: a user coming from the earlier upload-based build has an `uploads` tree already sitting in their state directory, and on first launch of the desktop build that tree is renamed — never deleted — to `uploads.discarded-<date>`, with a note left behind explaining where it went and why.

The state directory moves to the platform-appropriate location for a desktop app, `~/.local/share/com.jbolinge.wingspan/state`, replacing the old `$XDG_STATE_HOME/wingspan` path. The Schwab token file keeps the read-only contract this plan already established above: wingspan is handed a path, opens it read-only, and nothing in the codebase writes, creates, renames, chmods, locks, copies or backs up anything in that directory.

The transport between the SPA and the backend changes from HTTP/SSE to Tauri's IPC. Each server route becomes a Tauri command taking a single `request` argument, matching the existing typed request/response shapes so the frontend's API layer changes shape but not vocabulary. Every command is `async` and does its work inside `spawn_blocking`, so a panic in one command surfaces as that one panel's error rather than taking down the window. A loopback-only HTTP dev server is kept alongside the IPC transport purely so Playwright can continue driving the app the way it always has, without needing a WebView automation story of its own.

The migration proceeds in phases: phase 3 moves trades to local folders read in place, still served over HTTP, ahead of the pivot itself; phase 4 adds the desktop crate additively, alongside the existing server, so nothing is deleted before its replacement exists; phase 5 flips the SPA's transport from HTTP to Tauri IPC; phase 6 completes the crate split between the web-server and desktop binaries; phases 7 and 8 build the `.deb` bundle and add settings and diagnostics screens respectively. The authoritative source for this migration — every decision behind it, the full command inventory, and the phase-by-phase task breakdown — is `~/workspace/wingspan-tauri-plan/WINGSPAN-TAURI-PLAN.md`.
