# Wingspan IPC contract (frozen)

The desktop app talks to its backend over Tauri's `invoke` — commands, not
routes. What follows is the frozen contract: every command and the DTO it
answers with. Money fields are JSON numbers serialized from `Decimal`;
vols/greeks are floats. Timestamps are RFC 3339 UTC unless noted. Change a
shape only by editing this file first.

Three things about the transport itself, before the commands:

- **Every payload travels as one `request` argument.** Tauri turns
  camelCase JS keys into snake_case Rust parameters, but `web/src/api/
  types.ts` is *already* snake_case (`trade_ids`, `vol_shift_by_expiry`), so
  spreading a body's fields across separate command parameters fails at
  runtime rather than at compile time. `invoke('portfolio', { request: {
  ids } })`, never `invoke('portfolio', { ids })`.
- **`risk_graph` takes a second top-level argument, `seq`.** It is
  deliberately *not* a field of the request DTO: `queryKeys.riskGraph
  (request)` is `['risk-graph', request]`, and a per-call sequence number
  folded into the request would make every drag of the risk-graph slider a
  fresh TanStack Query cache key. `seq` never appears in a contract dump
  (`wingspan-cli dump-contract`) because it is not part of any DTO.
- This contract is shared by `wingspan-devserver` (the dev-only HTTP front
  end Playwright drives) and the desktop app — the appendix at the bottom
  maps each command to the route it corresponds to there. The devserver is
  never shipped.

## Read commands

### `health`
No request body.
`{ "status": "ok", "version": "0.2.4", "ready": true }`

`ready` is `false` until boot has published an `AppState`. This is the one
command that answers *while* the app is still starting up — every other one
pends until boot publishes (see "The pending-boot `AppSlot`" in
`docs/ARCHITECTURE.md`). The SPA does not poll it: readiness reaches the
window as the `app_ready`/`app_error` events, with the pending `trades` query
covering the wait.

### `market_snapshot`
No request body.
```jsonc
{
  "as_of": "2026-08-31T14:30:02Z",    // when the ladder ran — a refresh time, not a data age
  "data_as_of": "2026-08-31T14:29:58Z", // oldest timestamp behind it (spot + every quote)
  "spot_source": "schwab",            // "schwab" | "tastytrade" | "yfinance" | "cache" | "statement" | "fixture"
  "option_source": "yfinance",
  "degraded": ["Schwab: token file missing", "yfinance quotes are ~15 min delayed"],
  "spot": { "symbol": "SPX", "last": 6501.25, "change": -12.5, "as_of": "..." },
  "quote_count": 14,
  "market_open": true
}
```

### `trades`
No request body. Open trades + portfolio stats.
```jsonc
{
  "as_of": "...",
  "trades": [ Trade, ... ],
  "portfolio_all": PortfolioStats,      // every open trade
  "warning_count": 3
}
```

#### Trade
```jsonc
{
  "id": "m1-2026-07-14",               // slug: name + open date
  "name": "M1",
  "status": "open",                    // "open" | "closed"
  "opened_on": "2026-07-14",           // naive date (user's designation)
  "closed_on": null,                   // date, for closed trades
  "dit": 48,                           // calendar days
  "dte": 172,                          // to front expiry of open legs; null if none
  "structure": "2x BF 7325/6975/6625 FEB27 · 1x DIAG 8200/8250 MAR/FEB27",
  "cost": 4420.0,                      // |premium committed to open| (label: debit +, credit -)
  "value": 5100.0,                     // sum of open-leg mark value
  "pnl_open": 680.0,
  "pnl_total": 247.0,                  // includes realized - fees
  "pnl_pct": 5.6,                      // % of cost basis; null when basis is 0
  "realized": -433.0,
  "fees": 3.78,
  "greeks": Greeks,                    // position greeks from OUR solved IVs
  "weighted_vega": -812.4,
  "staleness_secs": 12,                // age of the worst quote backing pnl
  "warnings": [ Warning, ... ],
  "legs": [ Leg, ... ],
  "quarantined": [ QuarantinedOrder, ... ]
}
```

