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
`{ "status": "ok", "version": "0.1.0", "ready": true }`

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
  "as_of": "2026-08-31T14:30:02Z",
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
per line, keyed by that line's `key` — not just `"expiry"`.

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
  "note": "Per-trade attribution intentionally differs from broker/tax lots."
}
```

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
| `error` | `reconciliation_mismatch`, `order_parse_failed`, `statement_parse_failed`, `statement_column_missing` |
| `info` | `statement_section_skipped`, `settlement_inferred`, `vol_fallback`, `forward_fallback` |
| `warn` | everything else: `unmatched_order`, `date_outside_window`, `duplicate_execution_claim`, `duplicate_order_in_trade`, `duplicate_trade_id`, `hint_not_satisfied`, `closed_trade_has_open_legs`, `expired_leg_unknown_settlement`, `unassigned_account_position`, `trade_file_skipped`, `statement_row_skipped`, `execution_fees_unlinked`, `cash_trade_unlinked`, `duplicate_statement`, `non_utf8_file`, `iv_solve_failed`, `iv_clamped`, `missing_quote` |

`iv_solve_failed`, `iv_clamped`, `missing_quote`, `vol_fallback` and
`forward_fallback` are model-side (a valuation, not a file) and carry no
`trade_id`; every one but `forward_fallback` names a `contract`. The rest
come from the book.

### `app_config`
No request body. The whole `wingspan.toml` shape (`docs/CONFIG.md`); no
secret is present in it — the account number lives only in the Schwab token
file and the credentials only in the files `[schwab] env_file` and
`[tastytrade] env_file` name, none of which is part of the config body. The
config gained a `tastytrade` block (additively, between `schwab` and
`yfinance`); it holds variable *names*, a path, three booleans and a version
string. Plus a `resolved` object:
`trades_root`, `open_dir`, `closed_dir`, `statements_dir` (all `null` when
no book is loaded), `state_dir`, `valuation_date`, `demo`, `version`,
`provider_notes`.

## The book

### `reload`
No request body. Re-scan the trades dir now. →
`{ "reloaded": true, "trades": 6 }` — `trades` is the open-trade count after
the rescan (closed trades are not counted).

A reload fails for the same reasons a start-up does: a trades folder
renamed, emptied or unmounted since boot → `invalid-trades-root` with the
discovery sentence as `detail`. Nothing loaded at all is not a failure — it
reloads to an empty book.

### `trades_root`
No request body. Which book is loaded and how that was decided. Trade files
live where the user keeps them and are read **in place** — nothing is ever
copied onto this machine.
```jsonc
{
  "root": "/home/trader/TOS Exports/Trades",  // active root, resolved; null when empty
  "source": "local",                   // "local" | "config" | "demo" | "empty"
  "config_root": "/home/user/Trades",  // what [trades] root alone would use; null if unset
  "open_trades": 6, "closed_trades": 2,
  "chosen_at": "2026-09-03T15:04:00Z", // present only while source is "local"
  "watching": true,                    // always present; live, not install-time
  "recent": ["/home/trader/Archive/2025"]   // always present, may be empty
}
```
`"source": "empty"` with `"root": null` is not an error: no folder has been
chosen and no `[trades] root` is set, so the book is empty until one is.

`watching` is whether an edit in that folder will reach the dashboard by
itself. It is read from the live watch, not from whether one was installed
at boot: an unreadable or unmounted folder, or an exhausted
`fs.inotify.max_user_watches`, reads `false` here and puts the reason in
`app_config`'s `provider_notes`.

`recent` is the folders loaded before, most recent first, de-duplicated and
capped at five; entries that are no longer directories are pruned before
the list is sent.

### `probe_folder`
`{ "request": { "root": "/home/trader/TOS Exports" } }` →
```jsonc
{
  "root": "/home/trader/TOS Exports/Trades",  // the discovered root
  "open_trades": 6, "closed_trades": 2, "statements": 14,
  "note": "the trades layout is one level down, in /home/trader/TOS Exports/Trades"
}
```
What a folder holds if it were loaded. **Persists nothing, reloads nothing,
rewatches nothing, publishes nothing** — it is the "is this the folder you
meant?" half of the two-step choice, so a wrong pick costs a glance rather
than the book already loaded. `note` is absent unless there is something
worth reading first: the one-level descent, or `no statements in that
folder — the reconciliation will be empty`.

### `set_trades_root`
`{ "request": { "root": "/home/trader/TOS Exports/Trades" } }` →

Validated exactly as start-up validates (`~` expanded, canonicalized, the
same one-level discovery descent), persisted to
`<state_dir>/trades_root.json` **before** anything moves, then applied like
a restart: new book, new quote universe, new statement source, a
`trades_changed { "reason": "reload" }` event, the watcher moved onto the
new folder, the cached market snapshot dropped and a refresh requested at
once. → the same shape `trades_root` answers.

The previous root is left exactly as it was on failure. Errors:

- `invalid-trades-root` — the discovery sentence ("no trades folders
  under …"), or `that folder is inside Wingspan's own state directory`, or
  `that folder contains Wingspan's own state directory`, or
  `that folder contains a link outside itself`, or `<path>: not a folder I
  can open (…)`, or a folder a native folder picker returned that fails any
  of the same checks.
- `internal` — a state directory that cannot be written.

### `clear_trades_root`
No request body. **Clear** — Wingspan stops showing the trades; nothing on
disk is touched. → the same shape `trades_root` answers.

**No file, directory or byte inside the user's own folder is read, moved,
renamed or removed.** The whole command touches only `<state_dir>`: the
pointer in `trades_root.json` is forgotten (the folder's own path going to
the front of `recent`, so loading it again is one click) and the cached
market snapshot is dropped, because its quote universe was that book's own
contracts. The book then falls back to `[trades] root`, to the demo book
(if `load_demo` is in effect), or to nothing (`"source": "empty"`). A
`trades_changed` event follows.

