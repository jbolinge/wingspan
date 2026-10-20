# Wingspan web UI

React 19 + TypeScript SPA for the Wingspan options dashboard. Built against the
frozen contract in `../docs/IPC.md`; `src/api/types.ts` mirrors it field
for field. Inside the shipped desktop app every request is a Tauri `invoke`
command (`src/api/client.ts`); in the browser dev loop the same call becomes
an HTTP request against `wingspan-devserver`.

## Running

```sh
export NVM_DIR="$HOME/.nvm" && . "$NVM_DIR/nvm.sh"   # non-interactive shells
npm ci
npm run dev            # http://localhost:5173, /api proxied to 127.0.0.1:8787
VITE_MOCK=1 npm run dev  # never touch the network; serve src/api/mock.ts
npm run tauri dev      # the real desktop window (or `just dev` from the repo root)
```

If `wingspan-devserver serve` is not listening, the browser dev build falls
back to the mock layer automatically and the status bar says so; inside
`npm run tauri dev` there is no HTTP fallback to reach for — every command
goes straight to the app's own IPC.

The webview's `localStorage` (selection state, sort, theme) lives at its own
origin, `tauri://localhost`, entirely separate from the browser dev loop's
`http://localhost:5173`; it persists across launches of the desktop app but
is invisible to (and unaffected by) anything opened in a regular browser.

| Script              | What it does                          |
| ------------------- | ------------------------------------- |
| `npm run typecheck` | `tsc -b`, strict                      |
| `npm run typecheck:e2e` | the same, for `e2e/` and `e2e-wdio/`  |
| `npm run lint`      | oxlint                                |
| `npm run test`      | Vitest (`-- --run` for a single pass) |
| `npm run build`     | typecheck + production bundle to `dist/` |
| `npm run e2e`       | Playwright, against `wingspan-devserver` |
| `npm run e2e:wdio`  | the release binary `just bundle` produces (`target/release/wingspan`), driven through `tauri-driver` + WebKitWebDriver (`just smoke` wraps this with the Xvfb/D-Bus workarounds a headless box needs) |
| `npm run screenshots` | the README captures, from the demo devserver on `:8791` |

## Layout

```
src/
  api/       types.ts (the contract), client.ts, queries.ts, events.ts
             (Tauri events, or SSE in the dev loop), epoch.ts (rebuild
             counter), mock.ts
  lib/       pure logic: formatters, dates, aggregation, provider pill, BSM,
             chart option, greek strip, price slices, palette, model (chain
             keys, Pos column, strike window, model legs on the wire)
  store/     Zustand: selection, sort, graph settings, theme, modeled legs
             (localStorage)
  components/ status bar, positions table, portfolio cards, closed + health tabs
  components/graph/ risk graph, controls, date editor, what-if, P&L inset, price slices
  components/model/ Model tab: expiry strip, option chain, quantity cell, ticket
```

Modeled legs are UI state, never part of the book: they ride on the
`risk_graph`, `what_if` and `portfolio` requests as `model_legs`, entered at
today's mark, so the T+0 invariant below holds with a model drawn too.

Everything that can be a pure function is one, in `src/lib`, and is unit
tested; components are thin. The mock provider prices the same leg list that
feeds the table, the graph and the what-if panel, so demo mode obeys the
documented invariant — the `t0` line at spot equals the dashboard `pnl_total`.
The mock values everything on its own pinned instant, `MOCK_NOW` (Monday
2026-08-31, noon New York — the Rust demo's clock), never the browser's, so
its fixed 2026 expiries cannot rot as the calendar moves. The vitest run pins
`Date` too (`vitest.setup.ts`, 2026-09-15 noon New York, before every test);
a test that sets its own system time overrides it for that test only.

## Theming

Two palettes live as CSS custom properties selected by `<html data-theme>`;
Tailwind colour utilities resolve through them, so a theme switch is one
attribute flip. Categorical colours are the Okabe-Ito colour-blind-safe set:
profit is bluish-green and loss vermillion, which stay distinguishable under
deuteranopia and protanopia.
