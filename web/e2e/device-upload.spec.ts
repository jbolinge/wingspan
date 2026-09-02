/** Loading a book from this device (`docs/API.md`: `POST
 *  /api/v1/trades/upload`) — the only way trades reach the app.
 *
 *  The SPA is normally viewed from a computer whose trade files the server
 *  can never reach, and the server is not allowed to keep any of its own: the
 *  browser reads a folder picked with `<input webkitdirectory>` and uploads a
 *  snapshot, which Clear deletes again. Playwright has no automation surface
 *  for the native `showDirectoryPicker()` dialog, so every pick here goes
 *  through `setInputFiles` on that hidden input — the same fallback real
 *  browsers without File System Access use, and the one path Playwright can
 *  drive.
 *
 *  Two folders are staged under `target/e2e-root/`:
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
 *  An uploaded snapshot outlives a single test, so `afterEach` clears it as a
 *  backstop for every test in this file. */

import { cp, mkdir, rename, rm, writeFile } from 'node:fs/promises'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { expect, test } from '@playwright/test'
import { gotoApp, tradeRow, tradeRows } from './helpers'

const HERE = dirname(fileURLToPath(import.meta.url))
const REPO_ROOT = resolve(HERE, '../..')
const STAGE_ROOT = resolve(REPO_ROOT, 'target/e2e-root')
/** A real trades folder, readable by the server too — the pick just never
 *  goes through the server. */
const DEVICE_TRADES = resolve(STAGE_ROOT, 'device-trades')
/** One file, kept by the client's layout filter, skipped by the server's
 *  stricter filename sanitizer — see the file banner above. */
const DEVICE_NO_LAYOUT = resolve(STAGE_ROOT, 'device-no-layout')

const DEMO_LABEL = 'Trades folder: trades'
const UPLOADED_LABEL = 'Trades folder: this device'
const DIALOG = 'Load trades from this device'

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

  // Belt-and-suspenders: whatever a test uploaded, the server must be back on
  // the demo fixtures before the next test (in this file or any other spec
  // sharing the same server) reads it. `DELETE /trades/root` also deletes the
  // uploaded copy on disk (`state.rs`), so nothing more to clean up here.
  test.afterEach(async ({ request }) => {
    await request.delete('/api/v1/trades/root')
  })

  test('picking a real folder on this device uploads a snapshot, and Clear removes it', async ({
    page,
    request,
  }) => {
    await gotoApp(page)
    await expect(tradeRows(page)).toHaveCount(6)
    await expect(tradeRow(page, 'M1')).toBeVisible()

    await page.getByRole('button', { name: DEMO_LABEL }).click()
    const dialog = page.getByRole('dialog', { name: DIALOG })
    await expect(dialog.getByRole('button', { name: 'Choose folder…' })).toBeVisible()

    await page.getByLabel('Trades folder on this device').setInputFiles(DEVICE_TRADES)

    // Pre-upload summary: 6 open + 2 closed trade files, 10 statements, the
    // top-level README.md the one thing outside the layout.
    await expect(dialog.getByText('device-trades')).toBeVisible()
    await expect(dialog.getByText('8 trade files / 10 statements')).toBeVisible()
    await expect(dialog.getByText('1 (1 outside the trades layout)')).toBeVisible()
    await expect(dialog.getByText('46 KiB')).toBeVisible()

    await dialog.getByRole('button', { name: 'Upload & analyze' }).click()
    await expect(dialog.getByRole('status')).toContainText(
      '6 open / 2 closed trades loaded from this device',
    )
    await expect(dialog).toBeHidden()

    const uploadedBtn = page.getByRole('button', { name: UPLOADED_LABEL })
    await expect(uploadedBtn).toBeVisible()
    await expect(uploadedBtn).toHaveAttribute('title', /uploaded from this device/)

    await expect(tradeRows(page)).toHaveCount(6)
    await expect(tradeRow(page, 'Uploady')).toBeVisible()
    await expect(tradeRow(page, 'M1')).toHaveCount(0)

    // Clear is the only way back — and it deletes the copy from the server.
    await page.getByRole('button', { name: 'clear trades', exact: true }).click()
    const confirm = page.getByRole('dialog', { name: 'Clear trades' })
    await confirm.getByRole('button', { name: 'Clear trades', exact: true }).click()
    await expect(confirm).toBeHidden()

    await expect(page.getByRole('button', { name: DEMO_LABEL })).toBeVisible()
    await expect(tradeRows(page)).toHaveCount(6)
    await expect(tradeRow(page, 'Uploady')).toHaveCount(0)
    for (const trade of ['M1', 'M2', 'M3', 'M4', 'M5', 'BSH']) {
      await expect(tradeRow(page, trade)).toBeVisible()
    }
    const after = await request.get('/api/v1/trades/root')
    expect((await after.json()).source).toBe('demo')
  })

  /** The removed feature, pinned from the outside: no tab that browses the
   *  server's disk, and no route behind one. */
  test('there is no way to point the server at a folder of its own', async ({ page, request }) => {
    await gotoApp(page)
    await page.getByRole('button', { name: DEMO_LABEL }).click()
    const dialog = page.getByRole('dialog', { name: DIALOG })

    await expect(dialog.getByRole('button', { name: 'Choose folder…' })).toBeVisible()
    await expect(dialog.getByRole('tab')).toHaveCount(0)
    await expect(dialog.getByRole('textbox', { name: 'Folder path' })).toHaveCount(0)
    await expect(dialog.getByRole('button', { name: 'Select', exact: true })).toHaveCount(0)

    expect((await request.get('/api/v1/fs/dirs?path=/')).status()).toBe(404)
    const put = await request.put('/api/v1/trades/root', { data: { root: DEVICE_TRADES } })
    expect(put.status()).toBe(405)
  })

  test('a folder with nothing the server accepts shows the 422 discovery sentence inline', async ({
    page,
  }) => {
    await gotoApp(page)

    const tradesBtn = page.getByRole('button', { name: DEMO_LABEL })
    await tradesBtn.click()
    const dialog = page.getByRole('dialog', { name: DIALOG })
    await expect(dialog.getByRole('button', { name: 'Choose folder…' })).toBeVisible()

    await page.getByLabel('Trades folder on this device').setInputFiles(DEVICE_NO_LAYOUT)

    // The client's own filter keeps the file (its name reads as a trade file
    // straight under `Open Trades`), so nothing stops the upload client-side.
    await expect(dialog.getByText('1 trade file / 0 statements')).toBeVisible()
    const uploadBtn = dialog.getByRole('button', { name: 'Upload & analyze' })
    await expect(uploadBtn).toBeEnabled()
    await uploadBtn.click()

    const error = dialog.getByRole('alert')
    await expect(error).toContainText('No trades layout in that folder.')
    await expect(error).toContainText('no trades folders under the upload')
    await expect(error).toContainText('Open Trades')
    await expect(error).toContainText('1 file(s) were skipped')

    // Nothing changed: the dialog stays open on the failed attempt, and the
    // book behind it is still the demo one.
    await expect(dialog).toBeVisible()
    await expect(tradesBtn).toBeVisible()

    await page.keyboard.press('Escape')
    await expect(dialog).toBeHidden()
    await expect(page.getByRole('button', { name: DEMO_LABEL })).toBeVisible()
    await expect(tradeRow(page, 'M1')).toBeVisible()
    await expect(tradesBtn).toBeFocused()
  })
})
