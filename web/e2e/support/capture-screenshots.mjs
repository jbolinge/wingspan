#!/usr/bin/env node
/**
 * README screenshots: docs/screenshots/dashboard.png, risk-graph.png and
 * model.png.
 *
 * All are 1440×900, dark theme, showing the synthetic demo book. These are
 * Chromium captures of the SPA served by `wingspan-devserver serve --demo`
 * (the same demo-book devserver the Playwright suite runs on :8791), not the
 * native desktop window. Re-run with `just screenshots`.
 */

import { spawn } from 'node:child_process'
import { mkdir } from 'node:fs/promises'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { chromium } from '@playwright/test'

const HERE = dirname(fileURLToPath(import.meta.url))
const ROOT = resolve(HERE, '../../..')
const BASE_URL = process.env.WINGSPAN_URL ?? 'http://127.0.0.1:8791'
const OUT = resolve(ROOT, 'docs/screenshots')
const VIEWPORT = { width: 1440, height: 900 }

const SEED = {
  state: {
    theme: 'dark',
    tab: 'positions',
    selected: ['oct-strangle-2026-08-24', 'nov-condor-2026-08-26'],
    sortKey: 'opened_on',
    sortDir: 'asc',
    expanded: [],
  },
  // The store's persist version (`src/store/appStore.ts`).
  version: 4,
}

/** A contract the way the store keeps a modeled leg's (the frozen Leg shape). */
function spxPut(strike) {
  return {
    underlying: 'SPX',
    root: 'SPX',
    expiry: '2026-10-16',
    settlement: 'am',
    strike,
    right: 'put',
    multiplier: 100,
    osi: `SPX   261016P0${strike}000`,
    tos_code: `SPX261016P${strike}`,
  }
}

/** The Model tab with the demo book selected (both trades in Pos) and a put
 *  spread modeled under the strangle's short put — so the ticket's greeks
 *  table shows all three lines: Model, Position and their total. */
const MODEL_SEED = {
  state: {
    ...SEED.state,
    tab: 'model',
    model: {
      legs: [
        { contract: spxPut(7600), qty: -1 },
        { contract: spxPut(7500), qty: 1 },
      ],
      include: true,
      compare: false,
      expiry: 'auto',
      strikeCount: 32,
      view: 'chain',
    },
  },
  version: 4,
}

async function healthy() {
  try {
    const response = await fetch(`${BASE_URL}/api/v1/health`)
    return response.ok
  } catch {
    return false
  }
}

async function waitForServer(timeoutMs = 300_000) {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    if (await healthy()) return
    await new Promise((r) => setTimeout(r, 500))
  }
  throw new Error(`no wingspan-devserver at ${BASE_URL}`)
}

/**
 * The demo market's own instant. `serve --demo` pins its valuation clock to
 * the fixtures' day, but the browser runs on the real one — and the SPA
 * labels the T+x lines and the data-age pills against the browser's today,
 * so a capture taken weeks later would read "T+-20" and "31.0d". Freezing
 * the page's `Date` at the snapshot's `as_of` makes every capture look the
 * way the demo does on its own day, whenever the script is run.
 */
async function demoNow() {
  const response = await fetch(`${BASE_URL}/api/v1/market/snapshot`)
  if (!response.ok) throw new Error(`market snapshot: HTTP ${response.status}`)
  const { as_of: asOf } = await response.json()
  return new Date(asOf)
}

/** A browser context at `viewport`, seeded with `seed` and frozen at `now`. */
async function seededContext(browser, viewport, seed, now) {
  const context = await browser.newContext({ viewport, deviceScaleFactor: 2 })
  await context.clock.setFixedTime(now)
  await context.addInitScript((value) => window.localStorage.setItem('wingspan.ui', value), JSON.stringify(seed))
  return context
}

async function main() {
  await mkdir(OUT, { recursive: true })

  let server = null
  if (await healthy()) {
    console.log(`screenshots: reusing the server at ${BASE_URL}`)
  } else {
    console.log('screenshots: starting `wingspan-devserver serve --demo`')
    server = spawn(resolve(HERE, 'serve-demo.sh'), { stdio: 'inherit', detached: true })
    await waitForServer()
  }

  const now = await demoNow()
  const browser = await chromium.launch({
    args: ['--font-render-hinting=none', '--disable-lcd-text'],
  })
  try {
    // 1. The dashboard, exactly as a 1440×900 window shows it.
    const dashboard = await seededContext(browser, VIEWPORT, SEED, now)
    const page = await dashboard.newPage()
    await page.goto(BASE_URL)
    await page.getByTestId('risk-chart').waitFor()
    await page.waitForTimeout(1500)
    await page.screenshot({ path: resolve(OUT, 'dashboard.png') })
    await dashboard.close()

    // 2. The risk graph. The panel scrolls at 900px, so the window is taller
    //    and the 1440×900 frame is clipped around the graph itself.
    const graphContext = await seededContext(browser, { width: VIEWPORT.width, height: 1180 }, SEED, now)
    const graphPage = await graphContext.newPage()
    await graphPage.goto(BASE_URL)
    const chart = graphPage.getByTestId('risk-chart')
    await chart.waitFor()
    await graphPage.waitForTimeout(1500)

    // Park the crosshair on the spot so the readout is populated.
    const box = await chart.boundingBox()
    await graphPage.mouse.move(box.x + box.width * 0.55, box.y + box.height * 0.5)
    await graphPage.waitForTimeout(400)

    const top = Math.min(Math.max(0, box.y - 60), 1180 - VIEWPORT.height)
    await graphPage.screenshot({
      path: resolve(OUT, 'risk-graph.png'),
      clip: { x: 0, y: top, ...VIEWPORT },
    })
    await graphContext.close()

    // 3. The Model tab: the chain the position holds, the Pos column
    //    and the ticket.
    const modelContext = await seededContext(browser, VIEWPORT, MODEL_SEED, now)
    const modelPage = await modelContext.newPage()
    await modelPage.goto(BASE_URL)
    await modelPage.getByRole('region', { name: 'Model ticket' }).getByRole('list', { name: 'Modeled legs' }).getByRole('listitem').first().waitFor()
    await modelPage.locator('tr[data-strike]').first().waitFor()
    await modelPage.waitForTimeout(1500)
    await modelPage.screenshot({ path: resolve(OUT, 'model.png') })
    await modelContext.close()
  } finally {
    await browser.close()
    if (server) process.kill(-server.pid)
  }
  console.log(`screenshots: wrote ${OUT}/dashboard.png, ${OUT}/risk-graph.png and ${OUT}/model.png`)
}

await main()
