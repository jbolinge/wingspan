/** The render paths the clean demo fixtures never reach.
 *
 *  This suite talks to the second server (see `e2e/support/serve-edge.sh`),
 *  which serves a copy of `fixtures/trades/legs.csv` with three extra rows
 *  appended: a blank-price vertical ("Zeta"), a non-SPX futures symbol and an
 *  unparseable OCC code. That makes the API return the shapes the dev mock
 *  could not vary: legs with `avg_cost: null`, `sheet_leg_unpriced` and
 *  `sheet_row_skipped` warnings, and a reconciliation with nothing to show
 *  (a Legs book has no account statement to check against). */

import { expect, test } from '@playwright/test'
import { EDGE_BASE_URL } from '../playwright.config'
import type { ReconciliationResponse, TradesResponse, WarningsResponse } from '../src/api/types'
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

test('a blank-price vertical renders as legs without an average cost', async ({
  page,
  request,
}) => {
  const consoleErrors = collectConsoleErrors(page)
  const trades = (await (await request.get('/api/v1/trades')).json()) as TradesResponse
  const openCount = trades.trades.filter((trade) => trade.status === 'open').length
  const zeta = trades.trades.find((trade) => trade.id === ZETA)
  expect(zeta, 'the edge server should serve the extra unpriced trade').toBeDefined()
  expect(zeta?.legs.every((leg) => leg.avg_cost === null)).toBe(true)
  expect(zeta?.legs.every((leg) => typeof leg.mark === 'number')).toBe(true)
  const zetaContracts = new Set(zeta?.legs.map((leg) => leg.contract.tos_code) ?? [])

  await gotoApp(page, { selected: [ZETA], expanded: [ZETA] })

  await expect(tradeRows(page)).toHaveCount(openCount + 1) // + the expanded detail
  const row = tradeRow(page, 'Zeta')
  await expect(cell(row, COL.dte)).toHaveText(/^\d+$/)

  const detail = page.locator('tr', { hasText: 'Netted legs' }).last()
  const legs = detail.locator('tbody > tr')
  const legCount = zeta?.legs.length ?? 0
  await expect(legs).toHaveCount(legCount)
  for (let i = 0; i < legCount; i += 1) {
    const leg = legs.nth(i)
    // No Open Price cell for this leg, so there is no average cost to show…
    await expect(leg.getByRole('cell').nth(3)).toHaveText('—')
    // …but the leg is still priced, and says by whom.
    await expect(leg.getByRole('cell').nth(4)).toHaveText(/^\d+\.\d{2}$/)
    await expect(leg.getByRole('cell').nth(7)).toHaveText(/^\d+\.\d%$/)
    await expect(leg.getByRole('cell').nth(12)).toHaveText('fixture')
  }

  // A trade with no cost basis must not break the aggregate view or the graph.
  await expect(page.getByTestId('risk-chart')).toBeVisible()
  expect(consoleErrors).toEqual([])

  // The parser's own warning for this — one `sheet_leg_unpriced` per blank
  // leg — surfaced on Data health with the edit that would fix it.
  const { warnings } = (await (await request.get('/api/v1/warnings')).json()) as WarningsResponse
  const unpriced = warnings.filter(
    (w) => w.code === 'sheet_leg_unpriced' && w.contract !== null && zetaContracts.has(w.contract),
  )
  expect(unpriced).toHaveLength(legCount)

  await page.getByRole('button', { name: /^Data health/ }).click()
  for (const warning of unpriced) {
    const item = page.getByRole('listitem').filter({ hasText: warning.contract as string })
    await expect(item).toContainText('sheet_leg_unpriced')
    await expect(item).toContainText('Fix: The Open Price cell was blank')
  }
})

test('quote freshness is rendered per row from the real quote timestamps', async ({
  page,
  request,
}) => {
  const trades = (await (await request.get('/api/v1/trades')).json()) as TradesResponse
  const openCount = trades.trades.filter((trade) => trade.status === 'open').length

  await gotoApp(page, {})
  const dots = positionsTable(page).locator('[data-staleness]')
  await expect(dots).toHaveCount(openCount)
  // Fixture quotes are stamped `now`, so every row is fresh; the dot exists
  // and carries the level either way.
  await expect(positionsTable(page).locator('[data-staleness="fresh"]')).toHaveCount(openCount)
  await expect(dots.first()).toHaveAttribute('aria-label', /worst quote behind this row is/)
})

test('rows the parser could not use are reported as sheet_row_skipped, not a reconciliation mismatch', async ({
  page,
  request,
}) => {
  // A Legs book has no account statement to reconcile against — a row this
  // parser could not use is a `sheet_row_skipped` warning, never a
  // reconciliation row (there is no reconciliation table at all).
  const recon = (await (
    await request.get('/api/v1/reconciliation')
  ).json()) as ReconciliationResponse
  expect(recon.rows).toEqual([])
  expect(recon.source_note).not.toBeNull()

  const { warnings } = (await (await request.get('/api/v1/warnings')).json()) as WarningsResponse
  // The MES futures row and the unparseable-OCC row, at least.
  const skipped = warnings.filter((w) => w.code === 'sheet_row_skipped')
  expect(skipped.length).toBeGreaterThanOrEqual(2)
  const counts = {
    error: warnings.filter((w) => w.severity === 'error').length,
    warn: warnings.filter((w) => w.severity === 'warn').length,
    info: warnings.filter((w) => w.severity === 'info').length,
  }

  await gotoApp(page, { tab: 'health' })

  const card = page
    .locator('section')
    .filter({ has: page.getByRole('heading', { name: /^Reconciliation/ }) })
  await expect(card.getByRole('heading')).toHaveText('Reconciliation')
  await expect(card.getByText(recon.source_note as string)).toBeVisible()
  await expect(card.locator('table')).toHaveCount(0)

  // Every skipped row is reported with the edit that would fix it.
  await page.getByRole('button', { name: `warn ${counts.warn}`, exact: true }).click()
  for (const warning of skipped) {
    const item = page.getByRole('listitem').filter({ hasText: warning.message })
    await expect(item.first()).toContainText('sheet_row_skipped')
    await expect(item.first()).toContainText('Fix: A row could not be read')
  }
})
