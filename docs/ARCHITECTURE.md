# Architecture

```
Legs sheet / CSV ─parse→ LegRows ─replay→ Positions ─price(snapshot)→ Stats & Curves ─invoke/events→ SPA
```

(The dev-only `wingspan-devserver` serves the same DTOs over JSON and SSE
instead, for the Vite loop and Playwright.)

The source is a Google Sheet shared read-only, or a CSV file on this
machine. The sheet is fetched over HTTPS by **Rust**, never by the webview,
and only `docs.google.com` is reachable. A deployer may still set `[trades]
sheet_url` / `[trades] csv_path`, and `--demo` has its bundled book, but
neither is changeable at runtime and neither is required — with no source
at all the pipeline above simply runs on nothing.

This is a departure from "copy nothing": the bytes of the last successful
read — a sheet fetched over HTTPS **or a CSV file read off this disk** —
are cached at `<state_dir>/trade_source_cache.csv` (plus a `.json` sidecar
carrying `{ source, fetched_at, bytes, sha256 }`), both mode `0600` beside
the `0600` `trade_source.json` pointer, because a book that vanishes when
the network does — or when a removable disk is unplugged — is worse than
one kept on disk. So importing a local CSV does put a copy of its rows
inside the state directory; the file itself is never moved, renamed or
changed. `[trades] cache = false` opts out, and Clear deletes it.

Everything below the service layer is pure, synchronous, unit-testable Rust;
I/O (files, HTTP, the Python bridge) sits at the edges behind traits. Every
response body is produced by `service.rs`, which takes an `&AppState` and
plain DTOs and knows nothing about any transport; `web/e2e/fixtures/contract/`
is a checked-in dump of all of them for the demo book, and CI fails if the
code stops producing it.

## Crates

| crate | contents | depends on |
|---|---|---|
| `wingspan-core` | domain model, a source-agnostic `LegRow` intermediate and the Legs CSV parser, trade assembly, P&L, reconciliation, warnings; shared market value types (`market.rs`); the TOS order *formatter* (`tos_format.rs`, the Model ticket's order lines) (TOS order and statement parsers, now reached only from the core tests and `wingspan-cli export-legs`) | — |
| `wingspan-pricing` | BSM, IV solver, forward calibration (put-call parity), bump-and-reprice greeks, weighted vega, risk-graph engine, what-if | core |
| `wingspan-marketdata` | `SpotSource`/`OptionQuoteSource` traits; Schwab, tastytrade, yfinance bridge, cached snapshot, statement-IV, fixture providers; fallback chain + refresh loop | core, pricing |
| `wingspan-app` | transport-free library: figment config, state, the service layer (`service.rs`), events, trade sources (sheet fetch, CSV, demo) + the on-disk source cache, diagnostics, demo book | all |
| `wingspan-cli` | `wingspan-cli` binary: `validate`, `risk`, `doctor`, `dump-contract`, `export-legs` | wingspan-app |
| `wingspan-devserver` | **dev-only** `wingspan-devserver` binary: axum API + SSE over `wingspan-app`'s service layer, embedded SPA (`embed-ui`) — for Playwright and the Vite dev loop, never shipped, always bound to `127.0.0.1` | wingspan-app |
| `web/src-tauri` (crate `wingspan-desktop`) | the shipped product: Tauri desktop app, shipped executable `wingspan` | wingspan-app |

The shared market value types live in **core**, not marketdata, to keep the
dependency graph acyclic (pricing consumes snapshots; marketdata's
statement-IV and cached-snapshot rungs consume the pricing solver — forward
calibration, the IV surface and BSM — for their sticky-strike reprice).

`web/` is a React 19 SPA speaking the frozen contract in `docs/IPC.md`; it is
bundled by Tauri from `web/dist` into the desktop app's webview (and, for the
dev-only harness, into `wingspan-devserver` via `rust-embed`).

## The IPC boundary

