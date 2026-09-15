/** The one screenshot diff of the risk chart.
 *
 *  Only the chart element is captured, so the status bar's clock — the one
 *  genuinely unstable thing on the page — is outside the frame; the demo
 *  snapshot's `as_of` cannot reach the baseline. Inside the frame the graph is
 *  pinned (absolute price range, locked y-axis, no σ bands or breakeven
 *  labels: see PINNED_GRAPH) so the geometry is fixed run to run, and the
 *  generous `maxDiffPixelRatio` in playwright.config.ts absorbs the sub-pixel
 *  drift of curves priced from `now`. That ratio is a drift absorber, not a
 *  regression detector: a change confined to one part of the chart — the whole
 *  greek strip redrawn, say — stays comfortably under it, so this diff is the
 *  backstop and the unit tests in src/lib are where chart geometry is pinned.
 *
 *  Refresh the baseline with `just e2e-update` (`npm run e2e:update`), which
 *  passes `--update-snapshots=all`. The bare `--update-snapshots` will NOT do:
 *  Playwright reads it as mode `changed`, which rewrites only snapshots whose
 *  comparison actually failed — so any in-tolerance change leaves the stale
 *  baseline in place while reporting success. */

import { expect, test } from '@playwright/test'
import { DEMO_TRADE_IDS, gotoApp, graphReadout, PINNED_GRAPH } from './helpers'

// A taller viewport than the rest of the suite: the graph panel scrolls, and
// a clipped element screenshot would bake the fold — and the missing x axis —
// into the baseline.
test.use({ viewport: { width: 1440, height: 1150 } })

test('the risk chart matches its committed baseline', async ({ page }) => {
  await gotoApp(page, {
    theme: 'dark',
    selected: [...DEMO_TRADE_IDS],
    graph: PINNED_GRAPH,
  })

  const chart = page.getByTestId('risk-chart')
  await expect(chart).toBeVisible()
  // The curves are drawn from the response, so wait for the readouts it fills.
  await expect(graphReadout(page, 'max profit')).not.toHaveText('—')
  await expect(graphReadout(page, 'BE')).not.toHaveText('—')
  await expect(page.getByRole('status')).toHaveCount(0) // nothing still fetching

  await expect(chart).toHaveScreenshot('risk-chart.png')
})
