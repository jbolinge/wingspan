/**
 * The Model tab on the demo book (`serve --demo`, :8791): the option chain the
 * fixture lists from its own clock (2026-08-31), the strangle and the condor
 * counted in the Pos column, and a modeled leg carried from the chain to the
 * ticket, the tab badge and the risk graph — the Model tab's own graph view
 * and the one on Positions — and the ticket's legs written out as
 * thinkorswim orders and copied to the clipboard.
 *
 * Nothing is selected to begin with: an empty selection is "every open
 * trade" in the Model tab's Pos column, but on the graph, once a model
 * exists, it is the model alone.
 */

import { expect, test, type Locator, type Page } from '@playwright/test'
import type { ChainExpirationsResponse } from '../src/api/types'
import { DEMO_BASE_URL } from '../playwright.config'
import { DEMO_SPX_TRADE_IDS, collectConsoleErrors, gotoApp, positionsTable } from './helpers'

test.use({ baseURL: DEMO_BASE_URL })

/** U+2212, the Pos and Model columns' minus sign. */
const MINUS = '−'

/** The October monthly's chain section (SPX, AM-settled). */
function octoberSection(page: Page): Locator {
  return page
    .locator('section')
    .filter({ has: page.getByRole('heading', { name: /^SPX · 16 Oct 2026 · AM · \d+ DTE$/ }) })
}

/** The ticket's leg rows — scoped to their own list, because the Orders block
 *  under the greeks is a list in the same region. */
