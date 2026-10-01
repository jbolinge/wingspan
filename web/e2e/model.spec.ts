/**
 * The Model tab on the demo book (`serve --demo`, :8791): the option chain the
 * fixture lists from its own clock (2026-08-31), the strangle and the condor
 * counted in the Pos column, and a modeled leg carried from the chain to the
 * ticket, the tab badge and the risk graph on Positions.
 *
 * Nothing is selected to begin with: an empty selection is "every open
 * trade" in the Model tab's Pos column, but on the graph, once a model
 * exists, it is the model alone.
 */

import { expect, test, type Locator, type Page } from '@playwright/test'
import type { ChainExpirationsResponse } from '../src/api/types'
import { DEMO_BASE_URL } from '../playwright.config'
import { collectConsoleErrors, gotoApp, positionsTable } from './helpers'

test.use({ baseURL: DEMO_BASE_URL })

/** U+2212, the Pos and Model columns' minus sign. */
const MINUS = '−'

/** The October monthly's chain section (SPX, AM-settled). */
function octoberSection(page: Page): Locator {
  return page
    .locator('section')
    .filter({ has: page.getByRole('heading', { name: /^SPX · 16 Oct 2026 · AM · \d+ DTE$/ }) })
}

/** One side's Pos cell on one strike row of a section. */
function posCell(section: Locator, strike: number, side: 'call' | 'put'): Locator {
  return section.locator(`tr[data-strike="${strike}"] td[data-col="pos"]`).nth(side === 'call' ? 0 : 1)
}

test('the Model tab lists the expirations, the October monthly AM-settled', async ({ page, request }) => {
  const listing = (await (await request.get('/api/v1/chain/expirations')).json()) as ChainExpirationsResponse
  const october = listing.expirations.filter((e) => e.expiry === '2026-10-16')
  expect(october.map((e) => [e.root, e.settlement])).toEqual([
    ['SPX', 'am'],
    ['SPXW', 'pm'],
  ])

  await gotoApp(page, { tab: 'model' })
  const strip = page.getByRole('toolbar', { name: 'Expirations' })
  const am = strip.getByRole('button', { name: /^16 OCT 26 · \d+d · AM, SPX, monthly/ })
  await expect(am).toBeVisible()
  await expect(am).toContainText('AM')
  await expect(strip.getByRole('button', { name: /^16 OCT 26 · \d+d · PM, SPXW/ })).toBeVisible()
  // Held by the strangle, so open without being asked.
  await expect(am).toHaveAttribute('aria-pressed', 'true')
})

test('the October section counts the strangle in Pos when nothing is selected', async ({ page }) => {
  await gotoApp(page, { tab: 'model' })
  await expect(page.getByRole('button', { name: 'all 2 open trades' })).toBeVisible()
  const section = octoberSection(page)
  await expect(section).toBeVisible()
  // −1 7200 P and −1 8200 C: the demo's "Oct Strangle". Both are far off the
  // 32-strike window around 7750, and shown anyway because they are held.
  await expect(posCell(section, 7200, 'put')).toHaveText(`${MINUS}1`)
  await expect(posCell(section, 8200, 'call')).toHaveText(`${MINUS}1`)
  await expect(posCell(section, 7200, 'call')).toHaveText('')
  await expect(section.getByTestId('spot-row')).toContainText('SPX 7750.00')
})

