/** The render paths the clean demo fixtures never reach.
 *
 *  This suite talks to the second server (see `e2e/support/serve-edge.sh`),
 *  which serves the same fixtures plus one open trade whose order appears in
 *  no statement. That single addition makes the API return the shapes the dev
 *  mock could not vary: legs with `avg_cost: null`, an `unmatched_order`
 *  warning, and reconciliation rows that do not balance. */

import { expect, test } from '@playwright/test'
import { EDGE_BASE_URL } from '../playwright.config'
import type { ReconciliationResponse, TradesResponse } from '../src/api/types'
import {
  cell,
  COL,
  collectConsoleErrors,
  gotoApp,
  positionsTable,
  tradeRow,
  tradeRows,
} from './helpers'

test.use({ baseURL: EDGE_BASE_URL })

const ZETA = 'zeta-2026-08-20'

test('an unmatched order renders as legs without an average cost', async ({ page, request }) => {
  const consoleErrors = collectConsoleErrors(page)
  const trades = (await (await request.get('/api/v1/trades')).json()) as TradesResponse
  const zeta = trades.trades.find((trade) => trade.id === ZETA)
  expect(zeta, 'the edge server should serve the extra unmatched trade').toBeDefined()
  expect(zeta?.legs.every((leg) => leg.avg_cost === null)).toBe(true)
  expect(zeta?.legs.every((leg) => typeof leg.mark === 'number')).toBe(true)

  await gotoApp(page, { selected: [ZETA], expanded: [ZETA] })

  await expect(tradeRows(page)).toHaveCount(8) // 7 trades + the expanded detail
  const row = tradeRow(page, 'Zeta')
  await expect(cell(row, COL.dte)).toHaveText(/^\d+$/)
  // One warning, no quarantine.
  await expect(cell(row, COL.warnings)).toHaveText('⚠ 1')

  const detail = page.locator('tr', { hasText: 'Netted legs' }).last()
  const legs = detail.locator('tbody > tr')
  await expect(legs).toHaveCount(2)
  for (let i = 0; i < 2; i += 1) {
    const leg = legs.nth(i)
    // No execution behind the order, so there is no average cost to show…
    await expect(leg.getByRole('cell').nth(3)).toHaveText('—')
    // …but the leg is still priced, and says by whom.
    await expect(leg.getByRole('cell').nth(4)).toHaveText(/^\d+\.\d{2}$/)
    await expect(leg.getByRole('cell').nth(7)).toHaveText(/^\d+\.\d%$/)
    await expect(leg.getByRole('cell').nth(12)).toHaveText('fixture')
  }

  // The warning is on the trade, with the message the server wrote.
  await expect(detail.getByText('unmatched_order')).toBeVisible()
  await expect(detail.getByText(/no statement execution matches this order/)).toBeVisible()

  // A trade with no cost basis must not break the aggregate view or the graph.
  await expect(page.getByTestId('risk-chart')).toBeVisible()
  expect(consoleErrors).toEqual([])
})

test('quote freshness is rendered per row from the real quote timestamps', async ({ page }) => {
  await gotoApp(page, {})
  const dots = positionsTable(page).locator('[data-staleness]')
  await expect(dots).toHaveCount(7)
  // Fixture quotes are stamped `now`, so every row is fresh; the dot exists
  // and carries the level either way.
  await expect(positionsTable(page).locator('[data-staleness="fresh"]')).toHaveCount(7)
  await expect(dots.first()).toHaveAttribute('aria-label', /worst quote behind this row is/)
})

test('reconciliation reports the contracts no statement backs', async ({ page, request }) => {
  const recon = (await (
    await request.get('/api/v1/reconciliation')
  ).json()) as ReconciliationResponse
  const missing = recon.rows.filter((row) => row.status === 'missing')
  expect(missing).toHaveLength(2)

  await gotoApp(page, { tab: 'health' })

  const card = page
    .locator('section')
    .filter({ has: page.getByRole('heading', { name: /^Reconciliation/ }) })
  await expect(card.getByText(`23/${recon.rows.length} match`)).toBeVisible()
  // "problems only" is the default, and now there are problems.
  await expect(card.locator('tbody > tr')).toHaveCount(2)
  for (const row of missing) {
    const line = card.locator('tbody > tr').filter({ hasText: row.tos_code })
    await expect(line.getByRole('cell').nth(1)).toHaveText('0')
    await expect(line.getByRole('cell').nth(2)).toHaveText(String(row.trades_qty))
    await expect(line.getByRole('cell').nth(3)).toHaveText(String(-row.trades_qty))
    await expect(line.getByText('missing')).toBeVisible()
  }

  // The unmatched order is reported with the edit that would fix it.
  await expect(page.getByRole('button', { name: 'warn 7', exact: true })).toBeVisible()
  const item = page.getByRole('listitem').filter({ hasText: 'unmatched_order' })
  await expect(item).toHaveCount(1)
  await expect(item.getByText(ZETA, { exact: true })).toBeVisible()
  await expect(item.getByText(/Fix: No execution matched this order/)).toBeVisible()
})
