/** Loading a book from a folder on this computer (`docs/IPC.md`: the
 *  `trades_root`, `probe_folder`, `set_trades_root` and `clear_trades_root`
 *  commands) — the only way trades reach the app. This spec drives them over
 *  `wingspan-devserver`'s HTTP mapping (`GET`/`PUT`/`DELETE /api/v1/trades/root`,
 *  `POST /api/v1/trades/root/probe`), not the desktop app's IPC transport.
 *
 *  Trade files are read **in place** and watched. Picking a folder is two
 *  steps: probe (summarise a candidate without touching what is loaded),
 *  then commit (`PUT`, behind "Load & analyze") — so a wrong pick costs a
 *  glance, not the book already showing.
 *
 *  A second trades folder is staged under `target/e2e-root/` — a copy of
 *  `fixtures/trades` with `Open Trades/M1 July 14 2026` renamed to `Zed July
 *  14 2026` — so the suite can prove picking a *different* real folder
 *  actually swaps the book, not just that the dialog round-trips the same
 *  one. `target/e2e-root/empty` is a readable folder with no trades layout
 *  anywhere under it (nor one level down): a clean 422.
 *
 *  The active root lives in `<state_dir>/trades_root.json` and outlives a
 *  single test, so every test that sets one restores the demo default
 *  itself, and `afterEach` clears it again as a backstop for every test in
 *  this file (and any other spec sharing the same server). */

import { cp, mkdir, rm } from 'node:fs/promises'
import { resolve } from 'node:path'
import { expect, test } from '@playwright/test'
import type { TradesRootResponse } from '../src/api/types'
import { gotoApp, REPO_ROOT, stageTradesFolder, tradeRow, tradeRows } from './helpers'

const STAGE_ROOT = resolve(REPO_ROOT, 'target/e2e-root')
/** A second, valid trades folder — same layout, one renamed trade file. */
const ALT_TRADES = resolve(STAGE_ROOT, 'alt-trades')
/** A readable folder with no `Open Trades`/`Closed Trades`/`Daily Trade
 *  Statement` anywhere under it (nor one level down): a clean 422. */
const EMPTY_DIR = resolve(STAGE_ROOT, 'empty')

const DEMO_LABEL = 'Trades folder: trades'
const ALT_LABEL = 'Trades folder: alt-trades'
const FOLDER_DIALOG = 'Choose your trades folder'

async function rootOf(request: import('@playwright/test').APIRequestContext) {
  const response = await request.get('/api/v1/trades/root')
  expect(response.ok()).toBeTruthy()
  return (await response.json()) as TradesRootResponse
}

test.describe('local folder picker', () => {
  test.beforeAll(async () => {
    await mkdir(STAGE_ROOT, { recursive: true })
    await stageTradesFolder(ALT_TRADES, 'Zed')
    await rm(EMPTY_DIR, { recursive: true, force: true })
    await mkdir(EMPTY_DIR, { recursive: true })
  })

  // Belt-and-suspenders: whatever a test did to the root, the server must be
  // back on the demo fixtures before the next test (in this file or any
  // other spec sharing the same server) reads it. Idempotent — a no-op when
  // nothing is overridden.
  test.afterEach(async ({ request }) => {
    await request.delete('/api/v1/trades/root')
  })

  test('choosing a folder shows a summary first, then swaps the book', async ({
    page,
    request,
  }) => {
    await gotoApp(page)
    await expect(tradeRow(page, 'M1')).toBeVisible()

    const tradesBtn = page.getByRole('button', { name: DEMO_LABEL })
    await tradesBtn.click()
    const dialog = page.getByRole('dialog', { name: FOLDER_DIALOG })
    await dialog.getByRole('textbox', { name: 'Folder path' }).fill(ALT_TRADES)
    await dialog.getByRole('button', { name: 'Go' }).click()

    const summary = dialog.getByRole('group', { name: 'Folder summary' })
    await expect(summary).toBeVisible()
    await expect(summary).toContainText(ALT_TRADES)
    await expect(summary).toContainText('6 open / 2 closed')

    // Summary only: the book behind the dialog has not moved yet.
    await expect(tradeRow(page, 'M1')).toBeVisible()
    await expect(tradeRow(page, 'Zed')).toHaveCount(0)

    await dialog.getByRole('button', { name: 'Load & analyze' }).click()
    await expect(dialog).toBeHidden()

    await expect(tradeRows(page)).toHaveCount(6)
    await expect(tradeRow(page, 'Zed')).toBeVisible()
    await expect(tradeRow(page, 'M1')).toHaveCount(0)

    const altBtn = page.getByRole('button', { name: ALT_LABEL })
    await expect(altBtn).toBeVisible()
    await expect(altBtn).toHaveAttribute('title', /folder on this computer/)

    const root = await rootOf(request)
    expect(root.source).toBe('local')
    expect(root.watching).toBe(true)
  })

  test('a folder with no trades layout is refused inline', async ({ page, request }) => {
    await gotoApp(page)

    const tradesBtn = page.getByRole('button', { name: DEMO_LABEL })
    await tradesBtn.click()
    const dialog = page.getByRole('dialog', { name: FOLDER_DIALOG })
    await dialog.getByRole('textbox', { name: 'Folder path' }).fill(EMPTY_DIR)
    await dialog.getByRole('button', { name: 'Go' }).click()

    const error = dialog.getByRole('alert')
    await expect(error).toContainText('no trades folders under')
    await expect(error).toContainText('Open Trades')
    await expect(dialog.getByRole('button', { name: 'Load & analyze' })).toBeDisabled()

    // The failed probe must not have changed the active root.
    expect((await rootOf(request)).source).toBe('demo')

    await page.keyboard.press('Escape')
    await expect(dialog).toBeHidden()
    await expect(tradesBtn).toBeVisible()
  })

  test('editing a trade file updates the row with no manual reload', async ({ page, request }) => {
    // Setup only — not what this test is about — so drive it straight
    // through the API rather than the dialog.
    const put = await request.put('/api/v1/trades/root', { data: { root: ALT_TRADES } })
    expect(put.ok()).toBeTruthy()

    await gotoApp(page)
    await expect(tradeRows(page)).toHaveCount(6)
    await expect(tradeRow(page, 'Zed')).toBeVisible()

    // A change under `Open Trades/` that alters what the table shows: a
    // seventh trade file, copied from an existing one. The name still has to
    // parse as `<name> <Month> <day> <year>` (`trade_file.rs`) — appending a
    // suffix to M2's own name would make it an unparseable stray file and be
    // silently skipped, not a new trade.
    const extra = resolve(ALT_TRADES, 'Open Trades/Extra July 21 2026')
    await cp(resolve(ALT_TRADES, 'Open Trades/M2 July 21 2026'), extra)
    try {
      // No reload, no click: the watcher's 750ms debounce plus a real fs
      // round trip earns a generous timeout.
      await expect(tradeRows(page)).toHaveCount(7, { timeout: 15_000 })
    } finally {
      await rm(extra, { force: true })
    }
  })
})