#### Leg
```jsonc
{
  "contract": {
    "underlying": "SPX", "root": "SPXW", "expiry": "2026-08-21",
    "settlement": "pm", "strike": 5875.0, "right": "put",
    "multiplier": 100, "osi": "SPXW  260821P05875000", "tos_code": "SPXW260821P5875"
  },
  "qty": -4,
  "avg_cost": 22.10,                   // null for unmatched-order legs
  "mark": 18.55, "bid": 18.4, "ask": 18.7,
  "iv": 0.1712,                        // OUR solved IV
  "greeks": Greeks,                    // per-contract, NOT x qty
  "dte": 21,
  "quote_as_of": "...", "quote_source": "yfinance"
}
```

#### Greeks (units published as tooltips)
`{ "delta": -1.2, "gamma": 0.003, "theta": 55.0, "vega": -320.0 }`
delta/gamma per 1 index pt; vega per 1.00 vol pt; theta = reprice at T-1 day.

#### PortfolioStats
`{ "count": 6, "pnl_open": ..., "pnl_total": ..., "greeks": Greeks, "weighted_vega": ..., "weighted_vega_reference": "front_expiry (2026-09-18)" }`

### `trades_closed`
No request body.
`{ "trades": [Trade, ...], "totals": { "realized": ..., "fees": ..., "pnl_total": ..., "count": 8, "win_rate": 0.62, "avg_days_held": 21.5 } }`

`totals.realized` is the sum of each trade's `realized` — **pre-fee**, like
the per-row column and the sibling `fees` — and `totals.pnl_total` is the
sum of each trade's `pnl_total`, i.e. the net after fees. All three are
rounded to cents so they add up as shown.

A closed trade's P&L is banked, so this command answers on a first run with
**no market snapshot at all**: `legs: []`, zero greeks, `staleness_secs:
null`. The `no-market-data` error is for the commands a valuation is genuinely
required by — and for the rare closed trade that still holds an open leg
(`closed_trade_has_open_legs`).

### `portfolio`
`{ "request": { "ids": ["m1-2026-07-14", "m4-2026-08-04"] } }` →
`PortfolioStats` for the selected subset (empty `ids` = empty stats — unlike
`risk_graph`/`what_if`, where empty means the whole book).

### `risk_graph`
Two top-level arguments: `request` and `seq` (see above). Unlike
`portfolio`, empty `trade_ids` means the **whole open book**, not an empty
graph.
```jsonc
// request
{
  "trade_ids": ["m1-2026-07-14"],
  "price_range": null,                  // or {"min": 5800, "max": 7000} or {"pct": 0.12}
  "points": 241,
  "dates": null,                        // default preset; or ["today","+12d","+45d","expiry:2027-02-19"]
  "vol_shift": 0.0,                     // global, vol pts (0.01 = 1 vol pt)
  "vol_shift_by_expiry": {},            // {"2027-02-19": 0.02}
  "spot_override": null,
  "include_realized": true
}
// response
{
  "as_of": "...", "spot": 6501.25,
  "prices": [5800.0, ...],              // shared x-axis
  "lines": [
    // "key" is stable/unique per line ("t0", "t+12", "expiry",
    // "expiry:2027-02-19" for a later expiry) and doubles as the
    // "breakevens" key below; "label" is what the legend shows.
    { "date": "2026-08-31", "kind": "t0",     "key": "t0",     "label": "T+0",              "pnl": [ ... ] },
    { "date": "2026-10-12", "kind": "tplus",  "key": "t+12",   "label": "T+12",              "pnl": [ ... ] },
    { "date": "2027-02-19", "kind": "expiry", "key": "expiry", "label": "Exp 2027-02-19",    "pnl": [ ... ] }
  ],
  "t0_greeks": {                       // null when the date list has no T+0 line
    "key": "t0",                       // the line these belong to
    "delta": [ ... ],                  // $ per 1 index point, one per prices[i]
    "gamma": [ ... ],                  // $ of delta change per 1 index point
    "theta": [ ... ],                  // $ per calendar day (reprice at T+1d, ONE-style)
    "vega": [ ... ],                   // $ per 1.00 vol point
    "weighted_vega": [ ... ],          // $ per 1.00 vol point of the reference expiry
    "weighted_vega_reference": "front_expiry (2026-10-16)"
  },
  "per_trade": [ { "trade_id": "...", "date": "2026-08-31", "key": "t0", "pnl": [ ... ] } ],
  "breakevens": { "t0": [7180.2], "t+12": [6881.3], "expiry": [6120.5, 6893.0] },
  "max_profit_on_range": 8120.0, "max_loss_on_range": -4420.0,
  "pop": 0.61,                          // P(profit) at expiry, lognormal
  "sigma_bands": { "one": [6380.0, 6625.0], "two": [6260.0, 6750.0] },
  "front_atm_iv": 0.1585,
  "warnings": [ Warning, ... ]           // e.g. iv_solve_failed, missing_quote — model-side, keyed to a contract
}
```
Invariant: the `t0` line evaluated at spot equals dashboard `pnl_total`
(or `pnl_open` when `include_realized=false`). `breakevens` has one entry
per line, keyed by that line's `key` — not just `"expiry"`. `t0_greeks` is
the T+0 line's greeks per price, in the same units and under the same
per-leg vol shift as the curve above it; at spot they equal the dashboard's
position greeks and weighted vega.

