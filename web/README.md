# Wingspan web UI

React 19 + TypeScript SPA for the Wingspan options dashboard. Built against the
frozen HTTP contract in `../docs/API.md`; `src/api/types.ts` mirrors it field
for field.

## Running

```sh
export NVM_DIR="$HOME/.nvm" && . "$NVM_DIR/nvm.sh"   # non-interactive shells
npm install
npm run dev            # http://localhost:5173, /api proxied to 127.0.0.1:8787
VITE_MOCK=1 npm run dev  # never touch the network; serve src/api/mock.ts
```

If `wingspan-devserver serve` is not listening, the dev build falls back to
the mock layer automatically and the status bar says so.

| Script              | What it does                          |
| ------------------- | ------------------------------------- |
| `npm run typecheck` | `tsc -b`, strict                      |
| `npm run lint`      | oxlint                                |
| `npm run test`      | Vitest (`-- --run` for a single pass) |
| `npm run build`     | typecheck + production bundle to `dist/` |

## Layout

```
src/
  api/       types.ts (the contract), client.ts, queries.ts, events.ts (SSE), mock.ts
  lib/       pure logic: formatters, dates, aggregation, provider pill, BSM,
             chart option, crosshair readout, palette
  store/     Zustand: selection, sort, graph settings, theme (localStorage)
  components/ status bar, positions table, portfolio cards, closed + health tabs
  components/graph/ risk graph, controls, date editor, what-if, readouts
```

Everything that can be a pure function is one, in `src/lib`, and is unit
tested; components are thin. The mock provider prices the same leg list that
feeds the table, the graph and the what-if panel, so demo mode obeys the
documented invariant — the `t0` line at spot equals the dashboard `pnl_total`.

## Theming

Two palettes live as CSS custom properties selected by `<html data-theme>`;
Tailwind colour utilities resolve through them, so a theme switch is one
attribute flip. Categorical colours are the Okabe-Ito colour-blind-safe set:
profit is bluish-green and loss vermillion, which stay distinguishable under
deuteranopia and protanopia.
