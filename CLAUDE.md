# Wingspan

SPX options position dashboard & risk analyzer. Rust workspace + React SPA,
single release binary. The full, authoritative plan is `docs/PLAN.md` — read it
before changing behavior; it encodes hard-won quirks of the TOS data formats.

## Layout
- `crates/wingspan-core` — domain model, TOS order/statement parsers, trade
  assembly, P&L, reconciliation. Pure + synchronous; no I/O beyond file reads.
- `crates/wingspan-pricing` — BSM, IV solver, forwards, bumped greeks,
  weighted vega, risk-graph engine.
- `crates/wingspan-marketdata` — provider traits + Schwab/yfinance/cache/
  statement-IV/fixture sources and the fallback chain.
- `crates/wingspan-app` — transport-free library: config, state, the service
  layer, events, file watcher, diagnostics, demo book.
- `crates/wingspan-cli` — `wingspan-cli` binary: `validate`, `risk`, `doctor`,
  `dump-contract`.
- `crates/wingspan-devserver` — **dev-only** `wingspan-devserver` binary: clap
  CLI, axum API, SSE, embedded SPA (`--features embed-ui`); never shipped,
  always bound to `127.0.0.1`.
- `web/src-tauri/` — crate `wingspan-desktop`: the shipped Tauri desktop app,
  executable `wingspan`. This is the product.
- `web/` — React 19 + TS + Vite + Tailwind v4 + ECharts + TanStack Query +
  Zustand SPA. See `docs/API.md` for the frozen API contract.
- `fixtures/trades/` — anonymised statements + trade files (tests only).
  NEVER commit anything from `~/workspace/example_trades` un-anonymised.
- `fixtures/demo/` — the wholly synthetic book `wingspan-devserver serve
  --demo` shows, written by `scripts/make_demo_book.py` (never hand-edit;
  regenerate and commit).

## Rules
- `just ci` must be green before any commit lands.
- Money/strikes are `rust_decimal::Decimal`; vols/greeks are `f64`.
- Statement times are America/Chicago; expiry instants America/New_York.
- Settlement/root come from OPRA codes or `(Weeklys)`/`[AM]` tokens — never
  from the calendar.
- Warnings are data (see `Warning` in core), surfaced in the UI — never panic
  on malformed input.
- Secrets (Schwab tokens, account numbers) are never logged. The Schwab token
  file is **read-only to this app**: it is produced, refreshed and owned by
  other tooling on the server, wingspan is only handed its path, and nothing
  in the codebase may write, create, rename, chmod, lock, copy or back up
  anything in that directory. A refreshed access token lives only in memory.
- Node is installed via nvm: `export NVM_DIR="$HOME/.nvm" && . "$NVM_DIR/nvm.sh"`
  before any npm/node command in non-interactive shells.