The SPA never speaks HTTP inside the shipped app. Every request it makes is a
Tauri `invoke` command in `web/src-tauri/src/commands.rs`,
`settings_commands.rs` or `picker.rs` — one `#[tauri::command]` per entry in
`docs/IPC.md`, each a thin wrapper over a `wingspan_app::service::*` function.
Three rules hold for all of them: every payload travels as one `request`
argument (Tauri snake_cases camelCase JS keys, but `types.ts` is already
snake_case); every command that answers from the service layer is `async`
and does its work behind a `JoinHandle`, so a panic becomes a 500-shaped
error the SPA can render instead of a spinner that never resolves (a
*synchronous* command that panics runs inside WebKit's own callback and
takes the whole process down) — the five native pickers (`pick_csv_file`,
`pick_python`, `pick_env_file`, `pick_token_file`,
`pick_tastytrade_env_file`) are `async` for a second reason on top of that:
a plain `#[tauri::command]` on a sync fn is dispatched *inline on the thread
that delivered the IPC request*, which on Linux is the GTK main thread, so a
picker that waits there for a chooser is waiting for the very thread that
has to build it, and the whole application wedges (see `picker.rs`'s header
— this is the 0.2.0 folder-picker hang); and no command
that changes what the app executes, or where it reads a secret from, takes
a path from JS — the five path-shaped settings are written only by those
pickers, each behind its own confirmation dialog: `[yfinance] python`,
`[schwab] token_path`, `[schwab] env_file`, `[tastytrade] env_file`, and
`[trades] csv_path`.
`docs/IPC.md` is the frozen shape; this is the mechanism that serves it.

## The pending-boot `AppSlot`

`AppState::build` can take up to ~40 seconds on a cold Schwab launch — the
window is shown before it finishes, because a window that shows nothing for
40 seconds looks like one that failed to start. Every command reads the
application through `AppSlot`, a `tokio::sync::watch` cell: before boot
publishes, a command **pends** rather than answering an error, so the window
shows a spinner rather than an error it would have to retract. `health` is
the one exception — it reads the slot without awaiting it, so a caller can
ask whether the app is up yet without joining the queue. The SPA does not
call it: it learns readiness from the `app_ready`/`app_error` events, with
the pending `trades` query covering the wait. `wingspan-devserver`'s
`/api/v1/health` is what `web/playwright.config.ts` polls.

## A source change is a restart

Importing a different sheet or CSV, clearing the source, entering or
leaving the demo book, and writing a settings patch that touches the
provider chain all go through the same path: the whole `Runtime` —
`AppState`, its background refresh loop, its
event pump — is swapped for a freshly built one, never mutated in place.
The slot is emptied first, so a command that arrives mid-swap pends on the
next value instead of reading a half-changed application; the old runtime
is dropped only after the new one is published, which is what aborts its
refresh loop and event pump cleanly. This is what makes "Load demo book"
and a Settings save safe operations rather than half-applied ones: there is
no state to leave inconsistent, because there is only ever one `Runtime` in
force.

## The Model tab: chain reads and model legs

The option chain is a provider capability, not a snapshot field. Two
`OptionQuoteSource` methods — `expirations` and `chain`, both
default-unsupported — are walked down the configured option ladder by
`FallbackChain::resolve_expirations`/`resolve_chain`; `MarketService` keeps
the answers in memory (the listing for 15 minutes, up to eight expirations'
chains for one refresh interval each, a failed refetch serving the previous
answer marked `stale`) and **never** writes them to disk or into the
published snapshot. `wingspan-app` solves a chain against the book's own
priced market (`wingspan_pricing::chain_analytics`), so the chain, the
positions table and the risk graph share one spot, one forward curve and
one IV solver.

Model legs are **UI scratch state**. The SPA owns them (persisted in its own
`wingspan.ui` storage); Rust never stores one. They arrive as the
`model_legs` field of each `risk_graph`/`what_if`/`portfolio` request and
live for that one call: `service::model_position` validates and nets them
into a synthetic `Position` with the id `model`, entered at the current mark
(so the T+0-at-spot invariant below holds with any model), and the engine
prices it beside the selected trades — or alone, when the id list is empty:
`service::positions_with_model` gives all three commands that one rule,
because an empty list is the only way a request can say "no trades".
Nothing about the book, the warnings list or the trade rows ever sees it.

