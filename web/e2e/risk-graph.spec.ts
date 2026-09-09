/** The expiration line plus five T+x lines by default, and a what-if
 *  panel that recomputes the greeks (not just the chart). */

import { expect, test } from '@playwright/test'
import type { RiskGraphResponse, WhatIfResponse } from '../src/api/types'
import {
  collectConsoleErrors,
  DEMO_TRADE_IDS,
  expectNumberNear,
  gotoApp,
  graphReadout,
  PINNED_GRAPH,
  savedUi,
  statValue,
} from './helpers'

const ALL = [...DEMO_TRADE_IDS]

test('the default ladder is the expiration line plus five T+x lines', async ({ page, request }) => {
  const response = await request.post('/api/v1/risk-graph', {
    data: {
      trade_ids: ALL,
      price_range: null,
      points: 241,
      dates: null,
      vol_shift: 0,
      vol_shift_by_expiry: {},
      spot_override: null,
      include_realized: true,
    },
  })
  const graph = (await response.json()) as RiskGraphResponse
  expect(graph.lines.filter((line) => line.kind === 'expiry')).toHaveLength(1)
  expect(graph.lines.filter((line) => line.kind !== 'expiry')).toHaveLength(5)

  await gotoApp(page, { selected: ALL })

  const chart = page.getByTestId('risk-chart')
  await expect(chart).toHaveAttribute(
    'aria-label',
    'Risk graph for 6 selected trades: expiration line plus 5 T+x lines',
  )

  // The Lines section lists one toggle per curve, all on by default.
  const lineToggles = page.getByRole('checkbox', { name: /^Show / })
  await expect(lineToggles).toHaveCount(6)
  await expect(page.getByRole('checkbox', { name: 'Show T+0', exact: true })).toBeChecked()
  await expect(page.getByRole('checkbox', { name: /^Show EXP / })).toHaveCount(1)
  await expect(page.getByRole('checkbox', { name: /^Show T\+/ })).toHaveCount(5)
  for (let i = 0; i < 6; i += 1) await expect(lineToggles.nth(i)).toBeChecked()

  // Switching one off is reflected in the chart's own description.
  await page.getByRole('checkbox', { name: 'Show T+0', exact: true }).uncheck()
  await expect(chart).toHaveAttribute(
    'aria-label',
    'Risk graph for 6 selected trades: expiration line plus 4 T+x lines',
  )

  // Breakevens and the probability readouts come from the same response.
  await expectNumberNear(graphReadout(page, 'PoP'), graph.pop * 100, 1, 'PoP')
  await expectNumberNear(graphReadout(page, 'ATM IV'), graph.front_atm_iv * 100, 1, 'ATM IV')
})

test('the what-if panel recomputes P&L and the greeks readout', async ({ page, request }) => {
  const consoleErrors = collectConsoleErrors(page)
  await gotoApp(page, {
    selected: ALL,
    // The overlay draws one curve per selected trade; it also used to draw one
    // per ladder date, which React reported as duplicate keys.
    graph: { perTradeOverlay: true, whatIf: { enabled: true, price: null, date: null, volShift: 0 } },
  })

  const whatIf = page.locator('section').filter({ hasText: 'What-if' }).last()
  await expect(whatIf.getByRole('button', { name: /^what-if/ })).toHaveText('what-if on')

  const priceField = whatIf.getByLabel('What-if price value')
  await expect(priceField).toHaveValue('7750')

  const atSpot = await whatIfApi(request, 7750, 0)
  await expectNumberNear(statValue(whatIf, 'P&L'), atSpot.pnl, 25, 'what-if P&L at spot')
  await expectNumberNear(statValue(whatIf, 'Δ'), atSpot.greeks.delta, 0.5, 'what-if delta at spot')

  // Move the price 500 points down: every greek must follow.
  await priceField.fill('7250')
  await priceField.blur()
  const down = await whatIfApi(request, 7250, 0)
  expect(down.greeks.delta).not.toBeCloseTo(atSpot.greeks.delta, 1)
  await expectNumberNear(statValue(whatIf, 'P&L'), down.pnl, 25, 'what-if P&L at 7250')
  await expectNumberNear(statValue(whatIf, 'Δ'), down.greeks.delta, 0.5, 'what-if delta at 7250')
  await expectNumberNear(statValue(whatIf, 'Γ'), down.greeks.gamma, 0.05, 'what-if gamma at 7250')
  await expectNumberNear(statValue(whatIf, 'Θ'), down.greeks.theta, 1, 'what-if theta at 7250')
  await expectNumberNear(statValue(whatIf, 'V'), down.greeks.vega, 10, 'what-if vega at 7250')

  // …and so must a vol shift.
  await whatIf.getByLabel('What-if vol shift').fill('0.05')
  const shifted = await whatIfApi(request, 7250, 0.05)
  expect(shifted.pnl).not.toBeCloseTo(down.pnl, 0)
  await expectNumberNear(statValue(whatIf, 'P&L'), shifted.pnl, 25, 'what-if P&L at +5 vp')
  await expectNumberNear(statValue(whatIf, 'V'), shifted.greeks.vega, 10, 'what-if vega at +5 vp')

  expect(consoleErrors).toEqual([])
})

