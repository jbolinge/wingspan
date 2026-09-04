/** The status bar's "clear trades" button: a confirmation dialog, then the
 *  analysis set is emptied — and when a folder was loaded from this
 *  computer, D23 says Clear only *stops showing* it (`DELETE
 *  /api/v1/trades/root` forgets the folder; nothing on disk is touched). A
 *  book the deployer configured (here, `--demo --root fixtures/trades`'s) is
 *  not ours to unload and is left alone.
 *
 *  The second test proves the disk half of that promise: every file under a
 *  loaded folder, and every one of its modification times, is identical
 *  before and after Clear. That filesystem assertion is the whole point of
 *  the test and must never be relaxed. */

import { resolve } from 'node:path'
import { expect, test, type Page } from '@playwright/test'
import type { TradesRootResponse } from '../src/api/types'
import {
  DEMO_TRADE_IDS,
  filesUnder,
  gotoApp,
  REPO_ROOT,
  stageTradesFolder,
  tradeRow,
  tradeRows,
} from './helpers'

const CLEAR_LOCAL_TRADES = resolve(REPO_ROOT, 'target/e2e-root/clear-local-trades')

const DEMO_LABEL = 'Trades folder: trades'
const LOCAL_LABEL = 'Trades folder: clear-local-trades'

// The status-bar button is lowercase "clear trades", the dialog's confirm is
// "Clear trades" — `exact` keeps role-name matching case-sensitive so the two
// stay distinguishable.
const clearBtn = (page: Page) => page.getByRole('button', { name: 'clear trades', exact: true })
const confirmDialog = (page: Page) => page.getByRole('dialog', { name: 'Clear trades' })

async function rootOf(request: import('@playwright/test').APIRequestContext) {
  const response = await request.get('/api/v1/trades/root')
  expect(response.ok()).toBeTruthy()
  return (await response.json()) as TradesRootResponse
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
  await expect(dialog).toContainText('No folder is loaded, so there is nothing to clear.')
  await dialog.getByRole('button', { name: 'Clear trades', exact: true }).click()

  await expect(dialog).toBeHidden()
  await expect(page.getByTestId('risk-chart')).toHaveCount(0)
  await expect(page.getByText(/Select trades in the table/)).toBeVisible()
  // Nothing selected and no folder loaded: the button has nothing left to do.
  await expect(clearBtn(page)).toBeDisabled()

  expect((await rootOf(request)).source).toBe('demo')
})

test.describe('with a folder loaded from this computer', () => {
  test.beforeAll(async () => {
    await stageTradesFolder(CLEAR_LOCAL_TRADES, 'Localy')
  })

  // Same backstop as `local-folder.spec.ts`: whatever this test left behind,
  // the next spec must find the demo book.
  test.afterEach(async ({ request }) => {
    await request.delete('/api/v1/trades/root')
  })

  test('clearing stops showing the trades and leaves every file on disk', async ({
    page,
    request,
  }) => {
    await gotoApp(page)
    await expect(tradeRows(page)).toHaveCount(6)

    // Load through the real dialog.
    await page.getByRole('button', { name: DEMO_LABEL }).click()
    const folderDialog = page.getByRole('dialog', { name: 'Choose your trades folder' })
    await folderDialog.getByRole('textbox', { name: 'Folder path' }).fill(CLEAR_LOCAL_TRADES)
    await folderDialog.getByRole('button', { name: 'Go' }).click()
    await expect(folderDialog.getByRole('group', { name: 'Folder summary' })).toBeVisible()
    await folderDialog.getByRole('button', { name: 'Load & analyze' }).click()
    await expect(folderDialog).toBeHidden()

    await expect(page.getByRole('button', { name: LOCAL_LABEL })).toBeVisible()
    await expect(tradeRow(page, 'Localy')).toBeVisible()
    expect((await rootOf(request)).source).toBe('local')

    // The state to compare against once Clear has run.
    const before = await filesUnder(CLEAR_LOCAL_TRADES)

    await clearBtn(page).click()
    const dialog = confirmDialog(page)
    await expect(dialog).toContainText('Wingspan will stop showing the trades from')
    await expect(dialog).toContainText('Nothing on disk is touched.')
    await dialog.getByRole('button', { name: 'Clear trades', exact: true }).click()
    await expect(dialog.getByRole('status')).toContainText('trades cleared —')
    await expect(dialog).toBeHidden()

    // Back on the demo book.
    await expect(page.getByRole('button', { name: DEMO_LABEL })).toBeVisible()
    await expect(tradeRow(page, 'M1')).toBeVisible()
    await expect(tradeRow(page, 'Localy')).toHaveCount(0)
    expect((await rootOf(request)).source).not.toBe('local')

    // The promise, in one assertion: not one byte of the loaded folder moved.
    const after = await filesUnder(CLEAR_LOCAL_TRADES)
    expect(after).toEqual(before)
  })
})
