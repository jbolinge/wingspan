/** The book a new user sees: plain `wingspan-devserver serve --demo` on :8791 serves the
 *  wholly synthetic `fixtures/demo` (scripts/make_demo_book.py) — a strangle,
 *  an iron condor and one closed put spread, and nothing from the anonymised
 *  account data the rest of the suite runs against on :8787. */

import { expect, test } from '@playwright/test'
import type { ClosedTradesResponse, TradesResponse, WarningsResponse } from '../src/api/types'
import { DEMO_BASE_URL } from '../playwright.config'
import {
  cell,
  COL,
  collectConsoleErrors,
  DEMO_BOOK_TRADE_IDS,
  DEMO_TRADE_IDS,
  gotoApp,
  tradeRow,
  tradeRows,
} from './helpers'

test.use({ baseURL: DEMO_BASE_URL })

test('the demo serves the synthetic strangle, condor and XSP hedge, not the account fixtures', async ({
  page,
  request,
}) => {
  const consoleErrors = collectConsoleErrors(page)
  const trades = (await (await request.get('/api/v1/trades')).json()) as TradesResponse
  expect(trades.trades.map((trade) => trade.id)).toEqual([...DEMO_BOOK_TRADE_IDS])
  for (const id of DEMO_TRADE_IDS) {
    expect(trades.trades.map((trade) => trade.id)).not.toContain(id)
  }

  await gotoApp(page, { selected: [...DEMO_BOOK_TRADE_IDS] })
  await expect(tradeRows(page)).toHaveCount(3)

  const strangle = tradeRow(page, 'Oct Strangle')
  await expect(cell(strangle, COL.opened)).toHaveText('Aug 24 2026')
  await expect(cell(strangle, COL.dte)).toHaveText('46')
  await expect(cell(strangle, COL.structure)).toHaveText('-1x STRG 8200/7200 OCT26')
  const condor = tradeRow(page, 'Nov Condor')
  await expect(cell(condor, COL.opened)).toHaveText('Aug 26 2026')
  await expect(cell(condor, COL.dte)).toHaveText('81')
  await expect(cell(condor, COL.structure)).toHaveText('-2x IC 8400/8500/7000/6900 NOV26')

  // The XSP hedge is listed beside the SPX trades: ten Mini-SPX 650 puts, read
  // off the sheet as XSP and priced as a tenth of the SPX 6500 put.
  await expect(tradeRow(page, 'XSP Hedge')).toBeVisible()
  const hedge = trades.trades.find((trade) => trade.name === 'XSP Hedge')
  expect(hedge?.legs).toHaveLength(1)
  expect(hedge?.legs[0]).toMatchObject({
    qty: 10,
    contract: { underlying: 'XSP', root: 'XSP', settlement: 'pm', strike: 650, tos_code: 'XSP261218P650' },
  })
  expect(hedge?.legs[0]?.mark).toBeGreaterThan(0)

  // All selected and priced off the fixture surface, so the graph draws.
  // (The XSP hedge rides on the same SPX axis.)
  await expect(page.getByTestId('risk-chart')).toBeVisible()
  await expect(page.getByText('DEMO · fixture')).toBeVisible()
  expect(consoleErrors).toEqual([])
})

test('the demo book is clean: no warnings above info, nothing quarantined', async ({
  page,
  request,
}) => {
  const { warnings } = (await (await request.get('/api/v1/warnings')).json()) as WarningsResponse
  expect(warnings.filter((w) => w.severity !== 'info')).toEqual([])
  expect(warnings.filter((w) => w.code === 'duplicate_execution_claim')).toEqual([])

  await gotoApp(page, { tab: 'health' })
  // The tab badge and the severity filter are both count-driven and both
  // render nothing at zero (`App.tsx`, `WarningsCard.tsx`). The demo book is
  // now a clean Legs CSV, so zero is the honest answer here — derived from
  // the API rather than pinned, so the assertion keeps meaning something if
  // the fixture ever grows a row worth warning about.
  const tab = page.getByRole('button', {
    name: warnings.length > 0 ? `Data health ${warnings.length}` : 'Data health',
    exact: true,
  })
  await expect(tab).toBeVisible()
  // The severity filters are always rendered, count and all; only the tab
  // badge above disappears at zero.
  await expect(
    page.getByRole('button', { name: `info ${warnings.length}`, exact: true }),
  ).toBeVisible()
})

test('the closed tab shows the September put spread with its realised P&L', async ({
  page,
  request,
}) => {
  const closed = (await (await request.get('/api/v1/trades/closed')).json()) as ClosedTradesResponse
  expect(closed.trades.map((trade) => trade.name)).toEqual(['Sep Put Spread'])

  await gotoApp(page, { tab: 'closed' })
  const table = page.getByRole('table').first()
  await expect(table.locator('tbody > tr')).toHaveCount(1)
  const row = table
    .getByRole('row')
    .filter({ has: page.getByRole('button', { name: 'Expand Sep Put Spread', exact: true }) })
  await expect(row.getByRole('cell').nth(2)).toHaveText('Aug 10 2026')
  await expect(row.getByRole('cell').nth(3)).toHaveText('Aug 21 2026')
  // Sold at 9.00, bought back at 6.80, two lots: +$440 less $10.08 of fees.
  await expect(row.getByRole('cell').nth(6)).toHaveText('+$440.00')
  await expect(row.getByRole('cell').nth(7)).toHaveText('$10.08')
  await expect(row.getByRole('cell').nth(8)).toHaveText('+$429.92')
  await expect(page.getByText('count 1')).toBeVisible()
  await expect(page.getByText('win rate 100%')).toBeVisible()
})
