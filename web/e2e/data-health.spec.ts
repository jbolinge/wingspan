/** The data-health tab — LPTA's six quarantined orders as warnings with the
 *  fix hint, and a reconciliation table that balances against the 2026-08-28
 *  statement. */

import { expect, test } from '@playwright/test'
import type { ReconciliationResponse, WarningsResponse } from '../src/api/types'
import { gotoApp } from './helpers'

test('the warnings list shows the six duplicate LPTA claims and how to fix them', async ({
  page,
  request,
}) => {
  const { warnings } = (await (await request.get('/api/v1/warnings')).json()) as WarningsResponse
  const duplicates = warnings.filter((w) => w.code === 'duplicate_execution_claim')
  expect(duplicates).toHaveLength(6)
  expect(duplicates.every((w) => w.trade_id === 'lpta-2026-07-24')).toBe(true)

  await gotoApp(page, { tab: 'health' })

  // The filter buttons carry the counts by severity.
  const all = page.getByRole('button', { name: `all ${warnings.length}`, exact: true })
  await expect(all).toBeVisible()
  await expect(all).toHaveAttribute('aria-pressed', 'true')
  await expect(page.getByRole('button', { name: 'error 0', exact: true })).toBeVisible()
  await expect(page.getByRole('button', { name: 'warn 6', exact: true })).toBeVisible()
  await expect(
    page.getByRole('button', { name: `info ${warnings.length - 6}`, exact: true }),
  ).toBeVisible()

  await page.getByRole('button', { name: 'warn 6', exact: true }).click()
  const items = page.getByRole('listitem').filter({ hasText: 'duplicate_execution_claim' })
  await expect(items).toHaveCount(6)
  for (const item of await items.all()) {
    await expect(item.getByText('lpta-2026-07-24', { exact: true })).toBeVisible()
    await expect(item.getByText('warn', { exact: true })).toBeVisible()
  }
  // The hint that turns the quarantine into an actionable edit.
  await expect(
    items.first().getByText(/Fix: Add an explicit hint comment to the duplicated order line/),
  ).toBeVisible()

  // The tab badge counts the same warnings the server reported.
  await expect(page.getByRole('button', { name: `Data health ${warnings.length}` })).toBeVisible()
})

test('the reconciliation table balances against the latest statement', async ({ page, request }) => {
  const recon = (await (
    await request.get('/api/v1/reconciliation')
  ).json()) as ReconciliationResponse
  expect(recon.statement_as_of).toBe('2026-08-28')
  expect(recon.rows).toHaveLength(23)
  expect(recon.rows.every((row) => row.status === 'match')).toBe(true)

  await gotoApp(page, { tab: 'health' })

  const card = page
    .locator('section')
    .filter({ has: page.getByRole('heading', { name: /^Reconciliation/ }) })
  await expect(card.getByRole('heading')).toHaveText('Reconciliation — statement 2026-08-28')
  await expect(card.getByText('23/23 match')).toBeVisible()
  await expect(
    card.getByText('Per-trade attribution intentionally differs from broker/tax lots.'),
  ).toBeVisible()

  // "problems only" is on by default and there are none.
  const problemsOnly = card.getByRole('button', { name: 'problems only' })
  await expect(problemsOnly).toHaveAttribute('aria-pressed', 'true')
  await expect(
    card.getByText('Every contract reconciles against the latest statement.'),
  ).toBeVisible()

  // Switch it off and every contract is listed, quantity for quantity.
  await problemsOnly.click()
  await expect(card.locator('tbody > tr')).toHaveCount(23)
  await expect(card.getByText('match', { exact: true })).toHaveCount(23)
  for (const row of recon.rows.slice(0, 5)) {
    const line = card.locator('tbody > tr').filter({ hasText: row.tos_code })
    await expect(line.getByRole('cell').nth(1)).toHaveText(String(row.statement_qty))
    await expect(line.getByRole('cell').nth(2)).toHaveText(String(row.trades_qty))
    await expect(line.getByRole('cell').nth(3)).toHaveText('0')
  }
})

test('provider diagnostics and the resolved config tell the truth about demo mode', async ({
  page,
}) => {
  await gotoApp(page, { tab: 'health' })

  const diagnostics = page
    .locator('section')
    .filter({ has: page.getByRole('heading', { name: 'Provider diagnostics', exact: true }) })
  await expect(diagnostics.getByText('DEMO · fixture')).toBeVisible()
  await expect(diagnostics.getByText(/spot fixture · options fixture/)).toBeVisible()
  await expect(
    diagnostics.getByText('fixture: synthetic fixture prices (demo mode), not live market data'),
  ).toBeVisible()

  const config = page
    .locator('section')
    .filter({ has: page.getByRole('heading', { name: 'Resolved config', exact: true }) })
  await expect(
    config.getByText(/Secrets are redacted before this view is built; the account number never/),
  ).toBeVisible()
  await config.getByRole('button', { name: 'show' }).click()
  const json = config.locator('pre')
  await expect(json).toContainText('"demo": true')
  // Absolute and canonicalized, not the `fixtures/trades` the dev harness was
  // launched with — every reported root is normalised the same way.
  await expect(json).toContainText(/"trades_root": "\/[^"]*\/fixtures\/trades"/)
  await expect(json).not.toContainText('account_number')
})
