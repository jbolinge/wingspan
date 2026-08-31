# Wingspan HTTP API — v1 (frozen contract)

Base path `/api/v1`. JSON everywhere; errors are RFC 9457
`application/problem+json` (`{type, title, status, detail}`). Money fields are
JSON numbers serialized from `Decimal`; vols/greeks are floats. Timestamps are
RFC 3339 UTC unless noted. This contract is shared by `wingspan-server` and
`web/` — change it only by editing this file first.

## GET /api/v1/health
`{ "status": "ok", "version": "0.1.0" }`

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
`{ "trades": [Trade, ...], "totals": { "realized": ..., "fees": ..., "count": 8, "win_rate": 0.62, "avg_days_held": 21.5 } }`

## GET /api/v1/trades/{id}
One `Trade` (open or closed) with full legs.

## GET /api/v1/portfolio?ids=m1-2026-07-14,m4-2026-08-04
`PortfolioStats` for the selected subset (empty ids = empty stats).

## POST /api/v1/risk-graph
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
`true`).

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

## GET /api/v1/trades/root
Where the trade files are read from and how that was decided.
```jsonc
{
  "root": "/home/user/Trades",         // the active root (resolved)
  "source": "config",                  // "config" | "override" | "demo"
  "config_root": "/home/user/Trades",  // what config alone would use; null if unset
  "open_trades": 6, "closed_trades": 2
}
```

## PUT /api/v1/trades/root
`{ "root": "/some/folder" }` — validate the folder with the same discovery
rules as startup (case-insensitive dir names, one-level descent), reload the
book, emit SSE `trades_changed`, and persist the choice in the state dir
(`trades_root.json`) so it survives restarts. The user's `wingspan.toml` is
never rewritten. On success → the GET body above (`"source": "override"`).
Invalid folder → **422** problem+json whose `detail` is the discovery error
sentence.

## DELETE /api/v1/trades/root
Drop the override, return to the config root (404 problem+json if config has
none and the server was started with an override/demo root), reload, SSE
`trades_changed`. → the GET body.

## GET /api/v1/fs/dirs?path=/home/user&hidden=1
Server-side directory listing for the folder picker. Directories only, never
file contents. `path` omitted → the active root's parent, else the home dir.
`hidden` (default off) includes dot-directories. Unreadable/nonexistent path
→ 422 problem+json.
```jsonc
{
  "path": "/home/user",
  "parent": "/home",                    // null at filesystem root
  "home": "/home/user",
  "entries": [
    { "name": "Trades", "path": "/home/user/Trades", "has_trades_layout": true },
    { "name": "src",    "path": "/home/user/src",    "has_trades_layout": false }
  ]
}
```
`has_trades_layout` = the discovery probe (the three subfolders, one-level
descent allowed) succeeds there. Like every route, this sits behind the
bearer token whenever the bind is non-loopback.

## GET /api/v1/config
The whole `wingspan.toml` shape (`docs/CONFIG.md`) with secrets redacted
(`"auth_token": "***"`, account number never present — it lives only in the
Schwab token file), plus a `resolved` object: `trades_root`, `open_dir`,
`closed_dir`, `statements_dir`, `state_dir`, `valuation_date`, `demo`,
`version`, `provider_notes`.

## POST /api/v1/reload
Re-scan the trades dir now. → `{ "reloaded": true, "trades": 6 }` — `trades`
is the open-trade count after the rescan (closed trades are not counted).

## GET /api/v1/events  (SSE)
Events: `snapshot` (payload = /market/snapshot body), `trades_changed`
(payload = `{"reason": "fs" | "reload"}`), `provider`
(payload = `{"spot_source": ..., "option_source": ..., "degraded": [...]}`).

## Static
Everything not under `/api` serves the embedded SPA (feature `embed-ui`),
with SPA fallback to `index.html`.
