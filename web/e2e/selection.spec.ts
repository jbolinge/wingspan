/** Toggling a trade changes the cards and the graph. The analysis set is the
 *  whole point of the dashboard, so this drives it through the checkboxes and
 *  checks both consumers: the Selected portfolio card and the risk graph. */

import { expect, test } from '@playwright/test'
import type { PortfolioStats, RiskGraphResponse } from '../src/api/types'
import {
  expectNumberNear,
  gotoApp,
  graphReadout,
  positionsTable,
  sectionByHeading,
  statValue,
} from './helpers'

const M1 = 'm1-2026-07-14'
const M4 = 'm4-2026-08-04'

async function portfolio(
  request: import('@playwright/test').APIRequestContext,
  ids: string[],
): Promise<PortfolioStats> {
  const response = await request.get(`/api/v1/portfolio?ids=${ids.join(',')}`)
  expect(response.ok()).toBeTruthy()
  return (await response.json()) as PortfolioStats
}

async function riskGraph(
  request: import('@playwright/test').APIRequestContext,
  ids: string[],
): Promise<RiskGraphResponse> {
  const response = await request.post('/api/v1/risk-graph', {
    data: {
      trade_ids: ids,
      price_range: null,
      points: 241,
      dates: null,
      vol_shift: 0,
      vol_shift_by_expiry: {},
      spot_override: null,
      include_realized: true,
    },
  })
  expect(response.ok()).toBeTruthy()
  return (await response.json()) as RiskGraphResponse
}

test('toggling a trade into the analysis set moves the cards and the graph', async ({
  page,
  request,
}) => {
  await gotoApp(page, { selected: [M1] })

  const selectedCard = sectionByHeading(page, 'Selected')
  const chart = page.getByTestId('risk-chart')

  // --- one trade selected -------------------------------------------------
  const one = await portfolio(request, [M1])
  const graphOne = await riskGraph(request, [M1])
  await expect(chart).toBeVisible()
  await expect(chart).toHaveAttribute(
    'aria-label',
    'Risk graph for 1 selected trades: expiration line plus 5 T+x lines',
  )
  await expectNumberNear(statValue(selectedCard, 'P&L total'), one.pnl_total, 5, 'card P&L total')
  await expectNumberNear(statValue(selectedCard, 'Δ'), one.greeks.delta, 0.5, 'card delta')
  await expect(statValue(selectedCard, 'Trades')).toHaveText('1')
  await expectNumberNear(
    graphReadout(page, 'max profit'),
    graphOne.max_profit_on_range,
    25,
    'graph max profit',
  )

  const beforePixels = await chart.screenshot()

  // --- add a second trade -------------------------------------------------
  await positionsTable(page).getByRole('checkbox', { name: 'Select M4', exact: true }).check()
  await expect(page.getByText('2 of 6 selected')).toBeVisible()

  const two = await portfolio(request, [M1, M4])
  const graphTwo = await riskGraph(request, [M1, M4])
  expect(two.pnl_total).not.toBeCloseTo(one.pnl_total, 0)

  await expect(statValue(selectedCard, 'Trades')).toHaveText('2')
  await expectNumberNear(statValue(selectedCard, 'P&L total'), two.pnl_total, 5, 'card P&L total')
  await expectNumberNear(statValue(selectedCard, 'Δ'), two.greeks.delta, 0.5, 'card delta')
  await expectNumberNear(statValue(selectedCard, 'Θ'), two.greeks.theta, 1, 'card theta')

  // The graph is a canvas: assert both its data-derived readouts and that the
  // pixels actually changed.
  await expect(chart).toHaveAttribute(
    'aria-label',
    'Risk graph for 2 selected trades: expiration line plus 5 T+x lines',
  )
  await expectNumberNear(
    graphReadout(page, 'max profit'),
    graphTwo.max_profit_on_range,
    25,
    'graph max profit',
  )
  await expectNumberNear(
    graphReadout(page, 'max loss'),
    graphTwo.max_loss_on_range,
    25,
    'graph max loss',
  )
  await expect
    .poll(async () => (await chart.screenshot()).equals(beforePixels), {
      message: 'the risk chart should repaint when the analysis set changes',
    })
    .toBe(false)

  // --- and back out again -------------------------------------------------
  await positionsTable(page).getByRole('checkbox', { name: 'Select M1', exact: true }).uncheck()
  await expect(page.getByText('1 of 6 selected')).toBeVisible()
  const four = await portfolio(request, [M4])
  await expectNumberNear(statValue(selectedCard, 'P&L total'), four.pnl_total, 5, 'card P&L total')
  await expect(chart).toHaveAttribute(
    'aria-label',
    'Risk graph for 1 selected trades: expiration line plus 5 T+x lines',
  )
})

test('“all”, “none” and “invert” drive the same analysis set', async ({ page, request }) => {
  await gotoApp(page, { selected: [] })
  const selectedCard = sectionByHeading(page, 'Selected')
  await expect(selectedCard.getByText('Nothing selected', { exact: false })).toBeVisible()
  await expect(page.getByTestId('risk-chart')).toHaveCount(0)

  await page.getByRole('button', { name: 'all', exact: true }).click()
  await expect(page.getByText('6 of 6 selected')).toBeVisible()
  const all = await portfolio(request, [
    'bsh-2026-07-14',
    'm1-2026-07-14',
    'm2-2026-07-21',
    'm3-2026-07-28',
    'm4-2026-08-04',
    'm5-2026-08-15',
  ])
  await expectNumberNear(statValue(selectedCard, 'P&L total'), all.pnl_total, 10, 'all P&L total')
  // "All open trades" is the server's own aggregate; the two must agree.
  const allCard = sectionByHeading(page, 'All open trades')
  await expectNumberNear(statValue(allCard, 'P&L total'), all.pnl_total, 10, 'all-card P&L total')

  await page.getByRole('button', { name: 'invert', exact: true }).click()
  await expect(page.getByText('0 of 6 selected')).toBeVisible()
  await page.getByRole('button', { name: 'invert', exact: true }).click()
  await expect(page.getByText('6 of 6 selected')).toBeVisible()
  await page.getByRole('button', { name: 'none', exact: true }).click()
  await expect(page.getByText('0 of 6 selected')).toBeVisible()
})