async function whatIfApi(
  request: import('@playwright/test').APIRequestContext,
  price: number,
  volShift: number,
): Promise<WhatIfResponse> {
  const today = new Date()
  const date = new Date(
    Date.UTC(today.getFullYear(), today.getMonth(), today.getDate()),
  )
    .toISOString()
    .slice(0, 10)
  const response = await request.post('/api/v1/what-if', {
    data: { trade_ids: ALL, price, date, vol_shift: volShift },
  })
  expect(response.ok()).toBeTruthy()
  return (await response.json()) as WhatIfResponse
}

test('the per-trade overlay draws one curve per selected trade, not one per date', async ({
  page,
}) => {
  await gotoApp(page, { selected: ALL, graph: { perTradeOverlay: true } })
  await expect(page.getByTestId('risk-chart')).toBeVisible()

  const linesSection = page
    .locator('section')
    .filter({ has: page.getByRole('heading', { name: 'Lines', exact: true }) })
  // `/risk-graph` returns trades × dates curves; the overlay legend must list
  // each trade once (it listed each of them six times before).
  const legend = linesSection.locator('ul').last().locator('li')
  await expect(legend).toHaveText(['BSH', 'M1', 'M2', 'M3', 'M4', 'M5'])
})

test('the zoom slider narrows the chart, persists, and reset view puts it back', async ({
  page,
}) => {
  await gotoApp(page, { selected: ALL, graph: PINNED_GRAPH })
  await expect(page.getByTestId('risk-chart')).toBeVisible()
  await expect(graphReadout(page, 'max profit')).not.toHaveText('—')

  const zoomSlider = page.getByLabel('zoom', { exact: true })
  await expect(zoomSlider).toHaveValue('0')

  // log2 scale: slider value 2 is 400%.
  await zoomSlider.fill('2')
  await expect(page.getByText('400%')).toBeVisible()
  await expect.poll(async () => (await savedUi(page)).graph).toMatchObject({ xZoom: 4 })

  // Both ends of the track are reachable, and read exactly.
  await zoomSlider.fill('-2')
  await expect(page.getByText('25%')).toBeVisible()
  // 500% (log2 5 = 2.3219…) is off the 1/20-octave step grid, so the track
  // ends on the last step it does have and that step *means* 500%. Driven
  // with the keyboard, the way a trader reaches the end of a slider, the
  // thumb lands hard right and the readout says so — it used to sanitise back
  // a step, where `End` fired nothing and 500% could not be reached at all.
  await zoomSlider.focus()
  await zoomSlider.press('End')
  await expect(page.getByText('500%')).toBeVisible()
  await expect.poll(async () => (await savedUi(page)).graph).toMatchObject({ xZoom: 5 })

  // Sliding back to 100% is the default view itself, not a centred window
  // that happens to look like it: `reset view` goes dark with it.
  await zoomSlider.fill('0')
  await expect(page.getByText('100%')).toBeVisible()
  await expect
    .poll(async () => (await savedUi(page)).graph)
    .toMatchObject({ xZoom: 1, xCenter: null })
  await expect(page.getByRole('button', { name: 'reset view' })).toBeDisabled()

  await zoomSlider.fill('2')
  await expect.poll(async () => (await savedUi(page)).graph).toMatchObject({ xZoom: 4 })
  await page.getByRole('button', { name: 'reset view' }).click()
  await expect(page.getByText('100%')).toBeVisible()
  await expect(zoomSlider).toHaveValue('0')
  await expect
    .poll(async () => (await savedUi(page)).graph)
    .toMatchObject({ xZoom: 1, xCenter: null })
})

/** The two gestures the slider stands in for. Both are pure client-side
 *  arithmetic over the arrays the server already sent. */
