/** The server-side folder picker (`docs/API.md`: `GET`/`PUT`/`DELETE
 *  /api/v1/trades/root`, `GET /api/v1/fs/dirs`) driven from the status bar's
 *  Trades control.
 *
 *  A second trades folder is staged under `target/e2e-root/` — a copy of
 *  `fixtures/trades` with `Open Trades/M1 July 14 2026` renamed to `Zed July
 *  14 2026` (mirrors how `e2e/support/serve-edge.sh` stages its own fixtures
 *  copy) — so the suite can prove picking a *different* real folder actually
 *  swaps the book, not just that the dialog round-trips the same one.
 *
 *  The trades-root override lives in the server's state dir and outlives a
 *  single test (it is only cleared by another PUT/DELETE or a fresh process
 *  start), so every test that sets one restores the demo default itself, and
 *  `afterEach` clears it again as a backstop against a failed assertion
 *  leaving it dangling for the next test — in this file or any other. */

import { cp, mkdir, rename, rm } from 'node:fs/promises'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { expect, test } from '@playwright/test'
import { gotoApp, tradeRow, tradeRows } from './helpers'

const HERE = dirname(fileURLToPath(import.meta.url))
const REPO_ROOT = resolve(HERE, '../..')
const STAGE_ROOT = resolve(REPO_ROOT, 'target/e2e-root')
/** A second, valid trades folder — same layout, one renamed trade file. */
const ALT_TRADES = resolve(STAGE_ROOT, 'alt-trades')
/** A readable folder with no `Open Trades`/`Closed Trades`/`Daily Trade
 *  Statement` anywhere under it (nor one level down): a clean 422. */
const EMPTY_DIR = resolve(STAGE_ROOT, 'empty')

const DEMO_LABEL = 'Trades folder: trades'
const ALT_LABEL = 'Trades folder: alt-trades'

test.describe('trades root picker', () => {
  test.beforeAll(async () => {
    await rm(STAGE_ROOT, { recursive: true, force: true })
    await mkdir(STAGE_ROOT, { recursive: true })
    await cp(resolve(REPO_ROOT, 'fixtures/trades'), ALT_TRADES, { recursive: true })
    await rename(
      resolve(ALT_TRADES, 'Open Trades/M1 July 14 2026'),
      resolve(ALT_TRADES, 'Open Trades/Zed July 14 2026'),
    )
    await mkdir(EMPTY_DIR, { recursive: true })
  })

  // Belt-and-suspenders: whatever a test did to the root, the server must be
  // back on the demo fixtures before the next test (in this file or any
  // other spec sharing the same server) reads it. Idempotent — a no-op when
  // nothing is overridden.
  test.afterEach(async ({ request }) => {
    await request.delete('/api/v1/trades/root')
  })

  test('the status bar names the demo root, and the dialog lists real directories', async ({
    page,
  }) => {
    await gotoApp(page)

    const tradesBtn = page.getByRole('button', { name: DEMO_LABEL })
    await expect(tradesBtn).toBeVisible()
    await expect(tradesBtn).toHaveAttribute('title', /demo fixtures/)

    await tradesBtn.click()
    const dialog = page.getByRole('dialog', { name: 'Trades folder' })
    await expect(dialog).toBeVisible()
    // The default listing is the active root's parent, which contains the
    // real `fixtures/trades` directory the demo server is reading from.
    await expect(
      dialog.getByRole('button', { name: 'trades — contains trades layout' }),
    ).toBeVisible()

    await page.keyboard.press('Escape')
    await expect(dialog).toBeHidden()
    await expect(tradesBtn).toBeFocused()
  })

  test('picking a different real folder swaps the book, and Reset restores the demo fixtures', async ({
    page,
  }) => {
    await gotoApp(page)
    await expect(tradeRows(page)).toHaveCount(6)
    await expect(tradeRow(page, 'M1')).toBeVisible()

    const tradesBtn = page.getByRole('button', { name: DEMO_LABEL })
    await tradesBtn.click()
    const dialog = page.getByRole('dialog', { name: 'Trades folder' })
    const pathField = dialog.getByRole('textbox', { name: 'Folder path' })

    await pathField.fill(ALT_TRADES)
    await dialog.getByRole('button', { name: 'Go', exact: true }).click()
    // Confirms the navigation landed on the staged folder before selecting.
    await expect(
      dialog.getByRole('button', { name: 'Open Trades', exact: true }),
    ).toBeVisible()

    await dialog.getByRole('button', { name: 'Select', exact: true }).click()
    await expect(dialog.getByRole('status')).toContainText(
      `6 open / 2 closed trades loaded from ${ALT_TRADES}`,
    )
    await expect(dialog).toBeHidden()

    const altBtn = page.getByRole('button', { name: ALT_LABEL })
    await expect(altBtn).toBeVisible()
    await expect(altBtn).toHaveAttribute('title', new RegExp(`^${escapeRe(ALT_TRADES)} — chosen in app`))

    await expect(tradeRows(page)).toHaveCount(6)
    await expect(tradeRow(page, 'Zed')).toBeVisible()
    await expect(tradeRow(page, 'M1')).toHaveCount(0)

    // Reset back to the demo fixtures from the same control.
    await altBtn.click()
    await expect(dialog).toBeVisible()
    await dialog.getByRole('button', { name: 'Reset to config' }).click()
    await expect(dialog.getByRole('status')).toContainText('6 open / 2 closed trades loaded from')
    await expect(dialog).toBeHidden()

    const restoredBtn = page.getByRole('button', { name: DEMO_LABEL })
    await expect(restoredBtn).toBeVisible()
    await expect(restoredBtn).toHaveAttribute('title', /demo fixtures/)

    await expect(tradeRows(page)).toHaveCount(6)
    await expect(tradeRow(page, 'Zed')).toHaveCount(0)
    for (const trade of ['M1', 'M2', 'M3', 'M4', 'M5', 'BSH']) {
      await expect(tradeRow(page, trade)).toBeVisible()
    }
  })

  test('an invalid folder shows the discovery error inline', async ({ page }) => {
    await gotoApp(page)

    const tradesBtn = page.getByRole('button', { name: DEMO_LABEL })
    await tradesBtn.click()
    const dialog = page.getByRole('dialog', { name: 'Trades folder' })
    const pathField = dialog.getByRole('textbox', { name: 'Folder path' })

    await pathField.fill(EMPTY_DIR)
    await dialog.getByRole('button', { name: 'Go', exact: true }).click()
    await expect(dialog.getByText('No sub-folders here.')).toBeVisible()

    await dialog.getByRole('button', { name: 'Select', exact: true }).click()
    const error = dialog.getByRole('alert')
    await expect(error).toContainText('no trades folders under')
    await expect(error).toContainText('Open Trades')

    // The failed selection must not have changed the active root.
    await expect(page.getByRole('button', { name: DEMO_LABEL })).toBeVisible()

    // Esc still closes the dialog (even mid-error) and returns focus to the
    // control that opened it.
    await page.keyboard.press('Escape')
    await expect(dialog).toBeHidden()
    await expect(tradesBtn).toBeFocused()
  })
})

function escapeRe(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
}