test('a modeled leg reaches the ticket, the tab badge and the risk graph; Clear model empties it all', async ({
  page,
}) => {
  const consoleErrors = collectConsoleErrors(page)
  await gotoApp(page, { tab: 'model' })
  const section = octoberSection(page)
  await expect(section).toBeVisible()

  // 7500 is outside the default window around spot; show every strike.
  await page.getByRole('group', { name: 'Strikes around spot' }).getByRole('button', { name: 'All' }).click()
  const cell = section.getByRole('spinbutton', { name: 'Model quantity, SPX 16 Oct 26 7500 put' })
  await cell.fill('1')
  await cell.press('Enter')
  await expect(cell).toHaveValue('+1')

  const ticket = page.getByRole('region', { name: 'Model ticket' })
  await expect(ticket.getByRole('listitem')).toHaveCount(1)
  await expect(ticket.getByRole('listitem')).toContainText('SPX 16 OCT 26 7500 P')
  await expect(ticket.getByText(/^net \$[\d,]+ debit$/)).toBeVisible()
  const modelTab = page.getByRole('navigation').getByRole('button', { name: 'Model 1', exact: true })
  await expect(modelTab).toBeVisible()

  // The Pos column still counts both trades, but the graph will not draw them.
  await expect(page.getByText('· the graph shows the model alone')).toBeVisible()

  // Positions: with nothing selected the graph draws the model alone, and
  // says so.
  await page.getByRole('navigation').getByRole('button', { name: 'Positions', exact: true }).click()
  await expect(page.getByText('Model · 1 leg')).toBeVisible()
  await expect(page.getByTestId('risk-chart')).toBeVisible()
  await expect(page.getByTestId('risk-chart')).toHaveAttribute('aria-label', /^Risk graph for the model alone:/)
  await expect(page.getByText(/^model only · P&L/)).toBeVisible()
  // Nothing to compare the model against.
  await expect(page.getByRole('button', { name: 'compare position' })).toBeDisabled()

  // Ticking one trade puts it under the model.
  const strangle = positionsTable(page).getByRole('checkbox', { name: 'Select Oct Strangle', exact: true })
  await strangle.check()
  await expect(page.getByText(/^1 trade \+ model · P&L/)).toBeVisible()
  await expect(page.getByTestId('risk-chart')).toHaveAttribute(
    'aria-label',
    /^Risk graph for 1 selected trades and the model:/,
  )
  await expect(page.getByRole('button', { name: 'compare position' })).toBeEnabled()
  await strangle.uncheck()
  await expect(page.getByText(/^model only · P&L/)).toBeVisible()

  await modelTab.click()
  await page.getByRole('button', { name: 'Clear modeled trades' }).click()
  await expect(ticket.getByText(/No modeled legs/)).toBeVisible()
  await expect(page.getByRole('navigation').getByRole('button', { name: 'Model', exact: true })).toBeVisible()
  await expect(section.getByRole('spinbutton', { name: 'Model quantity, SPX 16 Oct 26 7500 put' })).toHaveValue('')

  await page.getByRole('navigation').getByRole('button', { name: 'Positions', exact: true }).click()
  await expect(page.getByText('Model · 1 leg')).toHaveCount(0)
  await expect(page.getByText(/Select trades in the table/)).toBeVisible()
  expect(consoleErrors).toEqual([])
})

/** Where `inner`'s vertical middle sits in `outer`, as a fraction of its height. */
async function verticalPosition(inner: Locator, outer: Locator): Promise<number> {
  const [a, b] = await Promise.all([inner.boundingBox(), outer.boundingBox()])
  if (!a || !b) throw new Error('no layout box')
  return (a.y + a.height / 2 - b.y) / b.height
}

test('the chain opens centred on spot, and a refetch leaves it there', async ({ page }) => {
  await gotoApp(page, { tab: 'model' })
  const scroll = page.getByTestId('chain-scroll')
  const spot = octoberSection(page).getByTestId('spot-row')
  await expect(spot).toBeVisible()
  // The October monthly is the topmost section, so its spot line is the one
  // the tab opens on: in the middle third of the scroll container.
  await expect.poll(() => verticalPosition(spot, scroll)).toBeGreaterThan(1 / 3)
  await expect.poll(() => verticalPosition(spot, scroll)).toBeLessThan(2 / 3)
  // And not by luck: the container really is scrolled.
  expect(await scroll.evaluate((el) => el.scrollTop)).toBeGreaterThan(0)
  const centred = await scroll.evaluate((el) => el.scrollTop)

  // A refetch brings new chain data for the same section; the rows stay put.
  const refetched = page.waitForResponse(
    (response) => response.url().endsWith('/api/v1/chain') && response.request().method() === 'POST',
  )
  await page.getByRole('button', { name: '↻ refresh' }).click()
  await refetched
  // The section's own fetching spinner goes once the new chain is drawn.
  await expect(octoberSection(page).getByRole('status')).toHaveCount(0)
  expect(await scroll.evaluate((el) => el.scrollTop)).toBe(centred)
  const after = await verticalPosition(spot, scroll)
  expect(after).toBeGreaterThan(1 / 3)
  expect(after).toBeLessThan(2 / 3)

  // Scrolled away by hand, the "⌖ spot" action brings it back.
  await scroll.evaluate((el) => {
    el.scrollTop = 0
  })
  await octoberSection(page).getByRole('button', { name: /^Centre SPX · 16 Oct 2026 .* on spot$/ }).click()
  await expect.poll(() => verticalPosition(spot, scroll)).toBeGreaterThan(1 / 3)
  await expect.poll(() => verticalPosition(spot, scroll)).toBeLessThan(2 / 3)
})
