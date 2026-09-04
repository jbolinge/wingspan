# Wingspan HTTP API — v1 (frozen contract)

Base path `/api/v1`. JSON everywhere; errors are RFC 9457
`application/problem+json` (`{type, title, status, detail}`). Money fields are
JSON numbers serialized from `Decimal`; vols/greeks are floats. Timestamps are
RFC 3339 UTC unless noted. This contract is shared by `wingspan-devserver`
(the development-only HTTP front end) and `web/` — the desktop app answers the
same shapes over Tauri commands. Change it only by editing this file first.

## GET /api/v1/health
`{ "status": "ok", "version": "0.1.0", "ready": true }`

`ready` is always `true` here — the HTTP server only answers once the
application state exists — and is in the body because the desktop shell
answers the same question while it is still starting up, and one field serves
both.

## GET /api/v1/market/snapshot
```jsonc
{
  "as_of": "2026-08-31T14:30:02Z",
  "spot_source": "schwab",            // "schwab" | "yfinance" | "cache" | "statement" | "fixture"
  "option_source": "yfinance",
  "degraded": ["Schwab: token file missing", "yfinance quotes are ~15 min delayed"],
  "spot": { "symbol": "SPX", "last": 6501.25, "change": -12.5, "as_of": "..." },
  "quote_count": 14,
  "market_open": true
}
```
(The full per-contract quote list stays server-side; legs carry their own
quote fields below.)

## GET /api/v1/trades
Open trades + portfolio stats.
```jsonc
{
  "as_of": "...",
  "trades": [ Trade, ... ],
  "portfolio_all": PortfolioStats,      // every open trade
  "warning_count": 3
}
```

### Trade
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

### Leg
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

### Greeks  (units published as tooltips)
`{ "delta": -1.2, "gamma": 0.003, "theta": 55.0, "vega": -320.0 }`
delta/gamma per 1 index pt; vega per 1.00 vol pt; theta = reprice at T-1 day.

### PortfolioStats
`{ "count": 6, "pnl_open": ..., "pnl_total": ..., "greeks": Greeks, "weighted_vega": ..., "weighted_vega_reference": "front_expiry (2026-09-18)" }`

## GET /api/v1/trades/closed
`{ "trades": [Trade, ...], "totals": { "realized": ..., "fees": ..., "pnl_total": ..., "count": 8, "win_rate": 0.62, "avg_days_held": 21.5 } }`

`totals.realized` is the sum of each trade's `realized` — **pre-fee**, like the
per-row column and the sibling `fees` — and `totals.pnl_total` is the sum of
each trade's `pnl_total`, i.e. the net after fees. All three are rounded to
cents so they add up as shown.

A closed trade's P&L is banked, so this route (and `GET /trades/{id}` for a
trade holding nothing) answers on a first run with **no market snapshot at
all**, with `legs: []`, zero greeks and `staleness_secs: null`. The 503 stays
for the routes a valuation is genuinely required by — and for the rare closed
trade that still holds an open leg (`closed_trade_has_open_legs`).

## GET /api/v1/trades/{id}
One `Trade` (open or closed) with full legs.

## GET /api/v1/portfolio?ids=m1-2026-07-14,m4-2026-08-04
`PortfolioStats` for the selected subset (empty ids = empty stats).

## POST /api/v1/risk-graph
Unlike `/portfolio`, empty `trade_ids` means the **whole open book**, not an
empty graph.
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
(or `pnl_open` when `include_realized=false`). `breakevens` has one entry per
line, keyed by that line's `key` — not just `"expiry"`.

## POST /api/v1/what-if
`{ "trade_ids": [...], "price": 6400, "date": "2026-09-15", "vol_shift": 0.01, "include_realized": true }`
→ `{ "pnl": ..., "greeks": Greeks }` — recomputed at that point, same engine.
`vol_shift` and `include_realized` are both optional (default `0.0` and
`true`). As with `/risk-graph`, empty `trade_ids` means the whole open book
(not empty, unlike `/portfolio`).

