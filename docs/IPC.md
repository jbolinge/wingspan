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
`quote_count` counts every quote in the snapshot: the book's contracts and
any the Model tab registered with `set_model_contracts`.

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
  "weighted_vega": -812.4,             // against portfolio_all.weighted_vega_reference; rows sum to portfolio_all
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
`{ "count": 6, "pnl_open": ..., "pnl_total": ..., "greeks": Greeks, "weighted_vega": ..., "weighted_vega_reference": "30d" }`

`weighted_vega_reference` is `"<n>d"` (the `"30d"` default) or
`"front_expiry (YYYY-MM-DD)"`. It is resolved for the set the stats cover; in
`trades`, every row's `weighted_vega` is measured against
`portfolio_all.weighted_vega_reference`, so the rows add up to
`portfolio_all.weighted_vega`.

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
`{ "request": { "ids": ["m1-2026-07-14", "m4-2026-08-04"], "model_legs": [] } }` →
`PortfolioStats` for the selected subset (empty `ids` = empty stats — unlike
`risk_graph`/`what_if`, where empty with no model means the whole book). The
weighted-vega reference is resolved for the requested set — in
`front_expiry` mode, the set's own front expiry.

`model_legs` is optional (default `[]`) — see [Model legs](#model-legs). With
model legs the stats are the selected trades **plus** the model: its greeks
and weighted vega are added (and the weighted-vega reference is resolved over
the trades and the model legs together, exactly as `risk_graph`'s `t0_greeks`
resolves it, so the header and the strip agree), but `pnl_open`/`pnl_total`
are unchanged — a modeled leg is entered at today's mark, so its P&L today is
zero — and `count` is still the number of trades: the model is not one.
`ids: []` with model legs is therefore the model alone (`count: 0`, zero P&L,
the model's own greeks) — the same rule all three model-aware commands
follow (see [Model legs](#model-legs)).

### `risk_graph`
Two top-level arguments: `request` and `seq` (see above). Unlike
`portfolio`, empty `trade_ids` with no `model_legs` means the **whole open
book**, not an empty graph. Empty `trade_ids` **with** a model is the model
alone (see [Model legs](#model-legs) — legs that all net to zero are no
model, so empty `trade_ids` then means the whole book again).
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
  "include_realized": true,
  "model_legs": []                      // optional; see "Model legs" below
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
    "weighted_vega_reference": "30d"   // the label portfolio publishes for this set
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
(or `pnl_open` when `include_realized=false`) — **with or without
`model_legs`**, because a modeled leg is entered at today's mark and so
contributes exactly nothing at spot today. When `model_legs` is non-empty,
`per_trade` carries one more curve per date with `"trade_id": "model"`, and
`warnings` carries a `missing_quote` for every modeled contract the snapshot
does not quote with a mark — no quote at all, or one with no bid, ask or mark,
the same rule `set_model_contracts` reports `missing` by (it is priced off a
neighbouring strike's vol until `set_model_contracts` has had it quoted). `breakevens` has one entry
per line, keyed by that line's `key` — not just `"expiry"`. `t0_greeks` is
the T+0 line's greeks per price, in the same units and under the same
per-leg vol shift as the curve above it; at spot they equal the dashboard's
position greeks and weighted vega.

### `what_if`
`{ "request": { "trade_ids": [...], "price": 6400, "date": "2026-09-15", "vol_shift": 0.01, "include_realized": true } }`
→ `{ "pnl": ..., "greeks": Greeks }` — recomputed at that point, same
engine. `vol_shift` and `include_realized` are both optional (default `0.0`
and `true`), and so is `model_legs` (default `[]`; see
[Model legs](#model-legs)). As with `risk_graph`, empty `trade_ids` means the
whole open book (not empty, unlike `portfolio`) when there is no model, and
the model alone when there is one.

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
come from the book. A modeled leg's warnings (`missing_quote`,
`vol_fallback`) appear only on the `risk_graph` response that drew it —
never in `warnings` or on a trade row, because the model is not part of the
book — and an `option_chain`'s own `iv_solve_failed`/`iv_clamped` only on
that chain's response.

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

## The Model tab

The Model tab shows the option chain of the underlying, one expiration per
section, and lets the trader enter a quantity next to any contract. Those
quantities — **model legs** — are scratch state the SPA owns (persisted in
its own `wingspan.ui` storage), never part of the book: nothing here writes a
trade, a leg or a file. They reach Rust only as the `model_legs` field of
`risk_graph`, `what_if` and `portfolio`, so the Positions tab can draw the
selected trades *as they would be* after the adjustment — or, with nothing
selected, the model on its own.

Four commands serve it: two reads of the chain, one that tells the
market service which contracts the model holds so they are quoted like the
book's own, and `model_orders`, which writes the model legs out as
thinkorswim order lines for the ticket's Orders block. All four are
additive; nothing about the commands above changes when no model legs are
sent.

### `chain_expirations`
No request body. Every unexpired expiration the option ladder lists. →
```jsonc
{
  "as_of": "2026-08-31T16:00:00Z",      // when this list was fetched
  "source": "schwab",                   // provider that answered; "fixture" for the demo
  "underlying": "SPX",
  "stale": false,                       // true when served from the in-memory cache after a failed refetch
  "degraded": ["cache: cannot list option chains"],   // every provider skipped/failed, one sentence each
  "expirations": [
    { "expiry": "2026-09-01", "dte": 1, "root": "SPXW", "settlement": "pm", "kind": "weekly" },
    { "expiry": "2026-09-18", "dte": 18, "root": "SPX",  "settlement": "am", "kind": "monthly" },
    { "expiry": "2026-09-18", "dte": 18, "root": "SPXW", "settlement": "pm", "kind": "weekly" }
  ]
}
```
An expiration is identified by the pair **`(expiry, root)`**: the same date
can appear twice — a third Friday lists both `SPX`/AM and `SPXW`/PM. Sorted
by `expiry`, then root (`SPX` before `SPXW`). `dte` is calendar days from the
market date (the valuation clock's date in New York; `0` on expiry day), and
only unexpired expirations are listed, so it is never negative. `kind` is an
**open set** — `"daily" | "weekly" | "monthly" | "quarterly" | "eom" |
"unknown"` today; a client renders a value it does not know rather than
dropping the expiration. `settlement` is `"am"` or `"pm"`, as on a
`Contract`.

The ladder is the configured `option_sources`, walked in order. A provider
that cannot list a chain at all — the cached snapshot and statement marks
never can — is skipped with one sentence in `degraded` ("cache: cannot list
option chains"); one that fails ("schwab did not answer within 10s") is
recorded the same way and the next is tried. The first provider that
answers wins and its name is `source`. The answer is kept in memory for 15
minutes (`as_of` stays the fetch time); a refetch that fails after that
serves the previous list with `"stale": true` and the failure appended to
`degraded`, dropping any expiration that has passed since. Nothing about the
chain is ever written to disk.

Needs no market snapshot. Errors: 503 `chain-unavailable` when no provider
could list a chain and there is no earlier answer to fall back on — `detail`
is the `degraded` sentences joined with "; " (e.g. "cache: cannot list option
chains; statement: cannot list option chains").

### `option_chain`
Every strike of one listed expiration, with **our** IVs and greeks.
```jsonc
// request
{ "request": { "expiry": "2026-10-16", "root": "SPX",
               "scope": "near" } }       // optional, "near" (default) | "all" — additive since 0.4.7
// response
{
  "as_of": "...", "source": "fixture", "stale": false, "degraded": [],
  "underlying": "SPX", "spot": 7750.0,
  "expiry": "2026-10-16", "root": "SPX", "settlement": "am", "dte": 46,
  "forward": 7792.1,                    // the forward the IVs were solved against
  "rows": [
    { "strike": 7700.0,
      "call": ChainQuote | null,
      "put":  ChainQuote | null }
  ],
  "warnings": [ Warning, ... ]          // model-side only: iv_solve_failed / iv_clamped, keyed to a contract
}
```
#### ChainQuote
```jsonc
{ "contract": Contract,                 // exactly Leg.contract's shape (underlying, root, expiry, settlement, strike, right, multiplier, osi, tos_code)
  "bid": 42.1, "ask": 42.9, "mark": 42.5, "last": 42.3,   // nullable; mark null => unpriceable, show "—" and do not let it be modeled
  "iv": 0.1712,                         // OUR solved IV; null when the solve failed
  "iv_hint": 0.1705,                    // the provider's own IV — diagnostics only, nullable, never used for pricing
  "greeks": Greeks | null,              // OUR per-contract greeks, same units as Leg.greeks; null exactly when iv is null
  "volume": 1203, "open_interest": 8812, // nullable; the fixture provider answers null
  "quote_as_of": "..." }
```
`rows` is sorted by strike ascending, one row per strike, and every strike
the provider listed for that `(expiry, root)` is present even when only one
side was quoted (the other side is `null`). `scope` says how much of the
expiration is wanted. With `"near"` (the default, and the only behaviour
before 0.4.7) a provider that pays per symbol (tastytrade) quotes only
strikes between 70 % and 120 % of spot, the window stretched to take in
every strike the book or the model holds in that expiry; one that returns a
whole expiration in one call returns all of it either way. With `"all"`
every strike the provider lists is quoted — what the Model tab sends for the
chain on screen while its **All** strikes button is pressed, so a far tail
put shows its row; the `16 · 32 · 64` bands, and the chains fetched only for
the ticket's marks, send `"near"`.

**The numbers are ours, not the provider's.** The chain's quotes are solved
against the **book's** priced market — the current snapshot's `spot` (the
chain never has a spot of its own), the book's calibrated forward curve
(`forward` is its value for this expiration), the book's pricing
parameters — with the same IV solver and the same per-contract greeks the
positions table uses. So a contract the book holds reads the same mark, IV
and greeks here as on its `Leg` row, and the chain, the table and the risk
graph agree by construction. A quote whose IV cannot be solved (crossed, no
mark) keeps its `bid`/`ask`/`mark` with `iv` and `greeks` `null` and an
`iv_solve_failed` warning; a mark below intrinsic is clamped to the minimum
vol (`iv_clamped`) and keeps its greeks.

`as_of`/`source`/`stale`/`degraded` describe the chain fetch, with the same
ladder, skip and stale rules as `chain_expirations`. One expiration's chain
is kept in memory for one market refresh interval (so a re-render, or the
refetch a `snapshot` event triggers, inside one cadence costs nothing), at
most eight expirations at once, the oldest fetched dropped first. A cached
`"all"` chain answers a `"near"` request for the rest of its cadence (it
holds everything the window would); a cached `"near"` chain never answers
an `"all"` one, which fetches. The stale fallback after a failed fetch is
whatever chain is cached for the expiration, whichever scope it was.

Errors, checked in this order: 503 `no-market-data` when there is no
snapshot at all (nothing to solve against — checked before any provider is
asked); 503 `chain-unavailable` when no provider could list or quote the
chain (`detail` = the sentences joined with "; "); 404 `not-found` when the
`(expiry, root)` is not a listed expiration; and, on the dev server only,
`invalid-request-body` for a body that does not parse (a missing `root`, a
date that is not `YYYY-MM-DD`).

### `set_model_contracts`
Register the contracts the model holds legs in, so the market service quotes
them along with the book.
```jsonc
// request — the COMPLETE set; an empty list clears it
{ "request": { "contracts": [ ModelContract, ... ] } }
// response
{ "contracts": 3, "quoted": 3, "missing": ["SPXW261016C7700"], "refreshed": true,
  "detail": null }    // one sentence when the refresh failed or timed out (the contracts are still registered)
```
#### ModelContract
A `Contract` minus its derived fields:
```jsonc
{ "underlying": "SPX", "root": "SPXW", "expiry": "2026-10-16", "settlement": "pm",
  "strike": 7700.0, "right": "call", "multiplier": 100 }
```
`underlying` defaults to the configured underlying and `multiplier` to 100
when absent. Any other key — `osi`, `tos_code` — is **ignored, not refused**,
so the SPA can send a chain row's whole `contract` object as is.
`settlement` and `right` are lowercase, as `Contract` writes them.

The set replaces the market service's **extra quote universe**: every
refresh quotes the book's contracts ∪ these. The command then runs one
refresh **and waits for it** (bounded by the refresh loop's own deadline —
four refresh intervals, between 30 s and 300 s), so the very next
`risk_graph` prices the modeled legs off their own quotes instead of a
neighbouring strike's vol. That refresh publishes a snapshot like any other,
so a `snapshot` event follows. `contracts` is the number of distinct
contracts registered; `quoted` and `missing` (TOS codes) are read off the
snapshot current afterwards — "quoted" means a quote with a mark. A refresh
that fails or overruns is **not** an error: `refreshed` is `false`, `detail`
says why, the contracts stay registered, and the next scheduled refresh
quotes them.

The extras live on the running market service only. They do **not** survive
a runtime swap — a source change, a settings save, entering or leaving the
demo book — so the SPA registers its set again on `app_ready` and
`trades_changed`. Not part of the contract dump: it is stateful.

Validation happens before anything is registered, so a refused request
leaves the previous set exactly as it was: 422 `out-of-range` for more than
64 contracts, or any contract out of range; 400 `bad-request` for any
contract that is malformed — both as for a model leg, below.

### `model_orders`
The model legs as thinkorswim (TOS) order strings, grouped into the spreads
TOS names — what the ticket's **Orders** block lists and its Copy buttons put
on the clipboard.
```jsonc
// request — Model legs exactly as `portfolio` carries them (default [])
{ "request": { "model_legs": [ { "contract": ModelContract, "qty": -1 }, ... ] } }
// response
{
  "orders": [
    {
      "strategy": "iron_condor",   // single | vertical | calendar | diagonal | butterfly | straddle | strangle | condor | iron_condor
      "side": "sell",              // "buy" | "sell"
      "qty": 1,                    // order quantity, > 0; the line writes it +n / -n
      "line": "SELL -1 IRON CONDOR SPX 100 16 OCT 26 [AM] 7600/7700/6900/6800 CALL/PUT @70.85 LMT",
      "net_price": 70.85,          // the number after `@`; null (and no `@… LMT`) when a leg has no finite value
      "legs": [ { "contract": Contract, "qty": -1 }, ... ],  // the WHOLE order's signed legs, in the line's order
      "unquoted": []               // TOS codes priced off a neighbour's vol, not a quote; usually empty
    },
    { "strategy": "single", "side": "buy", "qty": 2,
      "line": "BUY +2 SPX 100 20 NOV 26 [AM] 7000 PUT @58.80 LMT", ... }
  ],
  "text": "SELL -1 IRON CONDOR …\nBUY +2 SPX 100 …\n"   // every line, each newline-terminated; "" with no orders
}
```
The legs are validated and netted exactly as **Model legs** below (the same
refusals, the same slugs). The netted bag is then split by
`wingspan_core::tos_format::group`: legs on different roots never share an
order (`(Weeklys)` is per order); within a root the named spreads are peeled
off greedily, highest priority first — `IRON CONDOR` (an iron butterfly is
one too), `BUTTERFLY` (broken wings included), `CONDOR`, `VERTICAL`,
`CALENDAR`, `DIAGONAL`, `STRADDLE`, `STRANGLE` — each taking as many whole
units as every leg allows, and whatever is left is written as singles. The
split is also tried with `BUTTERFLY` and `CONDOR` ahead of `IRON CONDOR`
(whose verticals can otherwise be cut out of the middle of a fly — a put
broken-wing fly beside a call spread is also an iron condor beside a put
spread), and whichever gives fewer orders wins, the flies on a tie. So an
unbalanced position becomes smaller named orders (`+1/−3/+2` is a
`BUTTERFLY` and a `VERTICAL`), never a `~BUTTERFLY` ratio or `BACKRATIO`
line. The same legs in any order give the same orders, sorted by that
priority, then by expiry.

Each line is written the way TOS prints it — the nearer-the-money strike
first (calls ascending, puts descending), the far expiry first on a
calendar or diagonal, `[AM]` on every AM expiry, `(Weeklys)` for `SPXW`, no
`GTC`/exchange tail — so `BUY` is the debit form of a spread and `SELL` the
credit form. `net_price` is the per-unit net **mark**: each contract valued
with the same `value_now` a model leg is entered at, summed with the order's
own per-unit quantities, and written side-relative (positive for a debit
bought or a credit sold; a diagonal bought for a credit reads `@-1.20`).
Multi-leg orders round to the cent, a single to the SPX tick ($0.05 below
$3.00, $0.10 at or above). It is a starting limit, not advice: the trader
sets their own in TOS.

No model (an empty list, or legs that net to zero) is `{ "orders": [],
"text": "" }` and needs no market. Any leg needs a snapshot: 503
`no-market-data` when there is none, as `portfolio` answers. Every line
round-trips through the test-only order parser
(`docs/DATA-FORMATS.md` §"Writing orders").

### Model legs
`risk_graph`, `what_if`, `portfolio` and `model_orders` each accept an optional
```jsonc
"model_legs": [ { "contract": ModelContract, "qty": -2 }, ... ]     // default []
```
`qty` is a signed whole number of contracts (+ long, − short). Every leg is
validated, then legs on the same contract are **summed** and a zero total is
**dropped**; what remains becomes the legs of one synthetic position with
the id `"model"` (a trade id always ends in its open date, so it cannot
collide with one), priced by the same engine as every trade:

- **Each leg is entered at the current mark.** Its average cost is the
  model's value of the contract right now — exactly the quote's own mark
  when the snapshot quotes it (the IV solve reproduces it), else the model
  value at a neighbouring strike's vol, in which case the `risk_graph`
  response carries a `missing_quote` warning for that contract (call
  `set_model_contracts` to have it quoted). The position's net cash flow is
  `−Σ qty · mark · multiplier` and its fees are zero.
- **So the model is worth exactly nothing today at spot**, and the
  invariant "the `t0` line at spot equals `pnl_total`" holds with or without
  it. What the model changes is the *shape*: away from spot, at a later
  date, under a vol shift.
- **Explicit ids are those trades plus the model; empty ids are the model
  alone** — in all three commands. An empty list is the only way a request
  can say "no trades", so once there is a model it cannot also mean "the
  whole book": a trader who has selected nothing on the Positions tab sees
  what the modeled legs do on their own. With no model legs, empty keeps
  each command's old meaning (the whole open book for `risk_graph` and
  `what_if`, the empty set for `portfolio`), so nothing changes for a caller
  that sends none. "A model" means legs that survive the netting above:
  legs that all net to zero are no model, and empty ids then keep the old
  meaning too. (`service::positions_with_model` is the one rule.)
- `risk_graph`: `per_trade` gains a curve per date with `trade_id:
  "model"` — with empty `trade_ids`, the only curve, and every line equals
  it. With the model alone the `t0` line at spot is `0`, which is the
  invariant above with no trades in the sum.
- `what_if`: as `risk_graph` — the selected trades plus the model, or the
  model alone.
- `portfolio`: the model adds greeks and weighted vega, never P&L or
  `count` (see `portfolio`).

Refused before anything is priced:

| status | slug | when |
|---|---|---|
| 422 | `out-of-range` | more than 64 legs; a `qty` of 0, or past ±999; a strike ≤ 0 or > 100000; an expiry before the valuation date, or more than 20000 days after it |
| 400 | `bad-request` | an `underlying` other than the configured one; a root that is not 1–6 capital letters or digits; a `settlement` the root's own rule contradicts (`SPX` is AM, `SPXW` PM); a strike with more than 3 decimals (no option symbol can carry it); a `multiplier` of 0 |

A fractional `qty`, an unknown `right` or `settlement`, or a missing required
field never reaches the engine at all: Tauri rejects the call with its own
bare string, and the dev server with `invalid-request-body` (see Errors).

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
never moved, renamed or changed. (The rows either one yields are cached
inside the `0700` state directory, as `docs/CONFIG.md` describes, unless
`[trades] cache = false`.)
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
  "warnings": [ /* Warning */ ] }   // no "note" key at all unless there is one to read
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
read at all. `note` is **absent** unless there is something worth reading
first — the key is omitted, not `null` (the one field of this DTO that is
not always present). A published `/d/e/<id>/pub` link is the case that
produces one today: such a link cannot select a tab by name, so the note
says which tab was actually read.

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
| 422 | `invalid-trade-source` | a URL or path Wingspan will not use (bad shape, a host that is not `docs.google.com`, a CSV path that did not come from the picker), a spreadsheet id `docs.google.com` answers `404` for, a CSV it cannot parse, or a required column the header does not carry |
| 500 | `internal` | a state directory that cannot be written |

`{ "kind": "manual" }` is reserved for a future manual leg-entry seam and
also answers `invalid-trade-source` today.

**No user-typed string is ever fetched verbatim:** Rust extracts the
spreadsheet id and builds the request URL itself, and only
`docs.google.com` is reachable — redirects are followed only back to the
host the request started on, or to `accounts.google.com` (the signal a
sheet is not shared). **The webview never talks to Google** —
`tauri.conf.json`'s CSP `connect-src` forbids it.

**And no JS-supplied path is ever read.** In the desktop app both commands
refuse a `"kind": "csv"` path that Rust does not already know, with 422
`invalid-trade-source`. A path is known when it came from this process's
own `pick_csv_file` (the user chose it in the OS chooser and confirmed it),
or it is `[trades] csv_path`, or it is the active source or one of
`trade_source.json`'s `recent` entries. So an XSS in the webview cannot use
the probe — whose `columns_ignored` echoes header text back — to read a
file the user never named. The dev server is a localhost development tool
with no picker and no such gate; its CSV path comes from `--legs`, a typed
path or a recent entry.

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

Before returning, it shows a second, Rust-side confirmation naming the file
and what will happen to it — that it is read where it sits and never moved,
renamed or changed, and that a copy of the rows it reads is kept in the app's
own state directory (`[trades] cache = false` opts out). Confirming is also
what **admits** that path to `probe_trade_source`/`set_trade_source` (above):
a `csv` path Rust does not already know is refused.

### `open_sheet`
No request body. Desktop-only; opens the active sheet's real URL in the
system browser. The URL is rebuilt Rust-side from the stored source — it
is never handed to JS. Answers whenever the active source is a sheet —
including the `"kind": "config"` rung, since a deployer's `[trades]
sheet_url` is one; a file source or the demo book is refused with
`unprocessable`.

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
  "rows": [ { "grade": "ok", "section": "Trades", "name": "source", "detail": "..." }, ... ]
}
```
`grade` is one of `"ok" | "warn" | "fail" | "note"`.

## Shell

### `reveal_folder`
`{ "request": { "path": "..." } }` — opens a folder in the desktop's file
manager. Answers only for one of three paths the running application
already knows: the folder holding the active CSV file (whether it was
imported in the app or set as `[trades] csv_path`, but never the demo
book's bundled folder), the log directory, and the directory holding
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

### `copy_text`
`{ "request": { "text": "SELL -1 IRON CONDOR …\n" } }` → `null`. Puts one
string on the system clipboard — the Model ticket's Copy and Copy all. The
webview's `navigator.clipboard` cannot be relied on under WebKitGTK, so the
desktop app writes it from Rust (the `arboard` crate, on a worker thread;
X11, or XWayland under a Wayland session — no clipboard plugin, no plugin
permission). It takes no path and executes nothing. Refused with 400
`bad-request` past 64 KiB; a clipboard the OS will not open is a 500
`internal` whose `detail` says why. Awaits boot like every other command.
When it fails (a Wayland session with no XWayland), `web/src/lib/clipboard.ts`
gives the webview's own `navigator.clipboard` one try before reporting the
command's problem. The browser build has no route for it: `clipboard.ts`
uses `navigator.clipboard.writeText` there alone.

## Errors

A **failed** command rejects with the serialised `problem+json` struct — an
**object** — `{ "type": "https://wingspan.invalid/problems/<slug>", "title": ..., "status": <n>, "detail": "..." }`.
`type` is a deliberately unresolvable URN; nothing has to fetch it to
understand the error. The slugs, beyond the per-command notes above:

| status | slug | when |
|---|---|---|
| 400 | `bad-request` | a value the engine cannot use (e.g. risk-graph `points < 2`, a non-positive what-if `price`); a modeled contract on another underlying, with a malformed root, a settlement its root's rule contradicts, a strike finer than 0.001, or a zero multiplier |
| 404 | `not-found` | no such trade id; an `option_chain` `(expiry, root)` the provider does not list |
| 422 | `sheet-not-shared` | the fetch was redirected to `accounts.google.com`, or answered 401/403 — the sheet is not shared with anyone-with-the-link |
| 422 | `sheet-tab-not-found` | the fetch answered 200 with gviz's own error payload — no tab of that name in that spreadsheet |
| 422 | `sheet-unreachable` | a network error, a timeout, a 5xx, or a response past the size limit; the previous book stays on screen |
| 422 | `invalid-trade-source` | a URL or path Wingspan will not use (bad shape, a host that is not `docs.google.com`, a CSV path that did not come from the picker), a spreadsheet id `docs.google.com` answers `404` for, a CSV it cannot parse, or a required column the header does not carry |
| 422 | `out-of-range` | `risk_graph` with `points` > 2001 or more than 32 `dates`; `risk_graph`/`what_if` with a date more than 20000 days from today (a graph nothing could draw, and an offset chrono would overflow on); more than 64 `model_legs` or model contracts, a modeled `qty` of 0 or past ±999, a modeled strike ≤ 0 or > 100000, a modeled expiry before the valuation date or more than 20000 days out |
| 422 | `unprocessable` | a file the native chooser returned that this computer has no path for, or more than one file; a `reveal_folder` path outside its allow-list |
| 422 | `settings` | a settings patch that fails validation; a picked file outside what a picker allows, with no path on this computer, or more than one file |
| 500 | `internal` | a command's own task panicked, or a filesystem operation Wingspan cannot recover from |
| 503 | `no-market-data` | no snapshot to value against — see `trades_closed` for the exception |
| 503 | `chain-unavailable` | no option source could list or quote the chain (statement marks and the cache cannot; a live provider failed). `detail` is every provider's sentence, joined with "; " |

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
| `portfolio` | `GET /api/v1/portfolio?ids=...` **or** `POST /api/v1/portfolio` (the body is the command's `request`; the only way to send `model_legs`) |
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
| `chain_expirations` | `GET /api/v1/chain/expirations` |
| `option_chain` | `POST /api/v1/chain` |
| `set_model_contracts` | `PUT /api/v1/model/contracts` |
| `model_orders` | `POST /api/v1/model/orders` |
| `pick_csv_file`, `open_sheet`, `load_demo`, `leave_demo`, `settings_read`, `settings_update`, `pick_python`, `pick_env_file`, `pick_token_file`, `pick_tastytrade_env_file`, `probe_python`, `diagnostics`, `reveal_folder`, `open_log_folder`, `open_config_file`, `check_for_updates`, `copy_text` | *(none — desktop-only; the browser copies with `navigator.clipboard`)* |

`wingspan-devserver serve --demo [--legs <csv>]` is the new invocation; the
flag that used to point at a directory of TOS exports is gone.

Events map the same way: `GET /api/v1/events` (SSE) carries `snapshot`,
`trades_changed` and `provider` with identical payloads; `heartbeat`,
`app_ready` and `app_error` exist only on the desktop transport.
