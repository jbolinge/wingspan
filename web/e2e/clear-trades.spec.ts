/** The status bar's "clear trades" button: a confirmation dialog, then the
 *  analysis set is emptied — and when the book was uploaded from this device,
 *  every byte of it is deleted from the server (`DELETE
 *  /api/v1/trades/root`). A book the deployer configured (here, `--demo`'s)
 *  is not ours to delete and is left alone.
 *
 *  The upload here goes through the real dialog (`setInputFiles` on the
 *  `webkitdirectory` input, as in `device-upload.spec.ts`), staged under
 *  `target/e2e-root/` from a copy of the fixtures with M1 renamed so the
 *  uploaded book is tellable from the demo one. */

import { cp, mkdir, rename, rm } from 'node:fs/promises'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { expect, test, type Page } from '@playwright/test'
import type { TradesRootResponse } from '../src/api/types'
import { DEMO_TRADE_IDS, gotoApp, tradeRow, tradeRows } from './helpers'

const HERE = dirname(fileURLToPath(import.meta.url))
const REPO_ROOT = resolve(HERE, '../..')
const DEVICE_TRADES = resolve(REPO_ROOT, 'target/e2e-root/clear-device-trades')

const DEMO_LABEL = 'Trades folder: trades'
const UPLOADED_LABEL = 'Trades folder: this device'

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
  await expect(dialog).toContainText('Nothing was uploaded, so there is nothing to delete')
  await dialog.getByRole('button', { name: 'Clear trades', exact: true }).click()

  await expect(dialog).toBeHidden()
  await expect(page.getByTestId('risk-chart')).toHaveCount(0)
  await expect(page.getByText(/Select trades in the table/)).toBeVisible()
  // Nothing selected and nothing uploaded: the button has nothing left to do.
  await expect(clearBtn(page)).toBeDisabled()

  expect((await rootOf(request)).source).toBe('demo')
})

test.describe('with an uploaded snapshot', () => {
  test.beforeAll(async () => {
    await rm(DEVICE_TRADES, { recursive: true, force: true })
    await mkdir(dirname(DEVICE_TRADES), { recursive: true })
    await cp(resolve(REPO_ROOT, 'fixtures/trades'), DEVICE_TRADES, { recursive: true })
    await rename(
      resolve(DEVICE_TRADES, 'Open Trades/M1 July 14 2026'),
      resolve(DEVICE_TRADES, 'Open Trades/Uploady July 14 2026'),
    )
  })

  // Same backstop as `device-upload.spec.ts`: whatever this test left behind,
  // the next spec must find the demo book.
  test.afterEach(async ({ request }) => {
    await request.delete('/api/v1/trades/root')
  })

  test('clearing deletes the uploaded book from the server', async ({ page, request }) => {
    await gotoApp(page)
    await expect(tradeRows(page)).toHaveCount(6)

    // Upload through the real dialog.
    await page.getByRole('button', { name: DEMO_LABEL }).click()
    const tradesDialog = page.getByRole('dialog', { name: 'Load trades from this device' })
    await page.getByLabel('Trades folder on this device').setInputFiles(DEVICE_TRADES)
    await tradesDialog.getByRole('button', { name: 'Upload & analyze' }).click()
    await expect(tradesDialog).toBeHidden()
    await expect(page.getByRole('button', { name: UPLOADED_LABEL })).toBeVisible()
    await expect(tradeRow(page, 'Uploady')).toBeVisible()
    expect((await rootOf(request)).source).toBe('uploaded')

    await clearBtn(page).click()
    const dialog = confirmDialog(page)
    await expect(dialog).toContainText(
      'the trade files uploaded from this device will be deleted from the server',
    )
    await dialog.getByRole('button', { name: 'Clear trades', exact: true }).click()
    await expect(dialog.getByRole('status')).toContainText('upload deleted from the server')
    await expect(dialog).toBeHidden()

    // Back on the demo book, upload gone from the server.
    await expect(page.getByRole('button', { name: DEMO_LABEL })).toBeVisible()
    await expect(tradeRow(page, 'Uploady')).toHaveCount(0)
    await expect(tradeRow(page, 'M1')).toBeVisible()
    expect((await rootOf(request)).source).toBe('demo')
  })
})
