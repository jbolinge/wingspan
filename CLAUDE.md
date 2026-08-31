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
- `crates/wingspan-server` — `wingspan` binary: clap CLI, config, axum API,
  SSE, watcher, embedded SPA (`--features embed-ui`).
- `web/` — React 19 + TS + Vite + Tailwind v4 + ECharts + TanStack Query +
  Zustand SPA. See `docs/API.md` for the frozen API contract.
- `fixtures/trades/` — anonymised statements + trade files (tests, `--demo`).
  NEVER commit anything from `~/workspace/example_trades` un-anonymised.

## Rules
- `just ci` must be green before any commit lands.
- Money/strikes are `rust_decimal::Decimal`; vols/greeks are `f64`.
- Statement times are America/Chicago; expiry instants America/New_York.
- Settlement/root come from OPRA codes or `(Weeklys)`/`[AM]` tokens — never
  from the calendar.
- Warnings are data (see `Warning` in core), surfaced in the UI — never panic
  on malformed input.
- Secrets (Schwab tokens, account numbers) are never logged; token files are
  rewritten atomically with a timestamped backup.
- Node is installed via nvm: `export NVM_DIR="$HOME/.nvm" && . "$NVM_DIR/nvm.sh"`
  before any npm/node command in non-interactive shells.
