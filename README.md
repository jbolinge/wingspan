# Wingspan

**SPX options position dashboard & risk analyzer** — parses thinkorswim trade
files and account statements into per-trade positions, prices them with live
(Schwab) or delayed (yfinance) data, and draws an OptionNetExplorer-style risk
graph. Rust core, React front end, one release binary.

> Butterflies have wings; the risk graph spans price.

## Quick start

```bash
# demo mode with the bundled anonymised fixtures + synthetic market data
cargo run -p wingspan-server -- serve --demo
# then open http://127.0.0.1:8787
```

Point it at your own trades folder:

```bash
cp wingspan.example.toml wingspan.toml   # edit [trades] root
cargo run -p wingspan-server -- serve
```

## Documentation

- `docs/PLAN.md` — full product/architecture plan
- `docs/CONFIG.md` — every config key
- `docs/SCHWAB.md` — token file, refresh, API usage
- `docs/DATA-FORMATS.md` — TOS order grammar + statement dialect
- `docs/ARCHITECTURE.md` — crate map + extension points
- `docs/DEPLOY.md` — production checklist, systemd unit

## Development

```bash
just ci        # fmt, clippy, tests, web typecheck/lint/test/build
just e2e       # Playwright against `wingspan serve --demo`
just release   # single binary with the SPA embedded
```
