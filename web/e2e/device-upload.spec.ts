/** "On this device" (`docs/API.md`: `POST /api/v1/trades/upload`) driven from
 *  the Trades dialog's device tab.
 *
 *  The dialog's "On server" tab lists a *different* machine's filesystem —
 *  this tab is for the common case where the SPA is viewed from a laptop
 *  whose trade files the server can never reach: the browser reads a folder
 *  picked with `<input webkitdirectory>` and uploads a snapshot. Playwright
 *  has no automation surface for the native `showDirectoryPicker()` dialog,
 *  so every pick here goes through `setInputFiles` on that hidden input —
 *  the same fallback real browsers without File System Access use, and the
 *  one path Playwright can drive.
 *
 *  Two folders are staged under `target/e2e-root/` (mirrors
 *  `trades-root.spec.ts`'s own staging):
 *   - `device-trades`, a copy of `fixtures/trades` with `Open Trades/M1 July
 *     14 2026` renamed to `Open Trades/Uploady July 14 2026`, plus the
 *     fixtures' own `README.md` at the top level (outside the trades layout,
 *     the one file the client-side filter is expected to skip) — proves a
 *     real snapshot upload swaps the book;
 *   - `device-no-layout`, a single file whose name contains a colon
 *     (`Open Trades/weird:name.txt`). The client's own layout filter mirrors
 *     the server's and would normally keep nothing from a folder with no
 *     matching subfolders at all — which never reaches the network, since
 *     the Upload button stays disabled with nothing kept. A colon survives
 *     that filter (nothing in it says a filename can't have one) but fails
 *     the server's stricter `sanitize()` (drive-letter/NUL guard), so the
 *     file is *sent*, kept client-side, and only then skipped server-side —
 *     the one reachable way to see the real 422 sentence rendered from an
 *     actual round trip rather than the client's own "nothing to upload".
 *
 *  Like `trades-root.spec.ts`, the trades-root override (and now the
 *  uploaded snapshot it can carry) outlives a single test, so `afterEach`
 *  clears it as a backstop for every test in this file. */

import { cp, mkdir, rename, rm, writeFile } from 'node:fs/promises'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { expect, test } from '@playwright/test'
import { gotoApp, tradeRow, tradeRows } from './helpers'

const HERE = dirname(fileURLToPath(import.meta.url))
const REPO_ROOT = resolve(HERE, '../..')
const STAGE_ROOT = resolve(REPO_ROOT, 'target/e2e-root')
/** A real trades folder, readable by the server too — the pick just never
 *  goes through the server for this tab. */
const DEVICE_TRADES = resolve(STAGE_ROOT, 'device-trades')
/** One file, kept by the client's layout filter, skipped by the server's
 *  stricter filename sanitizer — see the file banner above. */
const DEVICE_NO_LAYOUT = resolve(STAGE_ROOT, 'device-no-layout')

const DEMO_LABEL = 'Trades folder: trades'
const UPLOADED_LABEL = 'Trades folder: this device'