## GET /api/v1/reconciliation
```jsonc
{
  "statement_as_of": "2026-08-28",
  "rows": [ { "tos_code": "SPXW260918P5050", "statement_qty": -1, "trades_qty": -1,
              "status": "match" } ],     // "match" | "qty_mismatch" | "unassigned" | "missing"
  "note": "Per-trade attribution intentionally differs from broker/tax lots."
}
```

## GET /api/v1/warnings
`{ "warnings": [Warning, ...] }`
### Warning
`{ "severity": "warn", "code": "duplicate_execution_claim", "message": "...", "trade_id": "lpta-2026-07-24", "contract": "SPXW260731P6325" }`
(severity: `"info" | "warn" | "error"`; trade_id/contract nullable)

`code` is an open set — a client must render an unknown one, never drop it.
The codes emitted today, by severity:

| severity | codes |
|---|---|
| `error` | `reconciliation_mismatch`, `order_parse_failed`, `statement_parse_failed`, `statement_column_missing` |
| `info` | `statement_section_skipped`, `settlement_inferred`, `vol_fallback`, `forward_fallback` |
| `warn` | everything else: `unmatched_order`, `date_outside_window`, `duplicate_execution_claim`, `duplicate_order_in_trade`, `duplicate_trade_id`, `hint_not_satisfied`, `closed_trade_has_open_legs`, `expired_leg_unknown_settlement`, `unassigned_account_position`, `trade_file_skipped`, `statement_row_skipped`, `execution_fees_unlinked`, `cash_trade_unlinked`, `duplicate_statement`, `non_utf8_file`, `iv_solve_failed`, `iv_clamped`, `missing_quote` |

`iv_solve_failed`, `iv_clamped`, `missing_quote`, `vol_fallback` and
`forward_fallback` are model-side (a valuation, not a file) and carry no
`trade_id` on `/warnings`; every one but `forward_fallback` names a `contract`.
The rest come from the book.

## GET /api/v1/trades/root
Which book is loaded and how that was decided. Trade files live where the user
keeps them and are read **in place** — nothing is ever copied onto this
machine.
```jsonc
{
  "root": "/home/trader/TOS Exports/Trades",  // active root, resolved; null when empty
  "source": "local",                   // "local" | "config" | "demo" | "empty"
  "config_root": "/home/user/Trades",  // what [trades] root alone would use; null if unset
  "open_trades": 6, "closed_trades": 2,
  "chosen_at": "2026-09-03T15:04:00Z", // only while source is "local"
  "watching": true,                    // always present; live, not install-time
  "recent": ["/home/trader/Archive/2025"]   // always present, may be empty
}
```
`"source": "empty"` with `"root": null` is not an error: no folder has been
chosen and no `[trades] root` is set, so the book is empty until one is.

`watching` is whether an edit in that folder will reach the dashboard by
itself. It is read from the live watch, not from whether one was installed at
boot: an unreadable or unmounted folder, or an exhausted
`fs.inotify.max_user_watches`, reads `false` here and puts the reason in
`GET /config`'s `provider_notes`.

`recent` is the folders loaded before, most recent first, de-duplicated and
capped at five; entries that are no longer directories are pruned before the
list is sent.

## PUT /api/v1/trades/root

```jsonc
// request
{ "root": "/home/trader/TOS Exports/Trades" }
```
Validated exactly as start-up validates (`~` expanded, canonicalized, the same
one-level discovery descent), persisted to `<state_dir>/trades_root.json`
**before** anything moves, then applied like a restart: new book, new quote
universe, new statement source, SSE `trades_changed {"reason": "reload"}`, the
watcher moved onto the new folder, the cached market snapshot dropped and a
refresh requested at once. → the GET body.

The previous root is left exactly as it was on failure. Errors:

