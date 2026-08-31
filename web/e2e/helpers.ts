/**
 * Shared e2e plumbing: seeding the persisted UI state, locating the parts of
 * the dashboard by their accessible names, and reading the real API alongside
 * the browser so assertions compare the SPA against the server that fed it.
 */

import { expect, type Locator, type Page } from '@playwright/test'
import type { GraphSettings, Tab, Theme } from '../src/store/appStore'

/** The `localStorage` key `index.html` reads before first paint. */
export const STORE_KEY = 'wingspan.ui'

export interface SeedState {
  theme?: Theme
  tab?: Tab
  selected?: string[]
  expanded?: string[]
  graph?: Partial<GraphSettings>
}

/** Every open trade in the demo fixtures, in file order. */
export const DEMO_TRADE_IDS = [
  'bsh-2026-07-14',
  'm1-2026-07-14',
  'm2-2026-07-21',
  'm3-2026-07-28',
  'm4-2026-08-04',
  'm5-2026-08-15',
] as const

/**
 * Writes the zustand-persist blob before the app boots, so a test can start
 * from a chosen selection/tab/theme instead of clicking its way there.
 *
 * Init scripts run on *every* navigation, so a seeded value is re-applied by
 * `page.reload()`: a test about persistence must not seed.
 */
export async function seedUi(page: Page, state: SeedState): Promise<void> {
  await page.addInitScript(
    ([key, value]: [string, string]) => window.localStorage.setItem(key, value),
    [STORE_KEY, JSON.stringify({ state, version: 1 })] as [string, string],
  )
}

/** Seed, open the dashboard and wait for the status bar to have real data. */
export async function gotoApp(page: Page, state: SeedState = {}): Promise<void> {
  await seedUi(page, state)
  await page.goto('/')
  await expect(page.getByRole('banner').getByText('DEMO · fixture')).toBeVisible()
}

export function positionsTable(page: Page): Locator {
  return page.getByRole('table', {
    name: 'Open option trades with P&L and position greeks',
  })
}

/** The row for one open trade, found by its select checkbox. */
export function tradeRow(page: Page, name: string): Locator {
  return positionsTable(page)
    .getByRole('row')
    .filter({ has: page.getByRole('checkbox', { name: `Select ${name}`, exact: true }) })
}

/** Column indices of the positions table, checkbox and expander included. */
export const COL = {
  name: 2,
  opened: 3,
  dit: 4,
  dte: 5,
  structure: 6,
  cost: 7,
  value: 8,
  pnlOpen: 9,
  pnlTotal: 10,
  pnlPct: 11,
  delta: 12,
  gamma: 13,
  theta: 14,
  vega: 15,
  weightedVega: 16,
  staleness: 17,
  warnings: 18,
} as const

export function cell(row: Locator, column: number): Locator {
  return row.getByRole('cell').nth(column)
}

/** The trade rows, excluding the leg tables nested inside expanded rows. */
export function tradeRows(page: Page): Locator {
  return positionsTable(page).locator(':scope > tbody > tr')
}

/**
 * The value next to a label in a stat block (portfolio cards, graph readouts,
 * what-if greeks): every one of them renders `<span>label</span><span>value
 * </span>`. Matching on the label's own text node survives the ⓘ affordance
 * and every Tailwind class change.
 */
export function statValue(scope: Locator, label: string): Locator {
  return scope.locator(
    `xpath=.//span[normalize-space(text())=${xpathLiteral(label)}]/../span[2]`,
  )
}

/** The risk-graph header readouts (max profit, max loss, PoP, ATM IV, BE);
 *  each label appears once on the page. */
export function graphReadout(page: Page, label: string): Locator {
  return statValue(page.locator('body'), label)
}

/** A card/section located by its heading. */
export function sectionByHeading(page: Page, heading: string): Locator {
  return page
    .locator('section')
    .filter({ has: page.getByRole('heading', { name: heading, exact: true }) })
}

/** XPath has no escape character; a literal with quotes needs concat(). */
export function xpathLiteral(value: string): string {
  if (!value.includes("'")) return `'${value}'`
  if (!value.includes('"')) return `"${value}"`
  return `concat('${value.split("'").join(`', "'", '`)}')`
}

/** Collects browser-side console errors so a test can assert there were none. */
export function collectConsoleErrors(page: Page): string[] {
  const errors: string[] = []
  page.on('console', (message) => {
    if (message.type() === 'error') errors.push(message.text())
  })
  page.on('pageerror', (error) => errors.push(error.message))
  return errors
}

/**
 * Graph settings for the screenshot baseline: an absolute price range and a
 * locked y-axis fix the geometry, and σ bands + breakeven labels are off
 * because both are driven by the front ATM IV, which moves as the fixture's
 * time-to-expiry shrinks. What is left — the curve shapes, the ladder colours,
 * the zero line and the spot marker — is what the diff should be about.
 */
export const PINNED_GRAPH: Partial<GraphSettings> = {
  rangeMode: 'abs',
  rangeMin: 6000,
  rangeMax: 9000,
  points: 241,
  yLock: true,
  yMin: -18000,
  yMax: 16000,
  showSigmaBands: false,
  showBreakevens: false,
  showSliceTable: false,
  perTradeOverlay: false,
}

/** `-$1,234.50` / `+12.5%` / `1,743.4` -> a number. */
export function parseNumeric(text: string): number {
  return Number(text.replace(/[$,%\s+]/g, '').replace(/−/g, '-'))
}

/**
 * Asserts a rendered number is within `tolerance` of the value the API
 * returned. The demo surface is priced from `now`, so a mark read over HTTP a
 * second before the page rendered is close, never equal.
 */
export async function expectNumberNear(
  locator: Locator,
  expected: number,
  tolerance: number,
  label: string,
): Promise<void> {
  await expect
    .poll(
      async () => {
        const actual = parseNumeric(await locator.innerText())
        return Math.abs(actual - expected) <= tolerance ? expected : actual
      },
      { message: `${label} should render within ${tolerance} of ${expected}` },
    )
    .toBe(expected)
}