test.describe('device upload', () => {
  test.beforeAll(async () => {
    await mkdir(STAGE_ROOT, { recursive: true })

    await rm(DEVICE_TRADES, { recursive: true, force: true })
    await cp(resolve(REPO_ROOT, 'fixtures/trades'), DEVICE_TRADES, { recursive: true })
    await rename(
      resolve(DEVICE_TRADES, 'Open Trades/M1 July 14 2026'),
      resolve(DEVICE_TRADES, 'Open Trades/Uploady July 14 2026'),
    )

    await rm(DEVICE_NO_LAYOUT, { recursive: true, force: true })
    await mkdir(resolve(DEVICE_NO_LAYOUT, 'Open Trades'), { recursive: true })
    await writeFile(resolve(DEVICE_NO_LAYOUT, 'Open Trades/weird:name.txt'), 'not a real trade file\n')
  })

  // Belt-and-suspenders, same as `trades-root.spec.ts`: whatever a test did
  // to the root — including an uploaded snapshot — the server must be back on
  // the demo fixtures before the next test (in this file or any other spec
  // sharing the same server) reads it. `DELETE /trades/root` also removes any
  // uploaded copy on disk (`state.rs`), so nothing more to clean up here.
  test.afterEach(async ({ request }) => {
    await request.delete('/api/v1/trades/root')
  })

  async function openDeviceTab(page: import('@playwright/test').Page) {
    const dialog = page.getByRole('dialog', { name: 'Trades folder' })
    await dialog.getByRole('tab', { name: 'On this device' }).click()
    const panel = page.getByRole('tabpanel', { name: 'On this device' })
    await expect(panel.getByRole('button', { name: 'Choose folder…' })).toBeVisible()
    return panel
  }

  test('picking a real folder on this device uploads a snapshot, and Reset restores the demo book', async ({
    page,
  }) => {
    await gotoApp(page)
    await expect(tradeRows(page)).toHaveCount(6)
    await expect(tradeRow(page, 'M1')).toBeVisible()

    const tradesBtn = page.getByRole('button', { name: DEMO_LABEL })
    await tradesBtn.click()
    const panel = await openDeviceTab(page)

    const input = page.getByLabel('Trades folder on this device')
    await input.setInputFiles(DEVICE_TRADES)

    // Pre-upload summary: 6 open + 2 closed trade files, 10 statements, the
    // top-level README.md the one thing outside the layout.
    await expect(panel.getByText('device-trades')).toBeVisible()
    await expect(panel.getByText('8 trade files / 10 statements')).toBeVisible()
    await expect(panel.getByText('1 (1 outside the trades layout)')).toBeVisible()
    await expect(panel.getByText('46 KiB')).toBeVisible()

    await panel.getByRole('button', { name: 'Upload & analyze' }).click()
    await expect(panel.getByRole('status')).toContainText(
      '6 open / 2 closed trades loaded from this device',
    )

    const dialog = page.getByRole('dialog', { name: 'Trades folder' })
    await expect(dialog).toBeHidden()

    const uploadedBtn = page.getByRole('button', { name: UPLOADED_LABEL })
    await expect(uploadedBtn).toBeVisible()
    await expect(uploadedBtn).toHaveAttribute('title', /uploaded from this device/)

    await expect(tradeRows(page)).toHaveCount(6)
    await expect(tradeRow(page, 'Uploady')).toBeVisible()
    await expect(tradeRow(page, 'M1')).toHaveCount(0)

    // Reset from the status bar reopens on the server tab (an uploaded root
    // is not something the device tab itself offers to drop).
    await uploadedBtn.click()
    await expect(dialog).toBeVisible()
    await expect(dialog.getByRole('tab', { name: 'On server', selected: true })).toBeVisible()
    await dialog.getByRole('button', { name: 'Reset to config' }).click()
    await expect(dialog.getByRole('status')).toContainText('6 open / 2 closed trades loaded from')
    await expect(dialog).toBeHidden()

    await expect(page.getByRole('button', { name: DEMO_LABEL })).toBeVisible()
    await expect(tradeRows(page)).toHaveCount(6)
    await expect(tradeRow(page, 'Uploady')).toHaveCount(0)
    for (const trade of ['M1', 'M2', 'M3', 'M4', 'M5', 'BSH']) {
      await expect(tradeRow(page, trade)).toBeVisible()
    }
  })

  test('a folder with nothing the server accepts shows the 422 discovery sentence inline', async ({
    page,
  }) => {
    await gotoApp(page)

    const tradesBtn = page.getByRole('button', { name: DEMO_LABEL })
    await tradesBtn.click()
    const panel = await openDeviceTab(page)

    const input = page.getByLabel('Trades folder on this device')
    await input.setInputFiles(DEVICE_NO_LAYOUT)

    // The client's own filter keeps the file (its name reads as a trade file
    // straight under `Open Trades`), so nothing stops the upload client-side.
    await expect(panel.getByText('1 trade file / 0 statements')).toBeVisible()
    const uploadBtn = panel.getByRole('button', { name: 'Upload & analyze' })
    await expect(uploadBtn).toBeEnabled()
    await uploadBtn.click()

    const error = panel.getByRole('alert')
    await expect(error).toContainText('No trades layout in that folder.')
    await expect(error).toContainText('no trades folders under the upload')
    await expect(error).toContainText('Open Trades')
    await expect(error).toContainText('1 file(s) were skipped')

    // Nothing changed: the dialog stays open on the failed attempt, and the
    // active root behind it is still the demo fixtures.
    const dialog = page.getByRole('dialog', { name: 'Trades folder' })
    await expect(dialog).toBeVisible()
    await expect(tradesBtn).toBeVisible()

    await page.keyboard.press('Escape')
    await expect(dialog).toBeHidden()
    await expect(page.getByRole('button', { name: DEMO_LABEL })).toBeVisible()
    await expect(tradeRow(page, 'M1')).toBeVisible()
  })

  test('the tabs are keyboard-accessible, and the server tab still works after using the device tab', async ({
    page,
  }) => {
    await gotoApp(page)

    const tradesBtn = page.getByRole('button', { name: DEMO_LABEL })
    await tradesBtn.click()
    const dialog = page.getByRole('dialog', { name: 'Trades folder' })
    const serverTab = dialog.getByRole('tab', { name: 'On server' })
    const deviceTab = dialog.getByRole('tab', { name: 'On this device' })

    // Opens on the server tab, focus on a roving tabindex strip.
    await expect(serverTab).toHaveAttribute('aria-selected', 'true')
    await expect(deviceTab).toHaveAttribute('tabindex', '-1')
    await serverTab.focus()

    await page.keyboard.press('ArrowRight')
    await expect(deviceTab).toHaveAttribute('aria-selected', 'true')
    await expect(deviceTab).toBeFocused()
    await expect(page.getByRole('tabpanel', { name: 'On this device' })).toBeVisible()

    await page.keyboard.press('End')
    await expect(deviceTab).toBeFocused() // already the last tab

    await page.keyboard.press('Home')
    await expect(serverTab).toHaveAttribute('aria-selected', 'true')
    await expect(serverTab).toBeFocused()
    await expect(
      page.getByRole('tabpanel', { name: 'On server' }).getByRole('button', { name: '⌂ Home' }),
    ).toBeVisible()

    await page.keyboard.press('ArrowLeft')
    await expect(deviceTab).toHaveAttribute('aria-selected', 'true') // wraps around

    // Use the device tab for real (a pick, not just a visit), then prove the
    // server tab still drives a real pick end-to-end afterwards — the
    // `useFsDirs` gate that only listens while the server tab shows must not
    // wedge on the way back.
    await deviceTab.click()
    const panel = page.getByRole('tabpanel', { name: 'On this device' })
    await panel.getByLabel('Trades folder on this device').setInputFiles(DEVICE_NO_LAYOUT)
    await expect(panel.getByText('1 trade file / 0 statements')).toBeVisible()

    await serverTab.click()
    const pathField = dialog.getByRole('textbox', { name: 'Folder path' })
    await pathField.fill(DEVICE_TRADES)
    await dialog.getByRole('button', { name: 'Go', exact: true }).click()
    await expect(dialog.getByRole('button', { name: 'Open Trades', exact: true })).toBeVisible()

    await dialog.getByRole('button', { name: 'Select', exact: true }).click()
    await expect(dialog.getByRole('status')).toContainText(
      `6 open / 2 closed trades loaded from ${DEVICE_TRADES}`,
    )
    await expect(dialog).toBeHidden()

    await expect(tradeRows(page)).toHaveCount(6)
    await expect(tradeRow(page, 'Uploady')).toBeVisible()
    await expect(tradeRow(page, 'M1')).toHaveCount(0)

    // Back to the demo fixtures for the next test.
    const overrideBtn = page.getByRole('button', { name: 'Trades folder: device-trades' })
    await overrideBtn.click()
    await expect(dialog).toBeVisible()
    await dialog.getByRole('button', { name: 'Reset to config' }).click()
    await expect(dialog.getByRole('status')).toContainText('6 open / 2 closed trades loaded from')
    await expect(dialog).toBeHidden()
    await expect(page.getByRole('button', { name: DEMO_LABEL })).toBeVisible()
  })
})
