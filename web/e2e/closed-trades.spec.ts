/** Plan §7: the closed-trades tab — LPTA and Delta 1 with their totals, and
 *  LPTA's six quarantined orders with the fix hint that resolves them. */

import { expect, test } from '@playwright/test'
import type { ClosedTradesResponse } from '../src/api/types'
import { gotoApp } from './helpers'

function closedRow(page: import('@playwright/test').Page, name: string) {
  return page
    .getByRole('table')
    .first()
    .getByRole('row')
    .filter({ has: page.getByRole('button', { name: `Expand ${name}`, exact: true }) })
}

test('the closed tab shows LPTA and Delta 1 with the API’s totals', async ({ page, request }) => {
  const closed = (await (await request.get('/api/v1/trades/closed')).json()) as ClosedTradesResponse
  expect(closed.trades.map((trade) => trade.name).sort()).toEqual(['Delta 1', 'LPTA'])

  await gotoApp(page, { tab: 'closed' })

  const table = page.getByRole('table').first()
  await expect(table.locator('tbody > tr')).toHaveCount(2)

  // Realised P&L, fees and net are fully determined by the statements — no
  // marks, so these are exact.
  const lpta = closedRow(page, 'LPTA')
  await expect(lpta.getByRole('cell').nth(2)).toHaveText('Jul 24 2026')
  await expect(lpta.getByRole('cell').nth(3)).toHaveText('Jul 29 2026')
  await expect(lpta.getByRole('cell').nth(6)).toHaveText('-$160.00')
  await expect(lpta.getByRole('cell').nth(7)).toHaveText('$2.35')
  await expect(lpta.getByRole('cell').nth(8)).toHaveText('-$162.35')
  // The badge counts warnings *and* quarantined orders (lib/aggregate), and
  // the server raises one warning per quarantined order, so six duplicated
  // orders read as twelve here while the Data-health tab counts them once.
  const lptaApi = closed.trades.find((trade) => trade.name === 'LPTA')
  expect(lptaApi?.warnings).toHaveLength(6)
  expect(lptaApi?.quarantined).toHaveLength(6)
  await expect(lpta.getByRole('cell').nth(9)).toHaveText(
    `⚠ ${(lptaApi?.warnings.length ?? 0) + (lptaApi?.quarantined.length ?? 0)}`,
  )

  const delta1 = closedRow(page, 'Delta 1')
  await expect(delta1.getByRole('cell').nth(2)).toHaveText('Aug 25 2026')
  await expect(delta1.getByRole('cell').nth(3)).toHaveText('Aug 28 2026')
  await expect(delta1.getByRole('cell').nth(6)).toHaveText('+$140.00')
  await expect(delta1.getByRole('cell').nth(7)).toHaveText('$4.80')
  await expect(delta1.getByRole('cell').nth(8)).toHaveText('+$135.20')

  // The running totals in the card header.
  await expect(page.getByText('count 2')).toBeVisible()
  await expect(page.getByText('win rate 50%')).toBeVisible()
  await expect(page.getByText('avg held 4d')).toBeVisible()
  await expect(page.getByText('fees $7.15')).toBeVisible()
  await expect(page.getByText('realised -$27.15')).toBeVisible()
  expect(closed.totals).toMatchObject({ count: 2, win_rate: 0.5, avg_days_held: 4, fees: 7.15 })
})

test('LPTA’s six duplicate orders are shown quarantined, with the fix', async ({
  page,
  request,
}) => {
  const closed = (await (await request.get('/api/v1/trades/closed')).json()) as ClosedTradesResponse
  const lptaApi = closed.trades.find((trade) => trade.name === 'LPTA')
  expect(lptaApi?.quarantined).toHaveLength(6)

  await gotoApp(page, { tab: 'closed' })
  await page.getByRole('button', { name: 'Expand LPTA', exact: true }).click()

  const detail = page.locator('tr', { hasText: 'Netted legs' }).last()
  await expect(detail.getByText('quarantined', { exact: true })).toBeVisible()
  await expect(detail.getByText('6 orders excluded')).toBeVisible()

  // Every quarantined order shows its raw line and who claimed the execution.
  const orders = detail.locator('code')
  await expect(orders).toHaveCount(6)
  for (const order of lptaApi?.quarantined ?? []) {
    await expect(detail.getByText(order.raw, { exact: true })).toBeVisible()
  }
  await expect(detail.getByText(/already claimed by m1-2026-07-14/).first()).toBeVisible()
  await expect(detail.getByText(/# ref=1007170257443/).first()).toBeVisible()

  // The round trip that did happen leaves no open legs.
  await expect(detail.getByText('No open legs', { exact: false })).toBeVisible()
})
