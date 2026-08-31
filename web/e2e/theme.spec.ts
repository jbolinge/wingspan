/** Plan §5/§7: the dark trading-terminal theme with a light toggle that
 *  survives a reload (and reaches the chart, not just the CSS). */

import { expect, test } from '@playwright/test'
import { positionsTable, STORE_KEY } from './helpers'

const root = (page: import('@playwright/test').Page) => page.locator('html')

async function background(page: import('@playwright/test').Page): Promise<string> {
  return page.evaluate(() => getComputedStyle(document.body).backgroundColor)
}

test('the theme toggle switches dark/light and persists across a reload', async ({ page }) => {
  // No seeding here: `seedUi` re-runs on every navigation, which would hide
  // the very thing under test. Start from a virgin localStorage and click.
  await page.goto('/')
  await expect(page.getByRole('banner').getByText('DEMO · fixture')).toBeVisible()
  await positionsTable(page).getByRole('checkbox', { name: 'Select M1', exact: true }).check()

  await expect(root(page)).toHaveAttribute('data-theme', 'dark')
  const dark = await background(page)
  const chart = page.getByTestId('risk-chart')
  await expect(chart).toBeVisible()
  const darkChart = await chart.screenshot()

  const toggle = page.getByRole('button', { name: 'Switch to the light theme' })
  await expect(toggle).toHaveText('☾ dark')
  await toggle.click()

  await expect(root(page)).toHaveAttribute('data-theme', 'light')
  await expect(page.getByRole('button', { name: 'Switch to the dark theme' })).toHaveText('☀ light')
  const light = await background(page)
  expect(light).not.toBe(dark)

  // The chart palette is theme-aware too, so the canvas must repaint.
  await expect
    .poll(async () => (await chart.screenshot()).equals(darkChart), {
      message: 'the risk chart should repaint in the light palette',
    })
    .toBe(false)

  // Persisted…
  expect(
    await page.evaluate(
      (key) => JSON.parse(window.localStorage.getItem(key) ?? '{}')?.state?.theme,
      STORE_KEY,
    ),
  ).toBe('light')

  // …and applied before first paint on the way back in (index.html reads the
  // same key), so there is no dark flash.
  await page.reload()
  await expect(root(page)).toHaveAttribute('data-theme', 'light')
  expect(await background(page)).toBe(light)

  await page.getByRole('button', { name: 'Switch to the dark theme' }).click()
  await expect(root(page)).toHaveAttribute('data-theme', 'dark')
  expect(await background(page)).toBe(dark)
})