function modeledLegs(ticket: Locator): Locator {
  return ticket.getByRole('list', { name: 'Modeled legs' }).getByRole('listitem')
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

test('one chain is open at a time, and none is fine', async ({ page }) => {
  await gotoApp(page, { tab: 'model' })
  const strip = page.getByRole('toolbar', { name: 'Expirations' })
  const am = strip.getByRole('button', { name: /^16 OCT 26 · \d+d · AM, SPX, monthly/ })
  const sections = page.locator('section').filter({ has: page.getByRole('heading', { name: / DTE$/ }) })
  await expect(octoberSection(page)).toBeVisible()
  await expect(sections).toHaveCount(1)

  // Closing the open chip leaves no chain open — and nothing pressed.
  await am.click()
  await expect(page.getByText('No chain open')).toBeVisible()
  await expect(sections).toHaveCount(0)
  await expect(strip.getByRole('button', { pressed: true })).toHaveCount(0)

  // Picking another chip shows exactly that one section; the previous stays hidden.
  await strip.getByRole('button', { name: /^20 NOV 26 · \d+d · AM, SPX/ }).click()
  await expect(sections).toHaveCount(1)
  await expect(page.getByRole('heading', { name: /^SPX · 20 Nov 2026 · AM · \d+ DTE$/ })).toBeVisible()
  await expect(page.getByText('No chain open')).toHaveCount(0)
  await expect(strip.getByRole('button', { pressed: true })).toHaveCount(1)
  await expect(octoberSection(page)).toHaveCount(0)
})

test('the October section counts the strangle in Pos when nothing is selected', async ({ page }) => {
  await gotoApp(page, { tab: 'model' })
  await expect(page.getByRole('button', { name: 'all 3 open trades' })).toBeVisible()
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
  await expect(modeledLegs(ticket)).toHaveCount(1)
  await expect(modeledLegs(ticket)).toContainText('SPX 16 OCT 26 7500 P')
  await expect(ticket.getByText(/^net \$[\d,]+ debit$/)).toBeVisible()
  const modelTab = page.getByRole('navigation').getByRole('button', { name: 'Model 1', exact: true })
  await expect(modelTab).toBeVisible()

  // The Pos column still counts both trades, but the graph will not draw them.
  await expect(page.getByText('· the graph shows the model alone')).toBeVisible()

  // The Model tab's own graph view: the same panel, the model alone, with
  // the ticket — and its greeks — still beside it.
  const views = page.getByRole('group', { name: 'Model view' })
  await views.getByRole('button', { name: 'Risk graph' }).click()
  await expect(views.getByRole('button', { name: 'Risk graph' })).toHaveAttribute('aria-pressed', 'true')
  await expect(page.getByRole('toolbar', { name: 'Expirations' })).toHaveCount(0)
  await expect(page.getByTestId('risk-chart')).toBeVisible()
  await expect(page.getByTestId('risk-chart')).toHaveAttribute('aria-label', /^Risk graph for the model alone:/)
  await expect(ticket).toBeVisible()
  // At this viewport the ticket is the 18rem rail, where the table turns: a
  // row per greek, a column per line — here the model's alone.
  const greeks = ticket.getByRole('table', { name: 'Greeks' })
  await expect(greeks.getByRole('columnheader')).toHaveText(['Greek', 'Model'])
  await expect(greeks.getByRole('rowheader')).toHaveText(['Δ', 'Γ', 'Θ', 'V', 'W.V'])
  // Back to the chain: the strip and the October section, the leg still in it.
  await views.getByRole('button', { name: 'Chain' }).click()
  await expect(page.getByRole('toolbar', { name: 'Expirations' })).toBeVisible()
  await expect(cell).toHaveValue('+1')

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
  // …and the position as it stands behind it, by default: the real server
  // answers the third, model-free request, and its lines are listed.
  const compare = page.getByRole('button', { name: 'compare position' })
  await expect(compare).toBeEnabled()
  await expect(compare).toHaveAttribute('aria-pressed', 'true')
  await expect(page.getByText('without the model')).toBeVisible()
  await expect(page.getByText('T+0 (position)')).toBeVisible()
  await expect(page.getByTestId('risk-chart')).toHaveAttribute(
    'aria-label',
    /with the position without the model dashed behind$/,
  )
  // Off, the ghost family goes and the toggle remembers.
  await compare.click()
  await expect(page.getByText('T+0 (position)')).toHaveCount(0)
  await compare.click()
  await expect(page.getByText('T+0 (position)')).toBeVisible()
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

test('with trades selected, the ticket prices them, the model and both — what the graph draws', async ({
  page,
}) => {
  const consoleErrors = collectConsoleErrors(page)
  await gotoApp(page, { tab: 'model', selected: [...DEMO_SPX_TRADE_IDS] })
  const section = octoberSection(page)
  await expect(section).toBeVisible()
  await page.getByRole('group', { name: 'Strikes around spot' }).getByRole('button', { name: 'All' }).click()
  const cell = section.getByRole('spinbutton', { name: 'Model quantity, SPX 16 Oct 26 7500 put' })
  await cell.fill('-1')
  await cell.press('Enter')
  await expect(cell).toHaveValue(`${MINUS}1`)

  await page.getByRole('group', { name: 'Model view' }).getByRole('button', { name: 'Risk graph' }).click()
  await expect(page.getByTestId('risk-chart')).toHaveAttribute(
    'aria-label',
    /^Risk graph for 2 selected trades and the model:/,
  )
  await expect(page.getByText(/^2 trades \+ model · P&L/)).toBeVisible()

  // The real `portfolio` command, for real trade ids: three priced columns,
  // none stuck loading or refused.
  const greeks = page.getByRole('region', { name: 'Model ticket' }).getByRole('table', { name: 'Greeks' })
  await expect(greeks.getByRole('columnheader')).toHaveText(['Greek', 'Model', 'Position', 'Total'])
  await expect(greeks.getByRole('cell')).toHaveCount(15)
  await expect(greeks.getByRole('cell').filter({ hasText: /^(…|—)$/ })).toHaveCount(0)
  expect(consoleErrors).toEqual([])
})

test('clicking the ask buys one and the bid sells one, into the Model cell', async ({ page }) => {
  const consoleErrors = collectConsoleErrors(page)
  await gotoApp(page, { tab: 'model' })
  const section = octoberSection(page)
  await expect(section).toBeVisible()
  await page.getByRole('group', { name: 'Strikes around spot' }).getByRole('button', { name: 'All' }).click()
  const model = section.getByRole('spinbutton', { name: 'Model quantity, SPX 16 Oct 26 7500 put' })
  const ask = section.getByRole('button', { name: /^Buy one at \d+\.\d\d, SPX 16 Oct 26 7500 put$/ })
  const bid = section.getByRole('button', { name: /^Sell one at \d+\.\d\d, SPX 16 Oct 26 7500 put$/ })
  await expect(ask).toHaveText(/^\d+\.\d\d$/)
  await ask.click()
  await ask.click()
  await expect(model).toHaveValue('+2')
  await bid.click()
  await bid.click()
  await bid.click()
  await expect(model).toHaveValue(`${MINUS}1`)
  // The leg is a modeled leg like any other: on the ticket and the badge.
  const ticket = page.getByRole('region', { name: 'Model ticket' })
  await expect(modeledLegs(ticket)).toHaveCount(1)
  await expect(modeledLegs(ticket)).toContainText('SPX 16 OCT 26 7500 P')
  await expect(page.getByRole('navigation').getByRole('button', { name: 'Model 1', exact: true })).toBeVisible()
  // Typing still works on top of it, and a buy counts from what was typed.
  await model.fill('3')
  await ask.click()
  await expect(model).toHaveValue('+4')
  // Back through zero clears the leg.
  for (let i = 0; i < 4; i += 1) await bid.click()
  await expect(model).toHaveValue('')
  await expect(modeledLegs(ticket)).toHaveCount(0)
  expect(consoleErrors).toEqual([])
})

test('dragging a modeled quantity to another strike moves it, emptying the source', async ({ page }) => {
  const consoleErrors = collectConsoleErrors(page)
  await gotoApp(page, { tab: 'model' })
  const section = octoberSection(page)
  await expect(section).toBeVisible()
  await page.getByRole('group', { name: 'Strikes around spot' }).getByRole('button', { name: 'All' }).click()

  const source = section.getByRole('spinbutton', { name: 'Model quantity, SPX 16 Oct 26 7500 put' })
  const target = section.getByRole('spinbutton', { name: 'Model quantity, SPX 16 Oct 26 7400 put' })
  await source.fill('-1')
  await source.press('Enter')
  await expect(source).toHaveValue(`${MINUS}1`)

  // The browser's own text drag of the selected `−1`, which the cell tags as
  // a move: the quantity lands on 7400 and 7500 is blank again, so sliding a
  // short strike down the chain is one gesture rather than type-then-clear.
  await source.dragTo(target)
  await expect(target).toHaveValue(`${MINUS}1`)
  await expect(source).toHaveValue('')
  const ticket = page.getByRole('region', { name: 'Model ticket' })
  await expect(modeledLegs(ticket)).toHaveCount(1)
  await expect(modeledLegs(ticket)).toContainText('SPX 16 OCT 26 7400 P')
  await expect(page.getByRole('navigation').getByRole('button', { name: 'Model 1', exact: true })).toBeVisible()

  // A drop onto a strike that already holds a quantity replaces it.
  const third = section.getByRole('spinbutton', { name: 'Model quantity, SPX 16 Oct 26 7300 put' })
  await third.fill('2')
  await third.press('Enter')
  await target.dragTo(third)
  await expect(third).toHaveValue(`${MINUS}1`)
  await expect(target).toHaveValue('')
  await expect(modeledLegs(ticket)).toHaveCount(1)
  await expect(modeledLegs(ticket)).toContainText('SPX 16 OCT 26 7300 P')
  expect(consoleErrors).toEqual([])
})

test('the ticket writes the model as thinkorswim orders, and Copy / Copy all put them on the clipboard', async ({
  page,
  context,
}) => {
  const consoleErrors = collectConsoleErrors(page)
  // Headless Chromium keeps a real clipboard, but reading it back needs both
  // permissions on this origin.
  await context.grantPermissions(['clipboard-read', 'clipboard-write'], { origin: DEMO_BASE_URL })
  await gotoApp(page, { tab: 'model' })
  const section = octoberSection(page)
  await expect(section).toBeVisible()
  await page.getByRole('group', { name: 'Strikes around spot' }).getByRole('button', { name: 'All' }).click()

  // A 7800/7900 call spread and a 7600/7500 put spread, both sold — an iron
  // condor — and one long 7300 put that fits no spread with the rest.
  const legs: [number, 'call' | 'put', string][] = [
    [7800, 'call', '-1'],
    [7900, 'call', '1'],
    [7600, 'put', '-1'],
    [7500, 'put', '1'],
    [7300, 'put', '1'],
  ]
  for (const [strike, right, qty] of legs) {
    const cell = section.getByRole('spinbutton', { name: `Model quantity, SPX 16 Oct 26 ${strike} ${right}` })
    await cell.fill(qty)
    await cell.press('Enter')
    await expect(cell).toHaveValue(qty.startsWith('-') ? `${MINUS}1` : '+1')
  }

  const ticket = page.getByRole('region', { name: 'Model ticket' })
  await expect(modeledLegs(ticket)).toHaveCount(5)
  const orders = ticket.getByRole('group', { name: 'Orders' })
  await expect(orders.getByText('2 orders', { exact: true })).toBeVisible()
  // Until the universe sync has had the new strikes quoted they are priced
  // off a neighbour's vol, and the block says so; the lines are only final
  // once that note has gone.
  await expect(orders.getByText(/no quote/)).toHaveCount(0)

  // The real server's grouping: one IRON CONDOR line, sold for a credit at
  // the net mark, then the lone put as a single — each with its own button.
  const condor = orders.getByRole('button', { name: /^Copy order 1: / })
  const single = orders.getByRole('button', { name: /^Copy order 2: / })
  await expect(condor).toHaveAttribute(
    'aria-label',
    /^Copy order 1: SELL -1 IRON CONDOR SPX 100 16 OCT 26 \[AM\] 7800\/7900\/7600\/7500 CALL\/PUT @\d+\.\d\d LMT$/,
  )
  await expect(single).toHaveAttribute(
    'aria-label',
    /^Copy order 2: BUY \+1 SPX 100 16 OCT 26 \[AM\] 7300 PUT @\d+\.\d0 LMT$/,
  )
  await expect(orders.getByText('Iron condor', { exact: true })).toBeVisible()
  await expect(orders.getByText('Single', { exact: true })).toBeVisible()
  // The line is on screen too, whole in its title.
  const lineOf = async (button: Locator): Promise<string> =>
    ((await button.getAttribute('aria-label')) ?? '').replace(/^Copy order \d+: /, '')
  const condorLine = await lineOf(condor)
  await expect(orders.getByTitle(condorLine, { exact: true })).toHaveText(condorLine)

  // The fixture marks follow the clock, so every snapshot can move a limit
  // by a cent: each comparison reads the lines on screen as it clicks.
  const clipboard = (): Promise<string> => page.evaluate(() => navigator.clipboard.readText())

  // One order: exactly its line, no newline — what TOS's order entry takes.
  const singleLine = await lineOf(single)
  await single.click()
  await expect(orders.getByRole('status')).toHaveText('Copied 1 order')
  await expect(single).toHaveText('Copied')
  expect(await clipboard()).toBe(singleLine)

  // Every order: one per line, each newline-terminated, in the listed order.
  const lines = [await lineOf(condor), await lineOf(single)]
  await orders.getByRole('button', { name: 'Copy all orders' }).click()
  await expect(orders.getByRole('status')).toHaveText('Copied 2 orders')
  expect(await clipboard()).toBe(`${lines[0]}\n${lines[1]}\n`)

  // Clearing the model clears the orders with it.
  await page.getByRole('button', { name: 'Clear modeled trades' }).click()
  await expect(orders).toHaveCount(0)
  expect(consoleErrors).toEqual([])
})

/** The December 18 chain section as XSP (Mini-SPX, PM-settled). */
function xspDecemberSection(page: Page): Locator {
  return page
    .locator('section')
    .filter({ has: page.getByRole('heading', { name: /^XSP · 18 Dec 2026 · PM · \d+ DTE$/ }) })
}

test('the XSP switch models Mini-SPX: a chain at a tenth of SPX, the held hedge in Pos, an XSP order line', async ({
  page,
}) => {
  const consoleErrors = collectConsoleErrors(page)
  await gotoApp(page, { tab: 'model' })
  await expect(octoberSection(page)).toBeVisible()

  const underlying = page.getByRole('group', { name: 'Chain underlying' })
  await expect(underlying.getByRole('button', { name: 'SPX' })).toHaveAttribute('aria-pressed', 'true')
  await underlying.getByRole('button', { name: 'XSP' }).click()
  await expect(underlying.getByRole('button', { name: 'XSP' })).toHaveAttribute('aria-pressed', 'true')

  // The held XSP hedge opens its own expiration, as an XSP chain: spot is
  // SPX's 7750 ÷ 10, strikes are SPX's ÷ 10 (a 775 row, and 650 held).
  const section = xspDecemberSection(page)
  await expect(section).toBeVisible()
  await expect(octoberSection(page)).toHaveCount(0)
  await expect(section.getByTestId('spot-row')).toContainText('XSP 775.00')
  await expect(section.locator('tr[data-strike="775"]')).toBeVisible()
  await expect(section.locator('tr[data-strike="7750"]')).toHaveCount(0)
  await expect(posCell(section, 650, 'put')).toHaveText('+10')
  // XSP is always PM: no AM monthly on the strip.
  const strip = page.getByRole('toolbar', { name: 'Expirations' })
  await expect(strip.getByRole('button', { name: / · AM, / })).toHaveCount(0)

  // Model one XSP put: the ticket names it XSP, and so does the TOS order.
  const cell = section.getByRole('spinbutton', { name: 'Model quantity, XSP 18 Dec 26 760 put' })
  await cell.fill('1')
  await cell.press('Enter')
  await expect(cell).toHaveValue('+1')
  const ticket = page.getByRole('region', { name: 'Model ticket' })
  await expect(modeledLegs(ticket)).toHaveCount(1)
  await expect(modeledLegs(ticket)).toContainText('XSP 18 DEC 26 760 P')
  const orders = ticket.getByRole('group', { name: 'Orders' })
  await expect(orders.getByRole('button', { name: /^Copy order 1: BUY \+1 XSP 100 18 DEC 26 760 PUT @\d+\.\d+ LMT$/ })).toBeVisible()
  await expect(orders.getByRole('button', { name: /SPX/ })).toHaveCount(0)

  // Back to SPX: the XSP leg stays in the ticket (a leg keeps its own
  // underlying), the chain is SPX's again.
  await underlying.getByRole('button', { name: 'SPX' }).click()
  await expect(section.getByTestId('spot-row')).toHaveCount(0)
  await expect(page.getByTestId('chain-scroll').getByTestId('spot-row')).toContainText('SPX 7750.00')
  await expect(modeledLegs(ticket)).toHaveCount(1)
  await expect(modeledLegs(ticket)).toContainText('XSP 18 DEC 26 760 P')

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
  // The October monthly is the chain the tab opens, so its spot line is the
  // one it opens on: in the middle third of the scroll container.
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
