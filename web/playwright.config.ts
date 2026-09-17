import { defineConfig, devices } from '@playwright/test'

/**
 * Playwright drives the development-only `wingspan-devserver serve --demo`
 * with the SPA embedded (`--features embed-ui`) — the axum server the SPA is
 * developed against, not the shipped Tauri desktop app. Nothing is mocked —
 * the dev-time mock layer in `src/api/mock.ts` is DEV-only and cannot latch
 * in a production bundle, and the suite asserts that it hasn't.
 *
 * Three servers are started:
 *   :8787  the anonymised test fixtures (`--demo --legs fixtures/trades/legs.csv`),
 *          a clean Legs CSV where every row parses and the golden numbers live;
 *   :8789  the same fixtures plus three appended rows (a blank-price vertical,
 *          a non-SPX futures row and an unparseable OCC code), so the
 *          null/degraded render paths (`avg_cost: null`, `sheet_leg_unpriced`,
 *          `sheet_row_skipped`) are exercised against real API responses;
 *   :8791  plain `serve --demo`: the wholly synthetic demo book a new user
 *          sees, checked by demo-book.spec.ts and photographed for the README.
 */

const HOST = '127.0.0.1'
const PORT = Number(process.env.WINGSPAN_E2E_PORT ?? 8787)
const EDGE_PORT = Number(process.env.WINGSPAN_E2E_EDGE_PORT ?? 8789)
const DEMO_PORT = Number(process.env.WINGSPAN_E2E_DEMO_PORT ?? 8791)

export const BASE_URL = `http://${HOST}:${PORT}`
export const EDGE_BASE_URL = `http://${HOST}:${EDGE_PORT}`
export const DEMO_BASE_URL = `http://${HOST}:${DEMO_PORT}`

export default defineConfig({
  testDir: './e2e',
  testMatch: '**/*.spec.ts',
  // The suite shares three servers and one screenshot baseline; serial keeps
  // both the fixture prices and the rendering load deterministic.
  fullyParallel: false,
  workers: 1,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  timeout: 30_000,
  expect: {
    timeout: 10_000,
    toHaveScreenshot: {
      // The demo surface is synthetic but not frozen: the fixture marks are
      // priced from `now`, so curves drift by a hair between runs and by a
      // day's theta between days. The baseline is taken with the graph pinned
      // (absolute price range, locked y-axis, σ bands and breakeven labels
      // off) so only the curve shapes matter, and the ratio absorbs the drift.
      maxDiffPixelRatio: 0.06,
      animations: 'disabled',
      scale: 'css',
    },
  },
  // One baseline per snapshot, shared by every platform: the committed image
  // is generated on Linux and CI runs Linux.
  snapshotPathTemplate: '{testDir}/__screenshots__/{arg}{ext}',
  reporter: process.env.CI
    ? [['list'], ['html', { open: 'never' }]]
    : [['list']],
  use: {
    baseURL: BASE_URL,
    trace: 'on-first-retry',
    screenshot: 'only-on-failure',
    viewport: { width: 1440, height: 900 },
    deviceScaleFactor: 1,
    locale: 'en-US',
    // No timezoneId. `serve --demo` no longer values the book against the host
    // clock — it pins the valuation date to the fixtures' own day — so "today"
    // is not what has to agree any more. What does is the *what-if date*: the
    // SPA seeds it from the browser's real today, and risk-graph.spec.ts
    // computes the expected date the same way, so both must be reading the same
    // calendar day. Pinning a timezone here would move the browser's today
    // without moving the machine's, and the two would disagree across midnight.
  },
  projects: [
    {
      name: 'chromium',
      use: {
        ...devices['Desktop Chrome'],
        viewport: { width: 1440, height: 900 },
        deviceScaleFactor: 1,
        launchOptions: {
          // Font hinting is the biggest source of pixel noise between
          // machines; turning it off makes the chart baseline portable.
          args: ['--font-render-hinting=none', '--disable-lcd-text'],
        },
      },
    },
  ],
  webServer: [
    {
      command: './e2e/support/serve.sh',
      url: `${BASE_URL}/api/v1/health`,
      // Locally (never in CI - reuseExistingServer is false there), a
      // process already answering on this port is reused as-is instead of
      // being rebuilt, even if it was started from a now-stale checkout of
      // the Rust or SPA source. That is the point for a fast local
      // edit/re-run loop, but it means a leftover `wingspan-devserver serve
      // --demo` from an earlier run can silently make the *next* run
      // exercise the old binary. Kill it (or just let CI, which always
      // rebuilds, be the final check) if a change to server/SPA source isn't
      // showing up.
      reuseExistingServer: !process.env.CI,
      timeout: 300_000,
      stdout: 'pipe',
      stderr: 'pipe',
      env: { WINGSPAN_E2E_PORT: String(PORT) },
    },
    {
      command: './e2e/support/serve-edge.sh',
      url: `${EDGE_BASE_URL}/api/v1/health`,
      // Same staleness trap as the main server above.
      reuseExistingServer: !process.env.CI,
      timeout: 300_000,
      stdout: 'pipe',
      stderr: 'pipe',
      env: { WINGSPAN_E2E_EDGE_PORT: String(EDGE_PORT) },
    },
    {
      command: './e2e/support/serve-demo.sh',
      url: `${DEMO_BASE_URL}/api/v1/health`,
      // Same staleness trap as the main server above.
      reuseExistingServer: !process.env.CI,
      timeout: 300_000,
      stdout: 'pipe',
      stderr: 'pipe',
      env: { WINGSPAN_E2E_DEMO_PORT: String(DEMO_PORT) },
    },
  ],
})
