import { defineConfig, devices } from '@playwright/test'

/**
 * Playwright drives the *real* binary: `wingspan serve --demo` with the SPA
 * embedded (`--features embed-ui`), i.e. exactly what ships. Nothing is
 * mocked — the dev-time mock layer in `src/api/mock.ts` is DEV-only and cannot
 * latch in a production bundle, and the suite asserts that it hasn't.
 *
 * Two servers are started:
 *   :8787  the demo fixtures, where every order matches an execution;
 *   :8789  the same fixtures plus one unmatched order, so the null/degraded
 *          render paths (no average cost, unmatched warning, reconciliation
 *          mismatch) are exercised against real API responses.
 */

const HOST = '127.0.0.1'
const PORT = Number(process.env.WINGSPAN_E2E_PORT ?? 8787)
const EDGE_PORT = Number(process.env.WINGSPAN_E2E_EDGE_PORT ?? 8789)

export const BASE_URL = `http://${HOST}:${PORT}`
export const EDGE_BASE_URL = `http://${HOST}:${EDGE_PORT}`

export default defineConfig({
  testDir: './e2e',
  testMatch: '**/*.spec.ts',
  // The suite shares two servers and one screenshot baseline; serial keeps
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
    // No timezoneId: the browser must agree with the server about "today",
    // and the server uses the host clock.
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
      reuseExistingServer: !process.env.CI,
      timeout: 300_000,
      stdout: 'pipe',
      stderr: 'pipe',
      env: { WINGSPAN_E2E_BIND: `${HOST}:${PORT}` },
    },
    {
      command: './e2e/support/serve-edge.sh',
      url: `${EDGE_BASE_URL}/api/v1/health`,
      reuseExistingServer: !process.env.CI,
      timeout: 300_000,
      stdout: 'pipe',
      stderr: 'pipe',
      env: { WINGSPAN_E2E_EDGE_BIND: `${HOST}:${EDGE_PORT}` },
    },
  ],
})
