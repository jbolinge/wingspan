/** Plan §7: “six open trades with DIT/DTE/P&L/greeks; banner shows the
 *  fixture source”. Every expected value comes from the running server, so
 *  this fails if the SPA renders a different book than the API serves. */

import { expect, test } from '@playwright/test'
import type { TradesResponse } from '../src/api/types'
import {
  cell,
  COL,
  collectConsoleErrors,
  DEMO_TRADE_IDS,
  expectNumberNear,
  gotoApp,
  positionsTable,
  tradeRow,
  tradeRows,
} from './helpers'

test.describe('dashboard', () => {
  test('lists the six open trades with the API’s DIT, DTE, P&L and greeks', async ({
    page,
    request,
  }) => {
    const consoleErrors = collectConsoleErrors(page)
    const trades = (await (await request.get('/api/v1/trades')).json()) as TradesResponse
    expect(trades.trades.map((trade) => trade.id)).toEqual([...DEMO_TRADE_IDS])

    await gotoApp(page)

    const table = positionsTable(page)
    await expect(tradeRows(page)).toHaveCount(6)

    for (const trade of trades.trades) {
      const row = tradeRow(page, trade.name)
      await expect(row).toBeVisible()
      // Whole-day counters are exact; anything priced moves with the clock.
      await expect(cell(row, COL.dit)).toHaveText(String(trade.dit))
      await expect(cell(row, COL.dte)).toHaveText(String(trade.dte))
      await expect(cell(row, COL.structure)).toHaveText(trade.structure)
      await expectNumberNear(cell(row, COL.pnlTotal), trade.pnl_total, 5, `${trade.name} P&L total`)
      await expectNumberNear(cell(row, COL.pnlOpen), trade.pnl_open, 5, `${trade.name} P&L open`)
      await expectNumberNear(cell(row, COL.delta), trade.greeks.delta, 0.5, `${trade.name} delta`)
      await expectNumberNear(cell(row, COL.theta), trade.greeks.theta, 1, `${trade.name} theta`)
      await expectNumberNear(cell(row, COL.vega), trade.greeks.vega, 5, `${trade.name} vega`)
      await expectNumberNear(
        cell(row, COL.weightedVega),
        trade.weighted_vega,
        5,
        `${trade.name} weighted vega`,
      )
      // Gamma is small but must still be a number, not an em dash.
      await expect(cell(row, COL.gamma)).toHaveText(/^-?\d/)
    }

    // Quote freshness is rendered per row; the fixture source is always fresh.
    await expect(table.locator('[data-staleness]')).toHaveCount(6)
    await expect(table.locator('[data-staleness="fresh"]')).toHaveCount(6)

    expect(consoleErrors).toEqual([])
  })

  test('the status banner names the demo fixture source, not a live provider', async ({
    page,
    request,
  }) => {
    const snapshot = await (await request.get('/api/v1/market/snapshot')).json()
    expect(snapshot.spot_source).toBe('fixture')

    await gotoApp(page)

    await expect(page.getByText('DEMO · fixture')).toBeVisible()
    await expect(
      page.getByText('fixture: synthetic fixture prices (demo mode), not live market data'),
    ).toBeVisible()
    await expect(page.getByText('SPX')).toBeVisible()
    await expect(page.getByText('7,750.00')).toBeVisible()

    // The dev-only mock layer must never latch in the embedded bundle.
    await expect(page.getByText(/^mock/)).toHaveCount(0)
    await expect(page.getByRole('img', { name: 'live' })).toBeVisible()
  })

  test('the weighted-vega reference in play is stated next to the numbers', async ({
    page,
    request,
  }) => {
    const trades = (await (await request.get('/api/v1/trades')).json()) as TradesResponse
    await gotoApp(page)
    await expect(
      page.getByText(`W.V reference: ${trades.portfolio_all.weighted_vega_reference}`),
    ).toBeVisible()
  })
})