- **422** `invalid-trades-root` — the discovery sentence ("no trades folders
  under …"), or `that folder is inside Wingspan's own state directory`, or
  `that folder contains a link outside itself`, or `<path>: not a folder I can
  open (…)`.
- **500** `internal` — a state directory that cannot be written.

## POST /api/v1/trades/root/probe

```jsonc
// request
{ "root": "/home/trader/TOS Exports" }
// response — FolderProbe
{
  "root": "/home/trader/TOS Exports/Trades",  // the discovered root
  "open_trades": 6, "closed_trades": 2, "statements": 14,
  "note": "the trades layout is one level down, in /home/trader/TOS Exports/Trades"
}
```
What a folder holds if it were loaded. **Persists nothing, reloads nothing,
rewatches nothing, publishes nothing** — it is the "is this the folder you
meant?" half of the two-step choice, so a wrong pick costs a glance rather
than the book already loaded. `note` is absent unless there is something worth
reading first: the one-level descent, or `no statements in that folder — the
reconciliation will be empty`. Same 422s as `PUT`.

## DELETE /api/v1/trades/root
**Clear** — stop showing these trades. → the GET body.

**No file, directory or byte inside the user's own folder is read, moved,
renamed or removed.** The whole command touches only `<state_dir>`: the
pointer in `trades_root.json` is forgotten (the folder's own path going to the
front of `recent`, so loading it again is one click) and the cached market
snapshot is dropped, because its quote universe was that book's own contracts.
The book then falls back to `[trades] root`, to `--demo`'s book, or to nothing
(`"source": "empty"`). SSE `trades_changed`.

With nothing to fall back to the clear still succeeds — stopping the display
is the point — so this route has no 404. Idempotent: clearing with nothing
loaded is a 200 that changes nothing.

## GET /api/v1/config
The whole `wingspan.toml` shape (`docs/CONFIG.md`) with secrets redacted
(`"auth_token": "***"`, account number never present — it lives only in the
Schwab token file), plus a `resolved` object: `trades_root`, `open_dir`,
`closed_dir`, `statements_dir` (all `null` when no book is loaded),
`state_dir`, `valuation_date`, `demo`, `version`, `provider_notes`.

## POST /api/v1/reload
Re-scan the trades dir now. → `{ "reloaded": true, "trades": 6 }` — `trades`
is the open-trade count after the rescan (closed trades are not counted).

A reload fails for the same reasons a start-up does: a trades folder renamed,
emptied or unmounted since boot → **422** `invalid-trades-root` with the
discovery sentence as `detail`; a broken config → 500. Nothing loaded at all
is not a failure — it reloads to an empty book.

## GET /api/v1/events  (SSE)
Events: `snapshot` (payload = /market/snapshot body), `trades_changed`
(payload = `{"reason": "fs" | "reload"}`), `provider`
(payload = `{"spot_source": ..., "option_source": ..., "degraded": [...]}`).

## Errors
Every failure is `application/problem+json`, and `type` is
`https://wingspan.invalid/problems/<slug>` — a deliberately unresolvable URN,
so nothing has to fetch it to understand the error. The slugs, beyond the
per-route notes above:

| status | slug | when |
|---|---|---|
| 400 | `bad-request` | a value the engine cannot use (e.g. risk-graph `points < 2`, a non-positive what-if `price`) |
| 403 | `untrusted-host` | a `Host` that is neither `localhost` nor a loopback IP. `detail` names it and says how to reach the instance instead. Checked on **every** request, ahead of the SPA, because a DNS-rebound name must not even read. |
| 403 | `cross-origin` | a POST/PUT/PATCH/DELETE carrying an `Origin` that is foreign or `null` (a sandboxed frame, a `file://` page). GETs are not checked: a forged one reads nothing back. |
| 404 | `not-found` | no such trade id |
| 422 | `invalid-trades-root` | the discovery sentence, or one of the folder refusals, on `PUT /trades/root`, `POST /trades/root/probe` and `POST /reload` |
| 422 | `out-of-range` | `POST /risk-graph` with `points` > 2001 or more than 32 `dates`; `POST /risk-graph`/`/what-if` with a date more than 20000 days from today (a graph nothing could draw, and an offset chrono would overflow on) |
| 503 | `no-market-data` | no snapshot to value against — see `/trades/closed` for the exception |

## Static
Everything not under `/api` serves the embedded SPA (feature `embed-ui`),
with SPA fallback to `index.html`.
