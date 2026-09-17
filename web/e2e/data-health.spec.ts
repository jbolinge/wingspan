/** The data-health tab — the Legs CSV's own warning codes with their fix
 *  hints, and a reconciliation card that honestly has nothing to reconcile
 *  (a Legs book has no account statement behind it). */

import { expect, test } from '@playwright/test'
import type { ReconciliationResponse, WarningsResponse } from '../src/api/types'
import { EDGE_BASE_URL } from '../playwright.config'
import { gotoApp } from './helpers'

/**
 * Non-`sheet_*` codes the Legs path can still raise: `disambiguate_trade_ids`
 * (assembly) and the model/pricing layer, which are source-agnostic. Every
 * other code in `warning.rs` (`unmatched_order`, `duplicate_execution_claim`,
 * `statement_*`, …) is folder/statement-only and must never appear here.
 */
const NON_SHEET_CODES = new Set([
  'duplicate_trade_id',
  'closed_trade_has_open_legs',
  'expired_leg_unknown_settlement',
  'iv_solve_failed',
  'iv_clamped',
  'missing_quote',
  'vol_fallback',
  'forward_fallback',
])

/** `Data health` (no badge) when nothing is outstanding, `Data health N`
 *  once it is — the tab badge is only rendered for `N > 0` (`App.tsx`). */
function healthTabName(count: number): string {
  return count > 0 ? `Data health ${count}` : 'Data health'
}

test('the warnings list carries only the Legs source’s own codes, with fix hints', async ({
  page,
  request,
}) => {
  const { warnings } = (await (await request.get('/api/v1/warnings')).json()) as WarningsResponse
  expect(warnings.every((w) => w.code.startsWith('sheet_') || NON_SHEET_CODES.has(w.code))).toBe(
    true,
  )
  const counts = {
    error: warnings.filter((w) => w.severity === 'error').length,
    warn: warnings.filter((w) => w.severity === 'warn').length,
    info: warnings.filter((w) => w.severity === 'info').length,
  }

  await gotoApp(page, { tab: 'health' })

  // The filter buttons carry the counts by severity, whatever they are.
  const all = page.getByRole('button', { name: `all ${warnings.length}`, exact: true })
  await expect(all).toBeVisible()
  await expect(all).toHaveAttribute('aria-pressed', 'true')
  await expect(page.getByRole('button', { name: `error ${counts.error}`, exact: true })).toBeVisible()
  await expect(page.getByRole('button', { name: `warn ${counts.warn}`, exact: true })).toBeVisible()
  await expect(page.getByRole('button', { name: `info ${counts.info}`, exact: true })).toBeVisible()

  // The tab badge counts the same warnings the server reported.
  await expect(
    page.getByRole('button', { name: healthTabName(warnings.length), exact: true }),
  ).toBeVisible()

  // Every warning that carries a fix hint shows it; every listed item names
  // its own code and severity.
  await all.click()
  // Scoped to the Warnings card's own list: the market-status card above it
  // also renders list items (one per degraded-provider reason, and demo mode
  // is always one of those), so a bare `listitem` role would over-count.
  const items = page.getByTestId('warnings-list').getByRole('listitem')
  await expect(items).toHaveCount(warnings.length)
  const byCode = new Map<string, number>()
  for (const warning of warnings) byCode.set(warning.code, (byCode.get(warning.code) ?? 0) + 1)
  for (const [code, count] of byCode) {
    await expect(items.filter({ hasText: code })).toHaveCount(count)
  }
})

/* The clear/restore flow needs a source that actually carries warnings, and
 * the main fixture CSV is deliberately clean (it is the export-legs parity
 * fixture — every leg priced, every OCC symbol valid). The edge server's
 * copy has the three bad rows appended, so this one test runs there. */