On the SPA side the tab is two views behind one switch (`model.view`,
persisted): **Chain**, one expiration's chain at a time (`model.expiry`, a
section key, `null` for none open, or `'auto'` for the first listed
expiration the model has a leg in, else the first the position holds, else
the nearest), and **Risk graph**, which mounts the very `RiskGraphPanel`
Positions does — same props, same TanStack Query keys, so the switch costs
no request. The model ticket stays beside either view; its greeks table is
three `portfolio` calls (the model alone, the selected trades alone, and the
two together — the last one the graph header's own key), so its numbers are
the ones behind the curves.

With a model drawn over selected trades the panel sends a third `risk_graph`
request — the drawn one with `model_legs: []` — and draws its lines as a
dashed, warm-grey ghost family under the combined curves
(`model.comparePosition`, persisted, on by default; `chartOption.ts`,
"compare"). Its `dates` are pinned to the combined response's own lines
(`effectiveDateSpecs`): with `dates: null` the server lays each request its
own ladder off *its* front expiry, and a model that adds a nearer expiration
would put the ghosts at other offsets than the lines they shadow. With nothing
selected the request is never sent — `trade_ids: []` without a model is the
whole book.

The one piece of server-side state the model has is the **extra quote
universe**. `set_model_contracts` hands the complete set of modeled
contracts to `MarketService::set_extra_universe`, then awaits one bounded
`refresh_once`, so the next graph prices each modeled leg off its own quote
rather than a neighbour's vol. The set lives on the running
`MarketService` — so, like everything else in a `Runtime`, it is gone after
a source change, a settings save or a demo toggle, and the SPA registers it
again on `app_ready` and `trades_changed`. The extras' quotes are part of
the snapshot (and so of the on-disk cached snapshot) like the book's.

### The ticket as thinkorswim orders

The ticket's **Orders** block lists the model as order lines a trader pastes
into thinkorswim. `service::model_orders` validates and nets the legs by the
very helper `model_position` uses, groups them with
`wingspan_core::tos_format::group` (named spreads first — an iron condor
stays one `IRON CONDOR`, an unbalanced position is decomposed into smaller
named 1:1 spreads, roots never mix), values each contract with the same
`value_now` a model leg is entered at, and writes each order with
`tos_format::render`. The formatter lives in Rust, beside the order parser,
for one reason: **every line it writes is round-trip tested through
`tos_order::parse`** — the line must parse back to the same signed legs,
side, strategy, quantity and price with no warnings, and the orders' legs
must sum back to the netted input (unit tests per strategy, plus a proptest
over random bags). The parser's hard-won grammar is the oracle; there is no
second implementation of it in TypeScript. The SPA renders the lines and
copies them: in the desktop app through the `copy_text` command (`arboard`
on a worker thread — the webview's `navigator.clipboard` cannot be relied
on under WebKitGTK, and no clipboard plugin permission is granted), in the
browser through `navigator.clipboard` (`web/src/lib/clipboard.ts`).

## Invariants

- All displayed greeks and all risk-graph curves use **our solved IVs** — the
  table and the graph always agree; the T+0 curve at spot equals the
  dashboard P&L — with or without model legs, which are entered at the
  current mark.
- Every order line `tos_format` writes parses back, through `tos_order`, to
  exactly the legs it was written from.
- Warnings are data with severity + anchors, surfaced in the UI; malformed
  input never panics.
- Per-trade attribution knowingly differs from broker/tax lots (the account
  nets across trades); reconciliation is quantity-only and says so.

## Extension points

| to add | touch |
|---|---|
| a strategy (e.g. `BACKRATIO`) | one row in the `Strategy` leg-template table in core |
| a market-data provider | one impl of `SpotSource`/`OptionQuoteSource` + a config entry in the fallback chain |
| an option-chain source (the Model tab) | override `expirations` + `chain` on that provider's `OptionQuoteSource` impl — both default to `Unsupported`, which the chain ladder skips with one sentence rather than counting as a failure |
| a statistic/column | one function in core/pricing + one column def in the SPA table |
| vol dynamics (e.g. sticky-delta) | one impl of the `VolDynamics` trait in pricing |
| an underlying | one row in the settlement-rules table (`contract.rs`); an index priced as a fraction of another (XSP = SPX ÷ 10) is one more row in `INDEX_PROXIES` there |
| a graph line type | one `GraphDate` variant + SPA legend entry |
| a trade source (e.g. a broker API) | one impl of `TradeSource` in `wingspan-app` + one `SourceRef`/`SourceKind` variant |

Manual leg entry is the seam this leaves open — `Vec<LegRow>` →
`book::from_legs` plus one `impl TradeSource`; `SourceRef::Manual`,
`SourceKind::Manual` and `LegOrigin::Manual` are reserved and a
`set_trade_source` request of `{ "kind": "manual" }` answers 422 today.
What is **not** designed: a persisted leg store with add/update/delete. The
Model tab's legs are deliberately not one (see above); the quote universe
for contracts not yet in the book is `MarketService`'s **extra** universe,
which every refresh quotes beside the book's own contracts.

See `docs/DATA-FORMATS.md` for the full parsing spec and `docs/DOMAIN.md`
for the domain rules, the provider specs and the acceptance numbers.