### `what_if`
`{ "request": { "trade_ids": [...], "price": 6400, "date": "2026-09-15", "vol_shift": 0.01, "include_realized": true } }`
→ `{ "pnl": ..., "greeks": Greeks }` — recomputed at that point, same
engine. `vol_shift` and `include_realized` are both optional (default `0.0`
and `true`). As with `risk_graph`, empty `trade_ids` means the whole open
book (not empty, unlike `portfolio`).

### `reconciliation`
No request body.
```jsonc
{
  "statement_as_of": "2026-08-28",
  "rows": [ { "tos_code": "SPXW260918P5050", "statement_qty": -1, "trades_qty": -1,
              "status": "match" } ],     // "match" | "qty_mismatch" | "unassigned" | "missing"
  "note": "Per-trade attribution intentionally differs from broker/tax lots.",
  "source_note": null   // additive, nullable; see below
}
```
`note` is the frozen sentence above, unchanged. A sheet or CSV book has no
account statement behind it, so `rows` is empty and `source_note` carries
the honest sentence instead: "this book was imported from a Legs sheet;
there is no account statement to reconcile against" (or the CSV
equivalent).

### `warnings`
No request body.
`{ "warnings": [Warning, ...] }`

#### Warning
`{ "severity": "warn", "code": "duplicate_execution_claim", "message": "...", "trade_id": "lpta-2026-07-24", "contract": "SPXW260731P6325" }`
(severity: `"info" | "warn" | "error"`; trade_id/contract nullable)

`code` is an open set — a client must render an unknown one, never drop it.
The codes emitted today, by severity:

| severity | codes |
|---|---|
| `error` | `reconciliation_mismatch`, `order_parse_failed`, `statement_parse_failed`, `statement_column_missing`, `sheet_column_missing` |
| `info` | `statement_section_skipped`, `settlement_inferred`, `vol_fallback`, `forward_fallback`, `sheet_value_disagrees`, `sheet_pnl_disagreement`, `sheet_duplicate_column`, `sheet_header_offset` |
| `warn` | everything else: `unmatched_order`, `date_outside_window`, `duplicate_execution_claim`, `duplicate_order_in_trade`, `duplicate_trade_id`, `hint_not_satisfied`, `closed_trade_has_open_legs`, `expired_leg_unknown_settlement`, `unassigned_account_position`, `trade_file_skipped`, `statement_row_skipped`, `execution_fees_unlinked`, `cash_trade_unlinked`, `duplicate_statement`, `non_utf8_file`, `iv_solve_failed`, `iv_clamped`, `missing_quote`, `sheet_column_absent`, `sheet_row_skipped`, `sheet_leg_unpriced`, `sheet_close_price_missing`, `sheet_trade_instance_unverified` |

`iv_solve_failed`, `iv_clamped`, `missing_quote`, `vol_fallback` and
`forward_fallback` are model-side (a valuation, not a file) and carry no
`trade_id`; every one but `forward_fallback` names a `contract`. The rest
come from the book.

The ten `sheet_*` codes are canonical here and in `docs/DATA-FORMATS.md` —
byte-identical in both files:

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

Marks from the sheet are never used for pricing — live providers only; the
`Mark` column is a cross-check.

Some codes are now **folder-sourced only** — emitted by `wingspan-cli
export-legs` and the core tests, never by a sheet or CSV book:
`reconciliation_mismatch`, `order_parse_failed`, `statement_parse_failed`,
`statement_column_missing`, `statement_section_skipped`,
`statement_row_skipped`, `unmatched_order`, `date_outside_window`,
`duplicate_execution_claim`, `duplicate_order_in_trade`,
`hint_not_satisfied`, `unassigned_account_position`, `trade_file_skipped`,
`execution_fees_unlinked`, `cash_trade_unlinked`, `duplicate_statement`,
`non_utf8_file`. The thinkorswim parsers stay in `wingspan-core` as
test-only code, reached from the core tests and from `wingspan-cli
export-legs`; there is no migration path from TOS folders and none is
needed.

