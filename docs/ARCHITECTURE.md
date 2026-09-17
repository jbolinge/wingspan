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
| `wingspan-core` | domain model, a source-agnostic `LegRow` intermediate and the Legs CSV parser, trade assembly, P&L, reconciliation, warnings; shared market value types (`market.rs`) (TOS order and statement parsers, now reached only from the core tests and `wingspan-cli export-legs`) | — |
| `wingspan-pricing` | BSM, IV solver, forward calibration (put-call parity), bump-and-reprice greeks, weighted vega, risk-graph engine, what-if | core |
| `wingspan-marketdata` | `SpotSource`/`OptionQuoteSource` traits; Schwab, tastytrade, yfinance bridge, cached snapshot, statement-IV, fixture providers; fallback chain + refresh loop | core, pricing |
| `wingspan-app` | transport-free library: figment config, state, the service layer (`service.rs`), events, trade sources (sheet fetch, CSV, demo) + the on-disk source cache, diagnostics, demo book | all |
| `wingspan-cli` | `wingspan-cli` binary: `validate`, `risk`, `doctor`, `dump-contract`, `export-legs` | wingspan-app |
| `wingspan-devserver` | **dev-only** `wingspan-devserver` binary: axum API + SSE over `wingspan-app`'s service layer, embedded SPA (`embed-ui`) — for Playwright and the Vite dev loop, never shipped, always bound to `127.0.0.1` | wingspan-app |
| `web/src-tauri` (crate `wingspan-desktop`) | the shipped product: Tauri desktop app, shipped executable `wingspan` | wingspan-app |

The shared market value types live in **core**, not marketdata, to keep the
dependency graph acyclic (pricing consumes snapshots; marketdata's
statement-IV provider consumes the pricing IV solver).

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
| a trade source (e.g. a broker API) | one impl of `TradeSource` in `wingspan-app` + one `SourceRef`/`SourceKind` variant |

Manual leg entry is the seam this leaves open — `Vec<LegRow>` →
`book::from_legs` plus one `impl TradeSource`; `SourceRef::Manual`,
`SourceKind::Manual` and `LegOrigin::Manual` are reserved and a
`set_trade_source` request of `{ "kind": "manual" }` answers 422 today.
What is **not** designed: a persisted leg store with add/update/delete, and
a quote universe for contracts not yet in the book.

See `docs/DATA-FORMATS.md` for the full parsing spec and `docs/DOMAIN.md`
for the domain rules, the provider specs and the acceptance numbers.