test('the wheel zooms about the cursor, the drag pans, and a double-click resets', async ({
  page,
}) => {
  const savedGraph = async (): Promise<{ xZoom: number; xCenter: number | null }> =>
    (await savedUi(page)).graph as { xZoom: number; xCenter: number | null }

  await gotoApp(page, { selected: ALL, graph: PINNED_GRAPH })
  const chart = page.getByTestId('risk-chart')
  await expect(chart).toBeVisible()
  await expect(graphReadout(page, 'max profit')).not.toHaveText('—')

  // The zoom row is the way back from any of these gestures: it must be on
  // screen without scrolling, because the wheel over the chart is a zoom.
  await expect(page.getByRole('button', { name: 'reset view' })).toBeInViewport()

  const box = (await chart.boundingBox())!
  const midX = box.x + box.width / 2
  const midY = box.y + box.height / 2

  await page.mouse.move(midX, midY)
  await page.mouse.wheel(0, -300)
  await expect.poll(async () => (await savedGraph()).xZoom).toBeGreaterThan(1)
  await expect(chart).toHaveCSS('cursor', 'grab')
  const zoomed = await savedGraph()

  // Dragging right pulls the curves right, so the window travels left.
  await page.mouse.move(midX, midY)
  await page.mouse.down()
  await page.mouse.move(midX + 60, midY, { steps: 6 })
  await page.mouse.move(midX + 120, midY, { steps: 6 })
  await page.mouse.up()
  await expect.poll(async () => (await savedGraph()).xCenter).toBeLessThan(zoomed.xCenter!)
  // A pan is not a re-query: the zoom the wheel set is still what is drawn.
  expect((await savedGraph()).xZoom).toBe(zoomed.xZoom)

  await chart.dblclick({ position: { x: box.width / 2, y: box.height / 2 } })
  await expect.poll(savedGraph).toMatchObject({ xZoom: 1, xCenter: null })
  await expect(page.getByText('100%')).toBeVisible()
})

/** Panning is not clamped to the computed range any more: dragging far enough
 *  moves the window — and the request behind it — past what the range mode
 *  asked the server for in the first place. */
test('a pan can move the window past the computed range', async ({ page }) => {
  const savedGraph = async (): Promise<{ xZoom: number; xCenter: number | null }> =>
    (await savedUi(page)).graph as { xZoom: number; xCenter: number | null }

  await gotoApp(page, { selected: ALL, graph: PINNED_GRAPH })
  const chart = page.getByTestId('risk-chart')
  await expect(chart).toBeVisible()
  await expect(graphReadout(page, 'max profit')).not.toHaveText('—')

  // 50% of the pinned 6000-9000 range: a 6000-wide window, twice as wide as
  // what was computed.
  await page.getByLabel('zoom', { exact: true }).fill(String(Math.log2(0.5)))
  await expect(page.getByText('50%')).toBeVisible()
  const zoomed = await savedGraph()
  expect(zoomed.xCenter).not.toBeNull()

  const box = (await chart.boundingBox())!
  const midX = box.x + box.width / 2
  const midY = box.y + box.height / 2

  // Dragging left pulls the curves left, so the window — and its centre —
  // travels right, well past the computed range's top end at 9000.
  await page.mouse.move(midX, midY)
  await page.mouse.down()
  await page.mouse.move(midX - 150, midY, { steps: 8 })
  await page.mouse.move(midX - 300, midY, { steps: 8 })
  await page.mouse.move(midX - 450, midY, { steps: 8 })
  await page.mouse.up()

  const panned = await savedGraph()
  expect(panned.xCenter).not.toBe(zoomed.xCenter)
  expect(panned.xCenter!).toBeGreaterThan(9000)
})

/** The chart covers nearly the whole scrolling graph panel; a wheel it cannot
 *  act on — one at either end of the zoom — has to reach the scroller
 *  underneath. */
test('a wheel over the chart still scrolls the panel at the end of the zoom', async ({
  page,
}) => {
  await gotoApp(page, {
    selected: ALL,
    graph: { ...PINNED_GRAPH, showSliceTable: true },
  })
  const chart = page.getByTestId('risk-chart')
  await expect(chart).toBeVisible()
  await expect(graphReadout(page, 'max profit')).not.toHaveText('—')

  // All the way out, where a wheel *down* has nothing left to zoom out to.
  await page.getByLabel('zoom', { exact: true }).fill('-2')
  await expect(page.getByText('25%')).toBeVisible()

  const scrollTop = async (): Promise<number> =>
    chart.evaluate((el) => el.closest('.overflow-y-auto')?.scrollTop ?? -1)
  expect(await scrollTop()).toBe(0)

  const box = (await chart.boundingBox())!
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2)
  await page.mouse.wheel(0, 300)

  await expect.poll(scrollTop).toBeGreaterThan(0)
  // The gesture scrolled; it did not also zoom.
  await expect(page.getByText('25%')).toBeVisible()
})
