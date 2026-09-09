/** The dashboard's room-making controls: the open-positions card folds to
 *  its header, the bar under it sizes the table, and the risk graph can take
 *  the whole tab — each remembered (or, for maximize, deliberately not). */

import { expect, test } from '@playwright/test'
import { DEMO_TRADE_IDS, gotoApp, positionsTable, savedUi } from './helpers'

test('the open-positions card folds to its header and the graph takes the room', async ({
  page,
}) => {
  await gotoApp(page, { selected: [...DEMO_TRADE_IDS] })
  const chart = page.getByTestId('risk-chart')
  await expect(chart).toBeVisible()
  const before = (await chart.boundingBox())!.height

  await page.getByRole('button', { name: 'Collapse Open positions' }).click()
  await expect(positionsTable(page)).toHaveCount(0)
  await expect(page.getByText('6 open · 6 selected')).toBeVisible()
  await expect(page.getByRole('separator')).toHaveCount(0)
  await expect.poll(async () => (await chart.boundingBox())!.height).toBeGreaterThan(before)
  expect((await savedUi(page)).positionsCollapsed).toBe(true)

  await page.getByRole('button', { name: 'Expand Open positions' }).click()
  await expect(positionsTable(page)).toBeVisible()
  await expect(page.getByRole('separator', { name: 'Resize the open positions table' })).toBeVisible()
})

test('the drag bar sizes the table and double-click resets it', async ({ page }) => {
  await gotoApp(page)
  const card = page.locator('section').filter({ has: page.getByRole('heading', { name: 'Open positions' }) })
  const handle = page.getByRole('separator', { name: 'Resize the open positions table' })
  const start = (await handle.boundingBox())!
  const cardBefore = (await card.boundingBox())!.height

  await page.mouse.move(start.x + start.width / 2, start.y + start.height / 2)
  await page.mouse.down()
  await page.mouse.move(start.x + start.width / 2, start.y + start.height / 2 - 60, { steps: 6 })
  await page.mouse.up()

  await expect.poll(async () => (await card.boundingBox())!.height).toBeLessThan(cardBefore - 40)
  const saved = (await savedUi(page)).positionsHeight
  expect(typeof saved).toBe('number')
  await expect(handle).toHaveAttribute('aria-valuenow', String(saved))
  // The table scrolls inside the shorter card rather than being cut off.
  await expect(positionsTable(page)).toBeVisible()

  await handle.dblclick()
  await expect.poll(async () => (await savedUi(page)).positionsHeight).toBeNull()
  await expect.poll(async () => (await card.boundingBox())!.height).toBeGreaterThan(cardBefore - 5)
})

test('maximize gives the graph the tab; Esc brings the dashboard back', async ({ page }) => {
  await gotoApp(page, { selected: [...DEMO_TRADE_IDS] })
  const chart = page.getByTestId('risk-chart')
  await expect(chart).toBeVisible()
  const before = (await chart.boundingBox())!.height

  await page.getByRole('button', { name: 'maximize', exact: true }).click()
  await expect(page.getByRole('heading', { name: 'Open positions' })).toHaveCount(0)
  await expect(page.getByText('All open trades')).toHaveCount(0)
  await expect(page.getByRole('button', { name: 'restore', exact: true })).toHaveAttribute(
    'aria-pressed',
    'true',
  )
  await expect.poll(async () => (await chart.boundingBox())!.height).toBeGreaterThan(before + 100)
  // A relaunch must open on the whole dashboard, so this one is not saved.
  expect(await savedUi(page)).not.toHaveProperty('graphMaximized')

  await page.keyboard.press('Escape')
  await expect(page.getByRole('heading', { name: 'Open positions' })).toBeVisible()
  await expect(page.getByText('All open trades')).toBeVisible()
})
