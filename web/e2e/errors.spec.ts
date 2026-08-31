/** The SPA's error path against the real server: `application/problem+json`.
 *
 *  The dev mock cannot produce these, so this drives the one control that can
 *  make the API refuse — an absolute price range whose bounds are crossed —
 *  and checks the problem document's `detail` reaches the screen intact. */

import { expect, test } from '@playwright/test'
import { DEMO_TRADE_IDS, gotoApp } from './helpers'

test('a rejected risk-graph request renders the problem+json detail', async ({ page, request }) => {
  // What the server says, verbatim.
  const rejected = await request.post('/api/v1/risk-graph', {
    data: {
      trade_ids: [...DEMO_TRADE_IDS],
      price_range: { min: 8000, max: 7000 },
      points: 241,
      dates: null,
      vol_shift: 0,
      vol_shift_by_expiry: {},
      spot_override: null,
      include_realized: true,
    },
  })
  expect(rejected.status()).toBe(400)
  expect(rejected.headers()['content-type']).toContain('application/problem+json')
  const problem = await rejected.json()
  expect(problem).toMatchObject({
    title: 'Bad Request',
    status: 400,
    detail: 'invalid price range [8000, 7000]',
  })

  await gotoApp(page, { selected: [...DEMO_TRADE_IDS] })
  await expect(page.getByTestId('risk-chart')).toBeVisible()

  // Drive the same request through the price-range controls.
  await page.getByRole('button', { name: 'abs', exact: true }).click()
  await page.getByLabel('min', { exact: true }).fill('8000')
  await page.getByLabel('min', { exact: true }).press('Enter')
  await page.getByLabel('max', { exact: true }).fill('7000')
  await page.getByLabel('max', { exact: true }).press('Enter')

  const alert = page.getByRole('alert')
  await expect(alert).toBeVisible()
  await expect(alert).toContainText('Request failed.')
  await expect(alert).toContainText(problem.detail)
  await expect(page.getByTestId('risk-chart')).toHaveCount(0)

  // Retrying a deterministic rejection keeps saying the same thing…
  await alert.getByRole('button', { name: 'Retry' }).click()
  await expect(alert).toContainText(problem.detail)

  // …and correcting the bounds brings the curves back.
  await page.getByLabel('min', { exact: true }).fill('7000')
  await page.getByLabel('min', { exact: true }).press('Enter')
  await page.getByLabel('max', { exact: true }).fill('8500')
  await page.getByLabel('max', { exact: true }).press('Enter')
  await expect(page.getByRole('alert')).toHaveCount(0)
  await expect(page.getByTestId('risk-chart')).toBeVisible()
})
