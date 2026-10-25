# Wingspan

**SPX options position dashboard & risk analyzer.** Wingspan imports the
legs of your trades from a Google Sheet or a CSV, prices them with live
(Schwab or tastytrade) or delayed (yfinance) market data, and draws an
OptionNetExplorer-style risk graph. It is a Tauri desktop app for Linux:
a Rust core with a React front end in a WebKitGTK window. Nothing leaves
your machine except the market-data and sheet requests you configure.

> Butterflies have wings; the risk graph spans price.

![Portfolio cards, the open-positions blotter and the risk graph](docs/screenshots/dashboard.png)

## What it does

- **Positions** — every open trade's DIT/DTE, open and realized P&L,
  greeks and weighted vega, with a selected-subset view and live totals.
  Butterflies, verticals, calendars, diagonals and ratio spreads are
  assembled from the sheet's legs; four columns (`Trade`, `Side`, `Qty`,
  `OCC Symbol`) are all it needs. An XSP (Mini-SPX) hedge in the sheet is
  priced off SPX automatically — one XSP contract counts as a tenth of the
  SPX one — so it sits on the same risk graph as your SPX trades.
- **Risk graph** — the expiration line plus a T+x ladder, breakevens,
  ±1σ/2σ bands, probability of profit and a what-if panel, driven by the
  app's own solved IVs so the graph and the table always agree. Zoom
  25–500%, drag to pan, and a T+0 strip of P&L, Δ, Γ, Θ, vega and weighted
  vega under the plot.
- **Model** — the SPX option chain (calls left, puts right, ITM shaded) one
  expiration at a time, with a `Pos` column for what you already hold, and
  an **SPX | XSP** switch that shows the same chain as Mini-SPX (SPX ÷ 10). Type
  a quantity into any contract to add it to the risk graph at today's mark —
  or click its ask to buy one and its bid to sell one — drag it to another
  strike, and see the position as it stands ghosted
  behind the model. The ticket shows Model / Position / Total greeks and
  writes the model out as thinkorswim order lines you can copy — an XSP
  leg's line says `XSP`. **Setup** buttons (M200 CD, M200 CDS, STT) build a
  named trade from the chain in one click: each leg fitted to its target
  delta, a balanced butterfly's wings kept equal, the expirations nearest
  the plan's DTE and of the kind it allows — ready for the risk graph and
  for thinkorswim.
- **Market data** — Schwab or tastytrade live quotes, falling back to a
  yfinance bridge, a cached snapshot or statement marks, each labelled
  honestly with a status pill and per-cell staleness.
- **Data health** and **Closed trades** — every warning with its fix, and
  the history with realized P&L, fees, win rate and average days held.

![The Model tab: the option chain, a modeled spread and the ticket](docs/screenshots/model.png)

## Install

The shipped product is a `.deb` for **Ubuntu 26.04 LTS**. Download it and
`SHA256SUMS` from the [Releases page](https://github.com/jbolinge/wingspan/releases),
then follow [`docs/DESKTOP.md`](docs/DESKTOP.md) to install, verify and
configure it.

On first run choose **Load demo book** (a wholly synthetic book, valued on
its own day) or **Import trades…** and paste the link of your tracker's
**Legs** tab, shared as anyone-with-link → Viewer, or pick a CSV export of
it. Wingspan shows which columns it found before loading anything, caches
the last fetch so an offline launch still shows the book (marked stale),
and **Clear trades** deletes nothing of yours. Market data is configured in
**Settings**; see [`docs/SCHWAB.md`](docs/SCHWAB.md),
[`docs/TASTYTRADE.md`](docs/TASTYTRADE.md) and
[`docs/CONFIG.md`](docs/CONFIG.md).

## Building from source

Prerequisites: the Rust toolchain in `rust-toolchain.toml`, Node 24,
[`just`](https://github.com/casey/just), Python 3 and the WebKitGTK 4.1 /
GTK 3 dev packages (`docs/DESKTOP.md` §13 lists them).

```bash
just dev      # the desktop window in dev mode against Vite
just ci       # every check CI runs: fmt, clippy, tests, web lint/types/tests, contract, e2e
just bundle   # the shipped .deb, a release build with the SPA embedded
just smoke    # drive the built .deb through tauri-driver + WebKitWebDriver
```

Web recipes run `npm ci` themselves when `web/node_modules` is missing or
stale. The dev-only `wingspan-devserver` exposes the same service layer over
HTTP on `127.0.0.1` for Vite and Playwright; it is never shipped.

## Layout

| Path | What it is |
| --- | --- |
| `crates/wingspan-core` | Domain model, Legs table parser, leg assembly, P&L, reconciliation |
| `crates/wingspan-pricing` | Black-Scholes, IV solver, forwards, greeks, weighted vega, risk-graph engine |
| `crates/wingspan-marketdata` | Schwab, tastytrade, yfinance, cache and statement sources, and the fallback chain |
| `crates/wingspan-app` | Transport-free application layer: config, state, services, trade sources, diagnostics |
| `crates/wingspan-cli` | `wingspan-cli` — `validate`, `risk`, `doctor`, fixture tooling; ships as a sidecar |
| `crates/wingspan-devserver` | Dev-only HTTP/SSE server with the embedded SPA |
| `web/src-tauri` | `wingspan-desktop` — the shipped Tauri app |
| `web/` | React 19 + TypeScript + Vite + Tailwind + ECharts SPA |
| `fixtures/` | Anonymised test fixtures and the synthetic demo book |

## Documentation

- [`docs/DESKTOP.md`](docs/DESKTOP.md) — install, configure, run, build
- [`docs/CONFIG.md`](docs/CONFIG.md) — every config key
- [`docs/SCHWAB.md`](docs/SCHWAB.md) / [`docs/TASTYTRADE.md`](docs/TASTYTRADE.md) — the market-data providers
- [`docs/DATA-FORMATS.md`](docs/DATA-FORMATS.md) — the Legs table spec
- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — crate map and extension points
- [`docs/IPC.md`](docs/IPC.md) — the frozen IPC contract
- [`docs/DOMAIN.md`](docs/DOMAIN.md) — domain rules, provider specs, golden numbers

## License

Copyright © 2026 Jason Olinger. Wingspan is free software, released under
the [GNU General Public License v3.0 or later](LICENSE).