With nothing to fall back to, Clear still succeeds — stopping the display
is the point. Idempotent: clearing with nothing loaded changes nothing.

### `pick_folder`
`{ "request": { "start": "/home/trader" } }` (both `request` and `start`
optional). Opens the native folder chooser and returns the chosen path, or
`null` on cancel. Never fails on a cancel — only on the chooser itself
failing to answer. Desktop-only; opened from Rust so an XSS in the webview
cannot summon a file chooser on its own (there is no `dialog:allow-open`
permission in `capabilities/main.json`).

### `load_demo`
No request body. Show the bundled synthetic book. Ignores and never touches
`trades_root.json` — pricing the user's real positions off synthetic
fixtures because of a stray menu click would be a lie the app told them —
so `leave_demo` restores the real folder exactly as it was. → the same
shape `trades_root` answers.

### `leave_demo`
No request body. Put the real book back: the saved folder is honoured
again, the clock is the system clock, and the credentials are the ones the
process booted with. → the same shape `trades_root` answers.

## Settings & diagnostics

*(Desktop-only — none of these have a `wingspan-devserver` route.)*

### `settings_read`
No request body. Returns the file `wingspan.toml` was read from (`null` if
none exists yet — the app is on built-in defaults and Save will create
one), the resolved settings as a fully-populated patch (every field
`Some`, so the form always shows what the app is actually using), the
read-only paths the settings card displays (`trades_root`, `token_path`,
`env_file`, `python`, `tastytrade_env_file`), and two provider status blocks.

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

### `settings_update`
`{ "request": <SettingsPatch> }` — every field optional; `None` leaves that
key exactly as it is, comment and all. Validated *before* the file is
touched (a patch that would not load is refused with the config's own
sentence and `wingspan.toml` is never written), then written with
`toml_edit` so comments, key order and unrelated keys survive, then the
whole application is rebuilt on the file that was just written. →
`{ "file": "/home/trader/.config/wingspan/wingspan.toml" }`.

`SettingsPatch` carries **only non-path keys** — `trades` (minus `root`),
`schwab` (minus `token_path`/`env_file`/`token_config`), `tastytrade` (minus
`env_file`), `yfinance` (minus `python`/`script`), `market` (minus
`state_dir`), `pricing`, `risk_graph` — and is `deny_unknown_fields`, so a
payload naming a path key is rejected by serde before any code runs. The path
fields are written only by the four pickers below, each behind its own
confirmation dialog.

A `[market]` source list **carried by the patch** that is **empty**, or that
names something which is not a provider in this build, is also refused — the
patch is rejected with a sentence naming the key and listing the legal names
and `wingspan.toml` is not written. The legal names are `schwab`,
`tastytrade`, `yfinance`, `cache` and `fixture` for `spot_sources`, and those
plus `statement` for `option_sources` (statement marks are registered
option-only). A list the patch does not carry is not examined, so a file
hand-edited to the same mistake still loads, still boots and still saves; the
`diagnostics` checklist warns about the unknown name there instead.

### `pick_python` / `pick_env_file` / `pick_token_file` / `pick_tastytrade_env_file`
No request body. Each opens a native file chooser, shows a confirmation
dialog naming the chosen path and stating what Wingspan will do with it,
and — only on confirmation — writes that path string into `wingspan.toml`
and rebuilds the app on it. → `{ "path": "/usr/bin/python3", "applied":
true, "detail": null }`, or `null` on cancel at either step. `applied` is
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
already knows (the active trades root, the log directory, the directory
holding `wingspan.toml`) — anything else is refused with `unprocessable`,
so an XSS cannot turn this into a general "open anything" primitive.

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
| 422 | `invalid-trades-root` | the discovery sentence, or one of the folder refusals, on `set_trades_root`, `probe_folder` and `reload` — now also covers a bad **picked** folder |
| 422 | `out-of-range` | `risk_graph` with `points` > 2001 or more than 32 `dates`; `risk_graph`/`what_if` with a date more than 20000 days from today (a graph nothing could draw, and an offset chrono would overflow on) |
| 422 | `unprocessable` | a settings patch that fails validation, a picked file outside what a picker allows, a `reveal_folder` path outside its allow-list |
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
"pick_folder not allowed. Command not found"
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
| `trades_changed` | `{ "reason": "fs" \| "reload" }` | the file watcher fires, or `reload` runs |
| `heartbeat` | `{}` | every 15 s while the application is running; 45 s of silence puts the status dot out |
| `app_ready` | none | boot, or a rebuild (a settings write, entering/leaving the demo), finished successfully |
| `app_error` | `{ "detail": "..." }` | boot, or a rebuild, failed |

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
| `trades_root` | `GET /api/v1/trades/root` |
| `probe_folder` | `POST /api/v1/trades/root/probe` |
| `set_trades_root` | `PUT /api/v1/trades/root` |
| `clear_trades_root` | `DELETE /api/v1/trades/root` |
| `pick_folder`, `load_demo`, `leave_demo`, `settings_read`, `settings_update`, `pick_python`, `pick_env_file`, `pick_token_file`, `pick_tastytrade_env_file`, `probe_python`, `diagnostics`, `reveal_folder`, `open_log_folder`, `open_config_file`, `check_for_updates` | *(none — desktop-only)* |

Events map the same way: `GET /api/v1/events` (SSE) carries `snapshot`,
`trades_changed` and `provider` with identical payloads; `heartbeat`,
`app_ready` and `app_error` exist only on the desktop transport.
