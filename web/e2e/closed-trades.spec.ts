/** The closed-trades tab — LPTA and Delta 1 with their totals. Money numbers
 *  survive `sheet_parity.rs`'s guarantee that the folder and Legs paths agree
 *  on `realized`/`cost_basis`; the folder book's LPTA quarantine (six
 *  duplicate order claims) has no equivalent here — a Legs CSV never carries
 *  orders to quarantine. */

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

  // Realised P&L, fees and net are fully determined by the sheet's own
  // prices — no marks, so these are exact.
  const lpta = closedRow(page, 'LPTA')
  await expect(lpta.getByRole('cell').nth(2)).toHaveText('Jul 24 2026')
  await expect(lpta.getByRole('cell').nth(3)).toHaveText('Jul 29 2026')
  await expect(lpta.getByRole('cell').nth(6)).toHaveText('-$160.00')
  await expect(lpta.getByRole('cell').nth(7)).toHaveText('$2.35')
  await expect(lpta.getByRole('cell').nth(8)).toHaveText('-$162.35')
  // The badge counts warnings plus quarantined orders not already covered by
  // a duplicate_execution_claim warning (lib/aggregate) — a Legs CSV has no
  // orders to quarantine, so this is computed from the API rather than
  // assumed, and holds whatever the Legs path produces (typically 0, so no
  // badge at all — see `ClosedTrades.tsx`, which renders nothing for a zero
  // count).
  const lptaApi = closed.trades.find((trade) => trade.name === 'LPTA')
  const lptaBadge =
    (lptaApi?.warnings.length ?? 0) +
    Math.max(
      0,
      (lptaApi?.quarantined.length ?? 0) -
        (lptaApi?.warnings.filter((w) => w.code === 'duplicate_execution_claim').length ?? 0),
    )
  await expect(lpta.getByRole('cell').nth(9)).toHaveText(lptaBadge > 0 ? `⚠ ${lptaBadge}` : '')

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
  // `realised` is the pre-fee bank (-160.00 + 140.00), `net` is after fees —
  // the same three columns, in the same order, as the table below.
  await expect(page.getByText('realised -$20.00')).toBeVisible()
  await expect(page.getByText('net -$27.15')).toBeVisible()
  expect(closed.totals).toMatchObject({
    count: 2,
    win_rate: 0.5,
    avg_days_held: 4,
    realized: -20,
    fees: 7.15,
    pnl_total: -27.15,
  })
})

test('LPTA has no quarantined orders under the Legs source', async ({ page, request }) => {
  // A Legs CSV carries no orders at all (`Trade.orders` is always empty for
  // one), so there is nothing for an OTO ladder to collide with — unlike a
  // folder book, where LPTA's statement replay quarantines duplicate claims.
  const closed = (await (await request.get('/api/v1/trades/closed')).json()) as ClosedTradesResponse
  const lptaApi = closed.trades.find((trade) => trade.name === 'LPTA')
  expect(lptaApi?.quarantined).toHaveLength(0)

  await gotoApp(page, { tab: 'closed' })
  await page.getByRole('button', { name: 'Expand LPTA', exact: true }).click()

  const detail = page.locator('tr', { hasText: 'Netted legs' }).last()
  // No quarantined block at all — `TradeDetail` renders nothing for it when
  // `trade.quarantined` is empty.
  await expect(detail.getByText('quarantined', { exact: true })).toHaveCount(0)
  await expect(detail.locator('code')).toHaveCount(0)

  // The one leg nets flat, closed: no open legs remain either way.
  await expect(detail.getByText('No open legs', { exact: false })).toBeVisible()
})
