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

/** The committed baseline's height in CSS pixels — what the chart panel is
 *  pinned to before the capture (see the comment at the capture). Re-taking
 *  the baseline with a different panel height means updating this too. */
const BASELINE_HEIGHT = 515

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

  // The chart fills a flex panel whose height is whatever the text above it
  // leaves over, and that differs by a pixel between Chromium builds (CI's
  // rendered 516 px against a 515 px baseline). A screenshot comparison needs
  // identical dimensions before it can tolerate anything, so the panel is
  // pinned to the baseline's height and the canvas is given a beat to follow
  // through the chart's ResizeObserver.
  await chart.evaluate((el, height) => {
    const host = el.parentElement as HTMLElement
    host.style.height = `${height}px`
    host.style.minHeight = `${height}px`
    host.style.maxHeight = `${height}px`
    host.style.flex = 'none'
  }, BASELINE_HEIGHT)
  await expect
    .poll(() => chart.evaluate((el) => el.querySelector('canvas')?.clientHeight ?? 0))
    .toBe(BASELINE_HEIGHT)
  // Same story for the panel's *position*: the text above it leaves the chart
  // at a fractional offset, and an element screenshot spans every pixel row
  // it touches — a whole-pixel height starting half-way down a row still
  // captures one row too many. Snap the panel to the pixel grid with a
  // relative offset, which moves nothing else on the page.
  await chart.evaluate((el) => {
    const host = el.parentElement as HTMLElement
    const rect = host.getBoundingClientRect()
    host.style.top = `${Math.floor(rect.top) - rect.top}px`
    host.style.left = `${Math.floor(rect.left) - rect.left}px`
  })
  await expect
    .poll(async () => {
      const box = await chart.boundingBox()
      return box ? [Number.isInteger(box.y), Math.round(box.height)] : null
    })
    .toEqual([true, BASELINE_HEIGHT])

  await expect(chart).toHaveScreenshot('risk-chart.png')
})