test.describe('warnings on a source that has some', () => {
  test.use({ baseURL: EDGE_BASE_URL })

  test('clearing warnings hides them until restored, survives a reload, and folds the card', async ({
    page,
    request,
  }) => {
    const { warnings } = (await (await request.get('/api/v1/warnings')).json()) as WarningsResponse
    // Clear one whole severity group — whichever one this source populates —
    // rather than assuming the Legs path raises a particular level.
    const level = (['info', 'warn', 'error'] as const).find(
      (s) => warnings.some((w) => w.severity === s),
    )
    test.skip(level === undefined, 'no warnings on this source to clear and restore')
    const chosen = level as 'info' | 'warn' | 'error'
    const cleared = warnings.filter((w) => w.severity === chosen).length

    // Not seeded: the seed is re-applied on every navigation and would undo
    // the very persistence this test is about.
    await page.goto('/')
    await expect(page.getByRole('banner').getByText('DEMO · fixture')).toBeVisible()
    await page.getByRole('button', { name: healthTabName(warnings.length), exact: true }).click()

    const remaining = warnings.length - cleared

    // Clear only the chosen severity: the severity filter scopes the button.
    await page.getByRole('button', { name: `${chosen} ${cleared}`, exact: true }).click()
    await page.getByRole('button', { name: `clear ${cleared}`, exact: true }).click()
    await expect(page.getByText(`Nothing left to show — ${cleared} cleared.`)).toBeVisible()
    await expect(page.getByRole('button', { name: `${chosen} 0`, exact: true })).toBeVisible()
    await expect(page.getByRole('button', { name: `all ${remaining}`, exact: true })).toBeVisible()
    await expect(
      page.getByRole('button', { name: healthTabName(remaining), exact: true }),
    ).toBeVisible()
    await expect(page.getByRole('button', { name: `restore ${cleared}`, exact: true })).toBeVisible()

    // The warnings are re-derived by the server on every load; the clearing
    // is remembered on this side, so a reload does not bring them back.
    await page.reload()
    await expect(
      page.getByRole('button', { name: healthTabName(remaining), exact: true }),
    ).toBeVisible()
    await expect(page.getByRole('button', { name: `all ${remaining}`, exact: true })).toBeVisible()

    // The card folds to its header; the filters and the buttons stay.
    const card = page
      .locator('section')
      .filter({ has: page.getByRole('heading', { name: 'Warnings', exact: true }) })
    await page.getByRole('button', { name: 'Collapse Warnings' }).click()
    await expect(card.getByTestId('warnings-list').getByRole('listitem')).toHaveCount(0)
    await expect(card.getByRole('button', { name: `all ${remaining}`, exact: true })).toBeVisible()
    await page.getByRole('button', { name: 'Expand Warnings' }).click()
    await expect(card.getByTestId('warnings-list').getByRole('listitem')).toHaveCount(remaining)

    await page.getByRole('button', { name: `restore ${cleared}`, exact: true }).click()
    await expect(
      page.getByRole('button', { name: `all ${warnings.length}`, exact: true }),
    ).toBeVisible()
    await expect(
      page.getByRole('button', { name: healthTabName(warnings.length), exact: true }),
    ).toBeVisible()
    await expect(page.getByRole('button', { name: /^restore/ })).toHaveCount(0)
  })
})

test('the reconciliation card explains there is nothing to reconcile against', async ({
  page,
  request,
}) => {
  const recon = (await (
    await request.get('/api/v1/reconciliation')
  ).json()) as ReconciliationResponse
  // A Legs book has no account statement behind it: `statement_as_of` is
  // null and `rows` is empty, and `source_note` carries the honest sentence
  // the empty table would otherwise leave unexplained.
  expect(recon.statement_as_of).toBeNull()
  expect(recon.rows).toEqual([])
  expect(recon.source_note).not.toBeNull()

  await gotoApp(page, { tab: 'health' })

  const card = page
    .locator('section')
    .filter({ has: page.getByRole('heading', { name: /^Reconciliation/ }) })
  // No `— statement …` suffix: there is no statement date to show.
  await expect(card.getByRole('heading')).toHaveText('Reconciliation')
  await expect(card.getByText(recon.source_note as string)).toBeVisible()
  await expect(
    card.getByText('Per-trade attribution intentionally differs from broker/tax lots.'),
  ).toBeVisible()
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
  // There is no folder behind a Legs book — the resolved config names the
  // source instead of pointing at a folder the fixtures dev harness happened
  // to launch with.
  await expect(json).toContainText('"source"')
  await expect(json).toContainText('"kind": "demo"')
  await expect(json).not.toContainText('trades_root')
  await expect(json).not.toContainText('account_number')
})