Still emitted on a sheet or CSV book: `duplicate_trade_id`,
`closed_trade_has_open_legs`, `expired_leg_unknown_settlement`,
`settlement_inferred`, and the model-side `iv_solve_failed`, `iv_clamped`,
`missing_quote`, `vol_fallback`, `forward_fallback`.

### `app_config`
No request body. The whole `wingspan.toml` shape (`docs/CONFIG.md`); no
secret is present in it — the account number lives only in the Schwab token
file and the credentials only in the files `[schwab] env_file` and
`[tastytrade] env_file` name, none of which is part of the config body. The
config gained a `tastytrade` block (additively, between `schwab` and
`yfinance`); it holds variable *names*, a path, three booleans and a version
string. Plus a `resolved` object:
`source: { kind, label, tab, fetched_at, stale }`, `state_dir`,
`valuation_date`, `demo`, `version`, `provider_notes`. The full sheet id is
never present in `resolved.source` — only the masked `label`, exactly as it
is masked everywhere else.

## The book

Boot **publishes the cached book first and refetches in the background**,
emitting `trades_changed { "reason": "fetch" }` only when the digest
changed; an offline boot answers from the cache with `"stale": true`.

### `reload`
No request body. Re-fetch the sheet (or re-read the CSV file) and
re-assemble the book now. →
`{ "reloaded": true, "trades": 6 }` — `trades` is the open-trade count after
the reload (closed trades are not counted).

