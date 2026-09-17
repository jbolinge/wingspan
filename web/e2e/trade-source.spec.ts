/** Importing a book (`docs/IPC.md`: the `trade_source`, `probe_trade_source`,
 *  `set_trade_source` and `clear_trade_source` commands) — the only way
 *  trades reach the app now that folder picking is gone. This spec drives
 *  them over `wingspan-devserver`'s HTTP mapping (`GET`/`PUT`/`DELETE
 *  /api/v1/trades/source`, `POST /api/v1/trades/source/probe`) against the
 *  main server (:8787), not the desktop app's IPC transport.
 *
 *  A CSV file is read **in place** — never moved, renamed or watched (its rows
 *  are cached under the state dir like a sheet's). Importing one is
 *  two steps: probe (summarise a candidate without touching what is loaded),
 *  then commit (`PUT`, behind "Load & analyze") — so a wrong path costs a
 *  glance, not the book already showing.
 *
 *  Two files are staged under `target/e2e-source/`: `alt-legs.csv`, a plain
 *  copy of the committed `fixtures/trades/legs.csv` (same trades — this spec
 *  is about the *source* changing, not the data), and `not-legs.csv`, a CSV
 *  with none of the required Legs columns, for the refusal path.
 *
 *  The active source lives in `<state_dir>/trade_source.json` and outlives a
 *  single test, so every test that sets one restores the demo default
 *  itself, and `afterEach` clears it again as a backstop for every test in
 *  this file (and any other spec sharing the same server).
 *
 *  **Not covered here: first run.** `kind: "empty"` is unreachable over this
 *  transport — every Playwright server boots with a source, and `DELETE
 *  /api/v1/trades/source` falls back to `--demo` rather than to nothing, so
 *  there is no way to reach the state where the dialog opens by itself.
 *  `web/src/components/TradeSourceControl.test.tsx` covers that half against
 *  a mock; the end-to-end walk is a manual release-checklist item
 *  (`docs/DESKTOP.md` §15). Closing the gap properly needs a devserver flag
 *  that boots with no source at all, and a fourth web server to run it. */

import { writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { expect, test, type APIRequestContext } from '@playwright/test'
import type { TradeSourceProbe, TradeSourceResponse } from '../src/api/types'
import { gotoApp, REPO_ROOT, stageLegsCsv, tradeRows } from './helpers'

const STAGE_ROOT = resolve(REPO_ROOT, 'target/e2e-source')
/** A plain copy of the committed fixture — same trades, a different path, so
 *  swapping to it proves the *source* moved without needing different data. */
const ALT_LEGS = resolve(STAGE_ROOT, 'alt-legs.csv')
/** A CSV with none of the required Legs columns: a clean refusal. */
const NOT_LEGS = resolve(STAGE_ROOT, 'not-legs.csv')
/** A CSV with only the four required columns: accepted, with a
 *  `sheet_column_absent` warning for each of `Opened`/`Open Price`. */
const MINIMAL_LEGS = resolve(STAGE_ROOT, 'minimal-legs.csv')

const DEMO_LABEL = 'Trade source: demo book'
const IMPORT_DIALOG = 'Import trades'

async function sourceOf(request: APIRequestContext): Promise<TradeSourceResponse> {
  const response = await request.get('/api/v1/trades/source')
  expect(response.ok()).toBeTruthy()
  return (await response.json()) as TradeSourceResponse
}

test.describe('importing a trade source', () => {
  test.beforeAll(async () => {
    await stageLegsCsv(ALT_LEGS)
    await writeFile(NOT_LEGS, 'a,b,c\n1,2,3\n', 'utf8')
    await writeFile(
      MINIMAL_LEGS,
      'Trade,Side,Qty,OCC Symbol\n' +
        'Zeta,BUY,1,SPX270219P7000\n' +
        'Zeta,SELL,1,SPX270219P6900\n',
      'utf8',
    )
  })

  // Belt-and-suspenders: whatever a test did to the source, the server must
  // be back on the demo fallback before the next test (in this file or any
  // other spec sharing the same server) reads it. Idempotent — a no-op when
  // nothing is imported.
  test.afterEach(async ({ request }) => {
    await request.delete('/api/v1/trades/source')
  })

  test('probing a CSV summarises it before anything is loaded, then swaps the book on commit', async ({
    page,
    request,
  }) => {
    await gotoApp(page)
    await expect(page.getByRole('button', { name: DEMO_LABEL })).toBeVisible()

    await page.getByRole('button', { name: DEMO_LABEL }).click()
    const dialog = page.getByRole('dialog', { name: IMPORT_DIALOG })
    await dialog.getByRole('textbox', { name: 'CSV path' }).fill(ALT_LEGS)
    await dialog.getByRole('button', { name: 'Go' }).click()

    const summary = dialog.getByRole('group', { name: 'Source summary' })
    await expect(summary).toBeVisible()
    await expect(summary).toContainText(ALT_LEGS)

    // The exact counts the Legs path produces are not guessed here — the
    // same probe the dialog just ran is repeated over the API so the
    // assertion is pinned to whatever `alt-legs.csv` actually contains.
    const probe = (await (
      await request.post('/api/v1/trades/source/probe', {
        data: { source: { kind: 'csv', path: ALT_LEGS } },
      })
    ).json()) as TradeSourceProbe
    await expect(summary).toContainText(`${probe.trades.open} open / ${probe.trades.closed} closed`)

    // Summary only: the book behind the dialog has not moved yet.
    expect((await sourceOf(request)).kind).toBe('demo')

    await dialog.getByRole('button', { name: 'Load & analyze' }).click()
    await expect(dialog).toBeHidden()

    await expect(page.getByRole('button', { name: 'Trade source: alt-legs.csv' })).toBeVisible()
    const after = await sourceOf(request)
    expect(after.kind).toBe('csv')
    expect(after.path).toMatch(/alt-legs\.csv$/)
  })

  test('a CSV that is not a Legs table is refused inline', async ({ page, request }) => {
    await gotoApp(page)

    await page.getByRole('button', { name: DEMO_LABEL }).click()
    const dialog = page.getByRole('dialog', { name: IMPORT_DIALOG })
    await dialog.getByRole('textbox', { name: 'CSV path' }).fill(NOT_LEGS)
    await dialog.getByRole('button', { name: 'Go' }).click()

    // What the dialog's own probe should have found, fetched independently
    // so the assertion below does not guess the exact column names.
    const probe = (await (
      await request.post('/api/v1/trades/source/probe', {
        data: { source: { kind: 'csv', path: NOT_LEGS } },
      })
    ).json()) as TradeSourceProbe
    expect(probe.columns_missing.length).toBeGreaterThan(0)

    const error = dialog.getByRole('alert')
    for (const column of probe.columns_missing) {
      await expect(error).toContainText(column)
    }
    await expect(dialog.getByRole('button', { name: 'Load & analyze' })).toBeDisabled()

    // The failed probe must not have changed the active source.
    expect((await sourceOf(request)).kind).toBe('demo')

    await page.keyboard.press('Escape')
    await expect(dialog).toBeHidden()
    await expect(page.getByRole('button', { name: DEMO_LABEL })).toBeVisible()
  })

  test('reloading does not drop the imported source', async ({ page, request }) => {
    // Setup only — not what this test is about — so drive it straight
    // through the API rather than the dialog.
    const put = await request.put('/api/v1/trades/source', {
      data: { source: { kind: 'csv', path: ALT_LEGS } },
    })
    expect(put.ok()).toBeTruthy()

    await gotoApp(page)
    await expect(page.getByRole('button', { name: 'Trade source: alt-legs.csv' })).toBeVisible()
    const before = await tradeRows(page).count()

    await page.getByRole('button', { name: '↻ reload' }).click()

    await expect(page.getByRole('button', { name: 'Trade source: alt-legs.csv' })).toBeVisible()
    await expect(tradeRows(page)).toHaveCount(before)
    const after = await sourceOf(request)
    expect(after.kind).toBe('csv')
    expect(after.path).toMatch(/alt-legs\.csv$/)
  })

  // A test proving the first-run empty state ("Import trades…" opens the
  // dialog on its own) is deliberately not here: it is already covered by
  // `web/src/components/TradeSourceControl.test.tsx`. A Playwright version
  // would need a devserver that boots with no source at all, and the
  // documented `serve --demo [--legs <csv>]` surface does not provide one —
  // `DELETE /api/v1/trades/source` always falls back to the `--demo` book.

  test('a CSV missing the expected columns loads with the warnings Data health names', async ({
    page,
    request,
  }) => {
    const put = await request.put('/api/v1/trades/source', {
      data: { source: { kind: 'csv', path: MINIMAL_LEGS } },
    })
    expect(put.ok()).toBeTruthy()

    await gotoApp(page, { tab: 'health' })

    const warnings = (await (await request.get('/api/v1/warnings')).json()) as {
      warnings: { code: string }[]
    }
    const absent = warnings.warnings.filter((w) => w.code === 'sheet_column_absent')
    expect(absent.length).toBeGreaterThan(0)

    await expect(
      page.getByTestId('warnings-list').getByRole('listitem').filter({ hasText: 'sheet_column_absent' }),
    ).not.toHaveCount(0)
  })
})
