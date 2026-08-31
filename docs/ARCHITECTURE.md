# Architecture

```
trades dir ─parse→ Trades ─match/net→ Positions ─price(snapshot)→ Stats & Curves ─JSON/SSE→ SPA
```

Everything below the HTTP API is pure, synchronous, unit-testable Rust; I/O
(files, HTTP, the Python bridge) sits at the edges behind traits.

## Crates

| crate | contents | depends on |
|---|---|---|
| `wingspan-core` | domain model, TOS order parser, statement parser, trade assembly, P&L, reconciliation, warnings; shared market value types (`market.rs`) | — |
| `wingspan-pricing` | BSM, IV solver, forward calibration (put-call parity), bump-and-reprice greeks, weighted vega, risk-graph engine, what-if | core |
| `wingspan-marketdata` | `SpotSource`/`OptionQuoteSource` traits; Schwab, yfinance bridge, cached snapshot, statement-IV, fixture providers; fallback chain + refresh loop | core, pricing |
| `wingspan-server` | `wingspan` binary: clap CLI, figment config, axum API + SSE, file watcher, embedded SPA (`embed-ui`) | all |

The shared market value types live in **core**, not marketdata, to keep the
dependency graph acyclic (pricing consumes snapshots; marketdata's
statement-IV provider consumes the pricing IV solver).

`web/` is a React 19 SPA speaking the frozen contract in `docs/API.md`; it is
embedded into the release binary via `rust-embed`.

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
