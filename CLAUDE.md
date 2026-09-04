# Wingspan

SPX options position dashboard & risk analyzer. Rust workspace + React SPA,
shipped as a Tauri desktop app — not a server. The full, authoritative plan
is `docs/PLAN.md` — read it before changing behavior; it encodes hard-won
quirks of the TOS data formats.

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
  executable `wingspan`. **This is the product.**
- `web/` — React 19 + TS + Vite + Tailwind v4 + ECharts + TanStack Query +
  Zustand SPA. See `docs/IPC.md` for the frozen contract.
- `fixtures/trades/` — anonymised statements + trade files (tests only).
  NEVER commit anything from `~/workspace/example_trades` un-anonymised.
- `fixtures/demo/` — the wholly synthetic book "Load demo book" (or
  `wingspan-devserver serve --demo`) shows, written by
  `scripts/make_demo_book.py` (never hand-edit; regenerate and commit).

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
  other tooling, wingspan is only handed its path, and nothing in the
  codebase may write, create, rename, chmod, lock, copy or back up anything
  in that directory. A refreshed access token lives only in memory.
- Node is installed via nvm: `export NVM_DIR="$HOME/.nvm" && . "$NVM_DIR/nvm.sh"`
  before any npm/node command in non-interactive shells.
- This box has no display. Run the desktop window under `xvfb-run -a`, with
  `WEBKIT_DISABLE_COMPOSITING_MODE=1`, `WEBKIT_DISABLE_DMABUF_RENDERER=1` and
  `DBUS_SESSION_BUS_ADDRESS=unix:path=/nonexistent/wingspan-no-portal` set —
  the D-Bus variable is not optional: without it the app hangs indefinitely
  in an `xdg-desktop-portal` call Xvfb has no portal to answer. See
  `docs/DESKTOP.md` §13.
- `WEBKIT_INSPECTOR_SERVER`/`_HTTP_SERVER` are cleared before WebKitGTK
  reads them, so no build opens a remote-inspector socket — with one
  deliberate exception, `TAURI_WEBVIEW_AUTOMATION=true`, without which the
  WebDriver smoke (`just smoke`) cannot drive the shipped binary at all.
  Do not "tidy" that condition away; see `docs/DESKTOP.md` §13.
- No `remove_dir_all` anywhere in application code except
  `migrate::delete_discarded_uploads`, which carries an explicit `#[allow]`
  at its one call site; `clippy.toml` enforces this.
- Every Tauri command's payload passes as a single `request` argument —
  Tauri snake_cases camelCase JS keys, but `types.ts` is already snake_case,
  so spreading a body across separate parameters fails at runtime.
- `config::load` runs before `tauri::Builder::default()`; a config load
  error is a dialog plus a boot on built-in defaults — never a silent exit,
  because a double-clicked application has no stderr to read.
- No command that changes what the app executes, or where it reads a secret
  from, takes a path from JS. The four path-shaped settings (`[yfinance]
  python`, `[schwab] token_path`/`env_file`/`token_config`) are written only
  by Rust-side pickers, each behind its own confirmation dialog naming the
  file and what it will be used for.