A reload fails for the same reasons `set_trade_source` does: `sheet-
unreachable` (a network error, a timeout, a 5xx, or a response past the
size limit), `sheet-not-shared` (redirected to `accounts.google.com`, or a
401/403), `sheet-tab-not-found` (a 200 carrying gviz's own error payload),
or `invalid-trade-source` (a CSV Wingspan can no longer parse, or a
required column no longer in the header). In every one of those cases the
previous book **stays on screen** — a reload never blanks the dashboard on
a bad fetch. Nothing loaded at all is not a failure — it reloads to an
empty book. `invalid-trades-root` is retired.

### `trade_source`
No request body. Which book is loaded and how that was decided. A sheet is
fetched over the network on each reload; a CSV file is read **in place** —
nothing is ever copied onto this machine.
```jsonc
{ "kind": "sheet",            // "sheet" | "csv" | "config" | "demo" | "empty"  ("manual" reserved)
  "label": "docs.google.com/…/1AbCdE…abcd · Legs",  // masked; the full id is never here
  "tab": "Legs",              // null for a CSV source
  "path": null,               // the CSV's full path when kind is "csv"; null otherwise
  "open_trades": 6, "closed_trades": 2, "legs": 39, "rows_skipped": 2,
  "chosen_at": "2026-09-16T15:04:00Z",   // when this source was imported
  "fetched_at": "2026-09-16T15:04:02Z",  // when its bytes were last read
  "stale": false,             // true when the book on screen came from the cache
  "detail": null,             // one sentence when the last fetch failed
  "auto_refresh_secs": null,  // [trades] poll_secs when polling is on
  "recent": [ { "kind": "sheet", "label": "docs.google.com/…/1AbCdE…abcd · Legs", "tab": "Legs" } ] }
```
`"kind": "empty"` with `"label": null` is not an error: a fresh install has
no source until one is imported.

`recent` is the sources loaded before, most recent first, de-duplicated and
capped at five; a CSV entry whose file no longer exists is pruned before
the list is sent.

**The share link is bearer-equivalent.** The full sheet id is never in this
DTO, never in `app_config`, never in `settings_read` and never in the log —
only the masked `label` is, exactly as `[schwab] token_path`'s secret is
handled.

### `probe_trade_source`
```jsonc
{ "request": { "source": { "kind": "sheet", "url": "https://docs.google.com/spreadsheets/d/1AbCdEfGhIjKlMnOpQrStUvWxYz0123456789abcd/edit#gid=123456789", "tab": "Legs" } } }
{ "request": { "source": { "kind": "csv",   "path": "/home/trader/Downloads/legs.csv" } } }   // path only ever from pick_csv_file
{ "request": { "source": { "kind": "manual" } } }                                             // reserved — 422 today
```
→
```jsonc
{ "label": "docs.google.com/…/1AbCdE…abcd · Legs", "tab": "Legs",
  "columns_found":   ["Trade","Side","Qty","OCC Symbol","Opened","Open Price","Closed","Close Price","Status","Fees","Mark","Expiry","#","Key"],
  "columns_missing": [],            // required columns not in the header — non-empty means the import would be refused
  "columns_absent":  [],            // expected columns missing or entirely blank
  "columns_ignored": ["Days","Band"],
  "rows": 41, "legs": 39, "rows_skipped": 2,
  "trades": { "open": 6, "closed": 2 },
  "warnings": [ /* Warning */ ],
  "note": null }
```
What the source holds if it were imported. **Persists nothing, loads
nothing, publishes nothing** — it is the "is this the sheet you meant?"
half of a two-step choice, so a wrong paste costs a glance instead of the
book already swapped. `columns_found` names every logical column the
header carries; `columns_missing` are required columns (`Trade`, `Side`,
`Qty`, `OCC Symbol`) not found at all — non-empty here means the import
would be refused; `columns_absent` are expected columns (`Opened`, `Open
Price`) missing or entirely blank — the import is still accepted, without
cost basis or DIT; `columns_ignored` are header columns Wingspan does not
read at all. `note` is absent unless there is something worth reading
first.

### `set_trade_source`
Takes the same request `probe_trade_source` takes (see above). Persists to
`<state_dir>/trade_source.json` (mode `0600`) **before** anything moves,
then applies like a restart: new book, new quote universe, a
`trades_changed { "reason": "reload" }` event, the cached market snapshot
dropped and a refresh requested at once. → the same shape `trade_source`
answers.

The previous source is left exactly as it was on failure. Errors:

| status | slug | when |
|---|---|---|
| 422 | `sheet-not-shared` | the fetch was redirected to `accounts.google.com`, or answered 401/403 — the sheet is not shared with anyone-with-the-link |
| 422 | `sheet-tab-not-found` | the fetch answered 200 with gviz's own error payload — no tab of that name in that spreadsheet |
| 422 | `sheet-unreachable` | a network error, a timeout, a 5xx, or a response past the size limit; the previous book stays on screen |
| 422 | `invalid-trade-source` | a URL or path Wingspan will not use (bad shape, a host that is not `docs.google.com`), CSV it cannot parse, or a required column the header does not carry |
| 500 | `internal` | a state directory that cannot be written |

`{ "kind": "manual" }` is reserved for a future manual leg-entry seam and
also answers `invalid-trade-source` today.

**No user-typed string is ever fetched verbatim:** Rust extracts the
spreadsheet id and builds the request URL itself, and only
`docs.google.com` is reachable. **The webview never talks to Google** —
`tauri.conf.json`'s CSP `connect-src` forbids it.

### `clear_trade_source`
No request body. **Clear** — Wingspan stops showing the trades; nothing of
the user's is touched. → the same shape `trade_source` answers, with
`"kind": "empty"`.

**No byte of your sheet or your CSV file is read, moved, renamed or
removed** — the whole command touches only `<state_dir>`: the pointer in
`trade_source.json` is forgotten (the cleared source going to the front of
`recent`, so loading it again is one click), the cached copy
(`trade_source_cache.csv` + `trade_source_cache.json`, both `0600`) is
deleted, and the cached market snapshot is dropped, because its quote
universe was that book's own contracts. The book then falls back to
`[trades]` config, to the demo book (if `load_demo` is in effect), or to
nothing (`"kind": "empty"`). A `trades_changed` event follows.

With nothing to fall back to, Clear still succeeds — stopping the display
is the point. Idempotent: clearing with nothing loaded changes nothing.

### `pick_csv_file`
No request body. `async` — opens the native file chooser and returns
`{ "path": "…" }`, or `null` on cancel. `null` means a cancel and nothing
else: a file the user really chose that this computer has no path for is a
422 (`unprocessable`) naming the reason — the slug the retired directory
picker answered for the same case — never a silent `null` the SPA would
read as a cancel. Beyond that it fails only when the chooser itself never
answers (500). Every outcome — the chooser opening, the path chosen, a
cancel, a refusal — is one `info`/`warn` line in the log, so a pick that
goes wrong on a machine nobody can attach to still says where it went.
Desktop-only; opened from Rust so an XSS in the webview cannot summon a
file chooser on its own (there is no `dialog:allow-open` permission in
`capabilities/main.json`). `pick_csv_file` is the sole native picker for the
trade source, and the only writer of `[trades] csv_path`.

### `open_sheet`
No request body. Desktop-only; opens the active sheet's real URL in the
system browser. The URL is rebuilt Rust-side from the stored source — it
is never handed to JS. Answers only when the current source is `"kind":
"sheet"`.

### `load_demo`
No request body. Show the bundled synthetic book. Ignores and never
touches `trade_source.json` — pricing the user's real positions off
synthetic fixtures because of a stray menu click would be a lie the app
told them — so `leave_demo` restores the real source exactly as it was. →
the same shape `trade_source` answers.

### `leave_demo`
No request body. Put the real book back: the saved source is honoured
again, the clock is the system clock, and the credentials are the ones the
process booted with. → the same shape `trade_source` answers.

## Settings & diagnostics

*(Desktop-only — none of these have a `wingspan-devserver` route.)*

### `settings_read`
No request body. Returns the file `wingspan.toml` was read from (`null` if
none exists yet — the app is on built-in defaults and Save will create
one), the resolved settings as a fully-populated patch (every field
`Some`, so the form always shows what the app is actually using), the
read-only paths the settings card displays (`source_label`, `token_path`,
`env_file`, `python`, `tastytrade_env_file`), and three provider status
blocks.

`token_status` **is Schwab's, and stays Schwab's** — the name is frozen. It is
`null` when no Schwab provider is configured; otherwise
`{ present, world_readable, detail }` — read-only, never a token.

`tastytrade_status` is its sibling, not a generalisation of it. `null` when
tastytrade is disabled or unconfigured; otherwise
`{ configured, sandbox, delayed, authorized, detail }` — read-only and **never
a credential**, not even a masked one. `detail` is one sentence: the cooldown
deadline while the failed-login guard is holding, else the last error, else
whether an access token is in hand. Reading it costs no I/O and never runs a
token exchange.

`source_status` is the trade source's sibling status block: `{ kind,
configured, host, tab } | null` — `host` is `"docs.google.com"` or `null`,
**never the id**. Reading it costs no I/O and performs no fetch. The share
link is bearer-equivalent and is handled like `[schwab] token_path`: never
on a settings patch, never returned by `settings_read`, masked in every DTO
and in the log.

### `settings_update`
`{ "request": <SettingsPatch> }` — every field optional; `None` leaves that
key exactly as it is, comment and all. Validated *before* the file is
touched (a patch that would not load is refused with the config's own
sentence and `wingspan.toml` is never written), then written with
`toml_edit` so comments, key order and unrelated keys survive, then the
whole application is rebuilt on the file that was just written. →
`{ "file": "/home/trader/.config/wingspan/wingspan.toml" }`.

`SettingsPatch` carries **only non-path keys** — `trades`, `schwab` (minus
`token_path`/`env_file`/`token_config`), `tastytrade` (minus `env_file`),
`yfinance` (minus `python`/`script`), `market` (minus `state_dir`),
`pricing`, `risk_graph` — and is `deny_unknown_fields`, so a payload naming
a path key is rejected by serde before any code runs. `SettingsPatch.trades`
is **exactly** `{ tab, poll_secs, timezone, cache }` — nothing else;
`[trades] sheet_url` and `[trades] fetch_timeout_secs` are config-file-only
and never on the patch, `sheet_url` because a share link is
bearer-equivalent (masked and handled like `[schwab] token_path`), and
`fetch_timeout_secs` for the same reason `[schwab] token_config` is. The path
fields are written only by the five pickers below, each behind its own
confirmation dialog. The fifth is `pick_csv_file` (documented with the book
above), the only writer of `[trades] csv_path`.

A `[market]` source list **carried by the patch** that is **empty**, or that
names something which is not a provider in this build, is also refused — the
patch is rejected with a sentence naming the key and listing the legal names
and `wingspan.toml` is not written. The legal names are `schwab`,
`tastytrade`, `yfinance` and `cache` for `spot_sources`, and those plus
`statement` for `option_sources` (statement marks are registered option-only;
`fixture` is registered only by `--demo`, which writes its own ladders, so it
is legal in neither). A list the patch does not carry is not examined, so a file
hand-edited to the same mistake still loads, still boots and still saves; the
`diagnostics` checklist warns about the unknown name there instead.

### `pick_python` / `pick_env_file` / `pick_token_file` / `pick_tastytrade_env_file`
No request body. Each opens a native file chooser, shows a confirmation
dialog naming the chosen path and stating what Wingspan will do with it,
and — only on confirmation — writes that path string into `wingspan.toml`
and rebuilds the app on it. → `{ "path": "/usr/bin/python3", "applied":
true, "detail": null }`, or `null` on cancel at either step — and, as with
`pick_csv_file`, only on a cancel: a chosen file with no path on this computer
is a 422, not a `null`. `applied` is
`false` when the file was written but this session could not be rebuilt on
it (the previous settings are still in force and `detail` says why); the
file is what the next launch reads either way. None of the four ever opens,
copies, chmods or creates the file itself; `pick_token_file` and
`pick_tastytrade_env_file` in particular write only the path string —
`[schwab] token_path` and `[tastytrade] env_file` respectively.

### `probe_python`
No request body. Runs the diagnostics checklist's own yfinance row (with
`probe`, so the interpreter really is spawned) and returns just that row:
`{ "ok": true, "detail": "… → yfinance 1.7.0" }`. **Warns, never fails** — a
machine with no `yfinance` is a supported configuration, so a missing
interpreter answers `{ "ok": false, "detail": "…" }`, never a rejected
promise.

### `diagnostics`
`{ "request": { "probe": false } }` — `probe` runs the checks that cost
something: the configured Python interpreter and the live providers (the
same thing `wingspan-cli doctor --probe` does). Without it those rows come
back as `"note"` rows saying they were not checked. →
```jsonc
{
  "rows": [ { "grade": "ok", "section": "Trades", "name": "root", "detail": "..." }, ... ]
}
```
`grade` is one of `"ok" | "warn" | "fail" | "note"`.

## Shell

### `reveal_folder`
`{ "request": { "path": "..." } }` — opens a folder in the desktop's file
manager. Answers only for one of three paths the running application
already knows: the folder holding the active CSV source (only when the
source is a CSV file), the log directory, and the directory holding
`wingspan.toml` — anything else is refused with `unprocessable`, so an XSS
cannot turn this into a general "open anything" primitive. A sheet source
has no revealable folder.

### `open_log_folder`
No request body. Opens the log directory.

### `open_config_file`
No request body. Opens `wingspan.toml` itself, or (when none exists yet)
the folder it would be created in.

### `check_for_updates`
No request body, no response body. Opens the GitHub Releases page in the
system browser. There is no updater plugin and nothing is downloaded or
installed by this command.

## Errors

A **failed** command rejects with the serialised `problem+json` struct — an
**object** — `{ "type": "https://wingspan.invalid/problems/<slug>", "title": ..., "status": <n>, "detail": "..." }`.
`type` is a deliberately unresolvable URN; nothing has to fetch it to
understand the error. The slugs, beyond the per-command notes above:

| status | slug | when |
|---|---|---|
| 400 | `bad-request` | a value the engine cannot use (e.g. risk-graph `points < 2`, a non-positive what-if `price`) |
| 404 | `not-found` | no such trade id |
| 422 | `sheet-not-shared` | the fetch was redirected to `accounts.google.com`, or answered 401/403 — the sheet is not shared with anyone-with-the-link |
| 422 | `sheet-tab-not-found` | the fetch answered 200 with gviz's own error payload — no tab of that name in that spreadsheet |
| 422 | `sheet-unreachable` | a network error, a timeout, a 5xx, or a response past the size limit; the previous book stays on screen |
| 422 | `invalid-trade-source` | a URL or path Wingspan will not use (bad shape, a host that is not `docs.google.com`), CSV it cannot parse, or a required column the header does not carry |
| 422 | `out-of-range` | `risk_graph` with `points` > 2001 or more than 32 `dates`; `risk_graph`/`what_if` with a date more than 20000 days from today (a graph nothing could draw, and an offset chrono would overflow on) |
| 422 | `unprocessable` | a file the native chooser returned that this computer has no path for, or more than one file; a `reveal_folder` path outside its allow-list |
| 422 | `settings` | a settings patch that fails validation; a picked file outside what a picker allows, with no path on this computer, or more than one file |
| 500 | `internal` | a command's own task panicked, or a filesystem operation Wingspan cannot recover from |
| 503 | `no-market-data` | no snapshot to value against — see `trades_closed` for the exception |

Two more shapes exist only on the dev harness, whose HTTP body extractor the
desktop `invoke` transport has no counterpart for: `invalid-request-body` at
400 (malformed JSON), 415 (a missing or wrong `Content-Type`) or 422 (valid
JSON of the wrong shape), and a bare 413 for a body past axum's default size
limit.

A **refused, misspelled, or wrongly-typed** command rejects with a **bare
string**, not an object — the Tauri ACL layer's own error, before any Rust
command body ever runs:

```
"pick_csv_file not allowed. Command not found"
"invalid args `request` for command `what_if`: missing field `date`"
```

**The client must accept both shapes.** `web/src/api/client.ts`'s
`toApiError` is the one place that does: an object carrying `status`+
`detail` (or `type`+`title`) becomes an `ApiError` from the parsed problem;
anything else becomes an `ApiError` from `String(raw)`.

## Events

Tauri events, not a stream — the SPA calls `listen()` for each.

| event | payload | when |
|---|---|---|
| `snapshot` | the `market_snapshot` body | every successful refresh |
| `provider` | `{ "spot_source": ..., "option_source": ..., "degraded": [...] }` | only when the *source* or degradation reasons change (so the status pill does not flicker) |
| `trades_changed` | `{ "reason": "fetch" \| "reload" }` | `"fetch"`: a background refetch (boot's background refetch, or `[trades] poll_secs`) found a **changed digest**; `"reload"`: `reload` ran, or the source changed |
| `heartbeat` | `{}` | every 15 s while the application is running; 45 s of silence puts the status dot out |
| `app_ready` | none | boot, or a rebuild (a settings write, entering/leaving the demo), finished successfully |
| `app_error` | `{ "detail": "..." }` | boot, or a rebuild, failed |

An unchanged digest emits nothing — a background refetch or a poll tick
that reads the same bytes back is silent. Nothing inspects the filesystem
for changes any more, so `"fs"` is gone: the book refreshes on Reload, on a
source change, and on `[trades] poll_secs` if you set one.

`health.ready` is `false` until the first `app_ready` — before that, every
other command pends rather than erroring (see "The pending-boot `AppSlot`"
in `docs/ARCHITECTURE.md`).

## Appendix: command → `wingspan-devserver` route

`wingspan-devserver` is the **dev-only** HTTP front end Playwright drives —
it is never bundled or shipped. Where a command has an HTTP equivalent
there, this is it; the desktop-only commands (settings, diagnostics, demo,
pickers, shell) have none.

| command | `wingspan-devserver` route |
|---|---|
| `health` | `GET /api/v1/health` |
| `market_snapshot` | `GET /api/v1/market/snapshot` |
| `trades` | `GET /api/v1/trades` |
| `trades_closed` | `GET /api/v1/trades/closed` |
| `portfolio` | `GET /api/v1/portfolio?ids=...` |
| `risk_graph` | `POST /api/v1/risk-graph` |
| `what_if` | `POST /api/v1/what-if` |
| `reconciliation` | `GET /api/v1/reconciliation` |
| `warnings` | `GET /api/v1/warnings` |
| `app_config` | `GET /api/v1/config` |
| `reload` | `POST /api/v1/reload` |
| `trade_source` | `GET /api/v1/trades/source` |
| `probe_trade_source` | `POST /api/v1/trades/source/probe` |
| `set_trade_source` | `PUT /api/v1/trades/source` |
| `clear_trade_source` | `DELETE /api/v1/trades/source` |
| `pick_csv_file`, `open_sheet`, `load_demo`, `leave_demo`, `settings_read`, `settings_update`, `pick_python`, `pick_env_file`, `pick_token_file`, `pick_tastytrade_env_file`, `probe_python`, `diagnostics`, `reveal_folder`, `open_log_folder`, `open_config_file`, `check_for_updates` | *(none — desktop-only)* |

`wingspan-devserver serve --demo [--legs <csv>]` is the new invocation; the
flag that used to point at a directory of TOS exports is gone.

Events map the same way: `GET /api/v1/events` (SSE) carries `snapshot`,
`trades_changed` and `provider` with identical payloads; `heartbeat`,
`app_ready` and `app_error` exist only on the desktop transport.
