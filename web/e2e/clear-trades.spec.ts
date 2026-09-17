/** The status bar's "clear trades" button: a confirmation dialog, then the
 *  analysis set is emptied — and when a CSV was imported from this computer,
 *  D23 says Clear only *stops showing* it (`DELETE /api/v1/trades/source`
 *  forgets the import; nothing on disk is touched). A book the deployer
 *  configured (here, `--demo --legs fixtures/trades/legs.csv`'s) is not ours
 *  to unload and is left alone.
 *
 *  The second test proves the disk half of that promise: every file under a
 *  loaded CSV's directory, and every one of its modification times, is
 *  identical before and after Clear. That filesystem assertion is the whole
 *  point of the test and must never be relaxed. */

import { resolve } from 'node:path'
import { expect, test, type Page } from '@playwright/test'
import type { TradeSourceResponse } from '../src/api/types'
import {
  DEMO_TRADE_IDS,
  filesUnder,
  gotoApp,
  REPO_ROOT,
  stageLegsCsv,
  tradeRow,
  tradeRows,
} from './helpers'

const CLEAR_LOCAL_DIR = resolve(REPO_ROOT, 'target/e2e-source/clear-local')
const CLEAR_LOCAL_CSV = resolve(CLEAR_LOCAL_DIR, 'clear-local-legs.csv')

const DEMO_LABEL = 'Trade source: demo book'
const LOCAL_LABEL = 'Trade source: clear-local-legs.csv'

// The status-bar button is lowercase "clear trades", the dialog's confirm is
// "Clear trades" — `exact` keeps role-name matching case-sensitive so the two
// stay distinguishable.
const clearBtn = (page: Page) => page.getByRole('button', { name: 'clear trades', exact: true })
const confirmDialog = (page: Page) => page.getByRole('dialog', { name: 'Clear trades' })

async function sourceOf(request: import('@playwright/test').APIRequestContext) {
  const response = await request.get('/api/v1/trades/source')
  expect(response.ok()).toBeTruthy()
  return (await response.json()) as TradeSourceResponse
}

test('clearing deselects every analyzed trade and leaves the deployer’s book alone', async ({
  page,
  request,
}) => {
  await gotoApp(page, { selected: [...DEMO_TRADE_IDS] })
  await expect(page.getByTestId('risk-chart')).toBeVisible()

  await clearBtn(page).click()
  const dialog = confirmDialog(page)
  await expect(dialog).toContainText('Are you sure you want to clear all current trades?')
  await expect(dialog).toContainText('No source is imported, so there is nothing to clear.')
  await dialog.getByRole('button', { name: 'Clear trades', exact: true }).click()

  await expect(dialog).toBeHidden()
  await expect(page.getByTestId('risk-chart')).toHaveCount(0)
  await expect(page.getByText(/Select trades in the table/)).toBeVisible()
  // Nothing selected and no source imported: the button has nothing left to do.
  await expect(clearBtn(page)).toBeDisabled()

  expect((await sourceOf(request)).kind).toBe('demo')
})

test.describe('with a CSV imported from this computer', () => {
  test.beforeAll(async () => {
    await stageLegsCsv(CLEAR_LOCAL_CSV)
  })

  // Same backstop as `trade-source.spec.ts`: whatever this test left behind,
  // the next spec must find the demo book.
  test.afterEach(async ({ request }) => {
    await request.delete('/api/v1/trades/source')
  })

  test('clearing stops showing the trades and leaves every file on disk', async ({
    page,
    request,
  }) => {
    await gotoApp(page)
    const demoCount = await tradeRows(page).count()

    // Load through the real dialog.
    await page.getByRole('button', { name: DEMO_LABEL }).click()
    const importDialog = page.getByRole('dialog', { name: 'Import trades' })
    await importDialog.getByRole('textbox', { name: 'CSV path' }).fill(CLEAR_LOCAL_CSV)
    await importDialog.getByRole('button', { name: 'Go' }).click()
    await expect(importDialog.getByRole('group', { name: 'Source summary' })).toBeVisible()
    await importDialog.getByRole('button', { name: 'Load & analyze' }).click()
    await expect(importDialog).toBeHidden()

    await expect(page.getByRole('button', { name: LOCAL_LABEL })).toBeVisible()
    expect((await sourceOf(request)).kind).toBe('csv')

    // The state to compare against once Clear has run.
    const before = await filesUnder(CLEAR_LOCAL_DIR)

    await clearBtn(page).click()
    const dialog = confirmDialog(page)
    await expect(dialog).toContainText('Wingspan will stop showing the trades from')
    await expect(dialog).toContainText('Nothing on disk')
    await dialog.getByRole('button', { name: 'Clear trades', exact: true }).click()
    await expect(dialog.getByRole('status')).toContainText('trades cleared —')
    await expect(dialog).toBeHidden()

    // Back on the demo book. The rows come back through a full re-query
    // after the clear, which on a cold CI runner has taken longer than the
    // default 10 s (it passed on retry there); the wait is generous so that
    // a slow refetch is not read as trades that were never cleared.
    await expect(page.getByRole('button', { name: DEMO_LABEL })).toBeVisible({ timeout: 30_000 })
    await expect(tradeRow(page, 'BSH')).toBeVisible({ timeout: 30_000 })
    await expect(tradeRows(page)).toHaveCount(demoCount, { timeout: 30_000 })
    expect((await sourceOf(request)).kind).not.toBe('csv')

    // The promise, in one assertion: not one byte of the imported CSV moved.
    const after = await filesUnder(CLEAR_LOCAL_DIR)
    expect(after).toEqual(before)
  })
})
