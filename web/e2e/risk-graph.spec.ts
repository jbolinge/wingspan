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

  // log2 scale: slider value 2 is 4x.
  await zoomSlider.fill('2')
  await expect(page.getByText('4.0×')).toBeVisible()
  await expect.poll(async () => (await savedUi(page)).graph).toMatchObject({ xZoom: 4 })

  await page.getByRole('button', { name: 'reset view' }).click()
  await expect(page.getByText('1.0×')).toBeVisible()
  await expect(zoomSlider).toHaveValue('0')
  await expect
    .poll(async () => (await savedUi(page)).graph)
    .toMatchObject({ xZoom: 1, xCenter: null })
})
