# Architecture

```
trades dir ─parse→ Trades ─match/net→ Positions ─price(snapshot)→ Stats & Curves ─JSON/SSE→ SPA
```

The trades dir is a folder the user chooses on their own computer and the app
reads **where it is** — nothing is ever copied, moved or deleted, and Clear
only stops showing it (`<state_dir>/trades_root.json` records the choice). A
deployer may still set `[trades] root`, and `--demo` has its bundled book, but
neither is changeable at runtime and neither is required — with no book at
all the pipeline above simply runs on nothing.

Everything below the HTTP API is pure, synchronous, unit-testable Rust; I/O
(files, HTTP, the Python bridge) sits at the edges behind traits. Every
response body is produced by `service.rs`, which takes an `&AppState` and
plain DTOs and knows nothing about HTTP; `web/e2e/fixtures/contract/` is a
checked-in dump of all of them for the demo book, and CI fails if the code
stops producing it.

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

`web/` is a React 19 SPA speaking the frozen contract in `docs/API.md`; it is
embedded into the desktop app's webview (and, for the dev-only harness, into
`wingspan-devserver` via `rust-embed`).

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
