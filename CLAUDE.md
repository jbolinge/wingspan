# Wingspan

SPX options position dashboard & risk analyzer. Rust workspace + React SPA,
shipped as a Tauri desktop app for Ubuntu 26.04 — not a server. The
authoritative documents are `docs/ARCHITECTURE.md` (crate map and the IPC
boundary), `docs/IPC.md` (the frozen contract), `docs/DESKTOP.md`
(install/run/build) and `docs/DATA-FORMATS.md` (the Legs table spec, plus the
test-only TOS grammar — the hard-won quirks live there). `docs/DOMAIN.md` has the domain rules, the
provider specs and the golden acceptance numbers.

## Layout
- `crates/wingspan-core` — domain model, the Legs table parser and leg
  assembly, P&L, reconciliation (the TOS order/statement parsers stay here,
  test-only). Pure + synchronous; no I/O beyond file reads.
- `crates/wingspan-pricing` — BSM, IV solver, forwards, bumped greeks,
  weighted vega, risk-graph engine.
- `crates/wingspan-marketdata` — provider traits + Schwab/tastytrade/
  yfinance/cache/statement-IV/fixture sources and the fallback chain.
- `crates/wingspan-app` — transport-free library: config, state, the service
  layer, events, trade sources (the Google Sheet fetch, the CSV file, the
  cache), diagnostics, demo book.
- `crates/wingspan-cli` — `wingspan-cli` binary: `validate`, `risk`, `doctor`,
  `dump-contract`, `export-legs` (fixture tooling). Ships alongside the app
  as a sidecar.
- `crates/wingspan-devserver` — **dev-only** `wingspan-devserver` binary: clap
  CLI, axum API, SSE, embedded SPA (`--features embed-ui`); never shipped,
  always bound to `127.0.0.1`. It exists so Playwright and the Vite dev loop
  can drive the same service layer over HTTP.
- `web/src-tauri/` — crate `wingspan-desktop`: the shipped Tauri desktop app,
  executable `wingspan`. **This is the product.**
- `web/` — React 19 + TS + Vite + Tailwind v4 + ECharts + TanStack Query +
  Zustand SPA. See `docs/IPC.md` for the frozen contract.
- `fixtures/trades/` — anonymised statements + trade files, plus the
  `legs.csv` exported from them by `wingspan-cli export-legs` (tests only).
  NEVER commit anything from `~/workspace/example_trades` un-anonymised.
- `fixtures/demo/` — the wholly synthetic book "Load demo book" (or
  `wingspan-devserver serve --demo`) shows, written by
  `scripts/make_demo_book.py` (never hand-edit; regenerate and commit).

## Rules
- `just ci` must be green before any commit lands. It includes the desktop
  crate's clippy and tests, so it needs the WebKitGTK dev packages
  (`docs/DESKTOP.md` §13); it builds `web/dist` and the CLI sidecar itself.
- Money/strikes are `rust_decimal::Decimal`; vols/greeks are `f64`.
- Statement times are America/Chicago; expiry instants America/New_York.
- Settlement/root come from OPRA codes or `(Weeklys)`/`[AM]` tokens — never
  from the calendar.
- Warnings are data (see `Warning` in core), surfaced in the UI — never panic
  on malformed input.
- Secrets (Schwab tokens and account numbers, tastytrade client secrets and
  refresh tokens) are never logged. The Schwab token file is **read-only to
  this app**: it is produced, refreshed and owned by other tooling, wingspan
  is only handed its path, and nothing in the codebase may write, create,
  rename, chmod, lock, copy or back up anything in that directory. A
  refreshed access token lives only in memory. tastytrade has no token file
  at all: its refresh token is a static environment variable and its access
  token lives only in memory. A Google Sheet share link is bearer-equivalent
  and handled the same way: masked in `Config::redacted()`, in `Debug`, in
  every DTO and in every log line, never on `SettingsPatch`, and never in a
  fixture, doc or test — `scripts/check_redaction.py` fails the build on a
  real spreadsheet id.
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
- No `remove_dir_all` anywhere in application code (R2); `clippy.toml`
  enforces it.
- Every Tauri command's payload passes as a single `request` argument —
  Tauri snake_cases camelCase JS keys, but `types.ts` is already snake_case,
  so spreading a body across separate parameters fails at runtime.
- `config::load` runs before `tauri::Builder::default()`; a config load
  error is a dialog plus a boot on built-in defaults — never a silent exit,
  because a double-clicked application has no stderr to read.
- No command that changes what the app executes, or where it reads a secret
  from, takes a path from JS. The four path-shaped settings (`[yfinance]
  python`, `[schwab] token_path`/`env_file`, `[tastytrade] env_file`) are
  written only by Rust-side pickers, each behind its own confirmation dialog
  naming the file and what it will be used for.
