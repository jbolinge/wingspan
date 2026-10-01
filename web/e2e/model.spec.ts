/**
 * The Model tab on the demo book (`serve --demo`, :8791): the option chain the
 * fixture lists from its own clock (2026-08-31), the strangle and the condor
 * counted in the Pos column, and a modeled leg carried from the chain to the
 * ticket, the tab badge and the risk graph on Positions.
 *
 * Nothing is selected in any of these: an empty selection is "every open
 * trade" on the Model tab, and — once a model exists — on the graph too.
 */

import { expect, test, type Locator, type Page } from '@playwright/test'
import type { ChainExpirationsResponse } from '../src/api/types'
import { DEMO_BASE_URL } from '../playwright.config'
import { collectConsoleErrors, gotoApp } from './helpers'

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

  // Positions: the graph draws the whole book plus the model, and says so.
  await page.getByRole('navigation').getByRole('button', { name: 'Positions', exact: true }).click()
  await expect(page.getByText('Model · 1 leg')).toBeVisible()
  await expect(page.getByTestId('risk-chart')).toBeVisible()
  await expect(page.getByTestId('risk-chart')).toHaveAttribute(
    'aria-label',
    /^Risk graph for 2 open trades and the model:/,
  )

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
