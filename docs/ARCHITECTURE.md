# Architecture

```
trades dir ─parse→ Trades ─match/net→ Positions ─price(snapshot)→ Stats & Curves ─JSON/SSE→ SPA
```

The trades dir is a folder on this machine the user picked, and the app
reads it **where it is** — nothing is ever copied, moved or deleted, and Clear
only stops showing it (`<state_dir>/trades_root.json` records the choice). A
deployer may still set `[trades] root`, and `--demo` has its bundled book, but
neither is changeable at runtime and neither is required — with no book at
all the pipeline above simply runs on nothing.

Everything below the service layer is pure, synchronous, unit-testable Rust;
I/O (files, HTTP, the Python bridge) sits at the edges behind traits. Every
response body is produced by `service.rs`, which takes an `&AppState` and
plain DTOs and knows nothing about any transport; `web/e2e/fixtures/contract/`
is a checked-in dump of all of them for the demo book, and CI fails if the
code stops producing it.

## Crates

| crate | contents | depends on |
|---|---|---|
| `wingspan-core` | domain model, TOS order parser, statement parser, trade assembly, P&L, reconciliation, warnings; shared market value types (`market.rs`) | — |
| `wingspan-pricing` | BSM, IV solver, forward calibration (put-call parity), bump-and-reprice greeks, weighted vega, risk-graph engine, what-if | core |
| `wingspan-marketdata` | `SpotSource`/`OptionQuoteSource` traits; Schwab, yfinance bridge, cached snapshot, statement-IV, fixture providers; fallback chain + refresh loop | core, pricing |
| `wingspan-app` | transport-free library: figment config, state, the service layer (`service.rs`), events, file watcher, diagnostics, demo book | all |
| `wingspan-cli` | `wingspan-cli` binary: `validate`, `risk`, `doctor`, `dump-contract` | wingspan-app |
| `wingspan-devserver` | **dev-only** `wingspan-devserver` binary: axum API + SSE over `wingspan-app`'s service layer, file watcher wiring, embedded SPA (`embed-ui`) — for Playwright and the Vite dev loop, never shipped, always bound to `127.0.0.1` | wingspan-app |
| `web/src-tauri` (crate `wingspan-desktop`) | the shipped product: Tauri desktop app, shipped executable `wingspan` | wingspan-app |

The shared market value types live in **core**, not marketdata, to keep the
dependency graph acyclic (pricing consumes snapshots; marketdata's
statement-IV provider consumes the pricing IV solver).

`web/` is a React 19 SPA speaking the frozen contract in `docs/IPC.md`; it is
bundled by Tauri from `web/dist` into the desktop app's webview (and, for the
dev-only harness, into `wingspan-devserver` via `rust-embed`).

## The IPC boundary

The SPA never speaks HTTP inside the shipped app. Every request the old API
made as a route is now a Tauri `invoke` command over
`web/src-tauri/src/commands.rs` and `settings_commands.rs` — one
`#[tauri::command]` per former handler, calling the same
`wingspan_app::service::*` functions. Three rules
hold for all of them: every payload travels as one `request` argument
(Tauri snake_cases camelCase JS keys, but `types.ts` is already
snake_case); every command is `async` and does its work behind a
`JoinHandle`, so a panic becomes a 500-shaped error the SPA can render
instead of a spinner that never resolves (a *synchronous* command that
panics runs inside WebKit's own callback and takes the whole process down);
and no command that changes what the app executes, or where it reads a
secret from, takes a path from JS — the four path-shaped settings are
written only by Rust-side pickers, each behind its own confirmation dialog.
`docs/IPC.md` is the frozen shape; this is the mechanism that serves it.

## The pending-boot `AppSlot`

`AppState::build` can take up to ~40 seconds on a cold Schwab launch — the
window is shown before it finishes, because a window that shows nothing for
40 seconds looks like one that failed to start. Every command reads the
application through `AppSlot`, a `tokio::sync::watch` cell: before boot
publishes, a command **pends** rather than answering an error, so the SPA's
spinner is exactly what a not-yet-listening socket used to produce. `health`
is the one exception — it reads the slot without awaiting it, because the
SPA asks precisely whether the app is up yet.

## A root change is a restart

Changing the trades folder, entering or leaving the demo book, and writing
a settings patch that touches the provider chain all go through the same
path: the whole `Runtime` — `AppState`, its background refresh loop, its
event pump — is swapped for a freshly built one, never mutated in place.
The slot is emptied first, so a command that arrives mid-swap pends on the
next value instead of reading a half-changed application; the old runtime
is dropped only after the new one is published, which is what aborts its
refresh loop and event pump cleanly. This is what makes "Load demo book"
and a Settings save safe operations rather than half-applied ones: there is
no state to leave inconsistent, because there is only ever one `Runtime` in
force.

## Invariants

- All displayed greeks and all risk-graph curves use **our solved IVs** — the
  table and the graph always agree; the T+0 curve at spot equals the
  dashboard P&L.
- Warnings are data with severity + anchors, surfaced in the UI; malformed
  input never panics.
- Per-trade attribution knowingly differs from broker/tax lots (the account
  nets across trades); reconciliation is quantity-only and says so.

## Extension points

| to add | touch |
|---|---|
| a strategy (e.g. `BACKRATIO`) | one row in the `Strategy` leg-template table in core |
| a market-data provider | one impl of `SpotSource`/`OptionQuoteSource` + a config entry in the fallback chain |
| a statistic/column | one function in core/pricing + one column def in the SPA table |
| vol dynamics (e.g. sticky-delta) | one impl of the `VolDynamics` trait in pricing |
| an underlying | one row in the settlement-rules table (`contract.rs`) |
| a graph line type | one `GraphDate` variant + SPA legend entry |

See `docs/DATA-FORMATS.md` for the full parsing spec and
`docs/PLAN.md` for the complete product plan.
