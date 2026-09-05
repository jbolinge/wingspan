#!/usr/bin/env node
/**
 * README screenshots: docs/screenshots/dashboard.png and risk-graph.png.
 *
 * Both are 1440×900, dark theme, showing the synthetic demo book. These are
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
  version: 2,
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

  const browser = await chromium.launch({
    args: ['--font-render-hinting=none', '--disable-lcd-text'],
  })
  try {
    // 1. The dashboard, exactly as a 1440×900 window shows it.
    const dashboard = await browser.newContext({ viewport: VIEWPORT, deviceScaleFactor: 2 })
    await dashboard.addInitScript(
      (seed) => window.localStorage.setItem('wingspan.ui', seed),
      JSON.stringify(SEED),
    )
    const page = await dashboard.newPage()
    await page.goto(BASE_URL)
    await page.getByTestId('risk-chart').waitFor()
    await page.waitForTimeout(1500)
    await page.screenshot({ path: resolve(OUT, 'dashboard.png') })
    await dashboard.close()

    // 2. The risk graph. The panel scrolls at 900px, so the window is taller
    //    and the 1440×900 frame is clipped around the graph itself.
    const graphContext = await browser.newContext({
      viewport: { width: VIEWPORT.width, height: 1180 },
      deviceScaleFactor: 2,
    })
    await graphContext.addInitScript(
      (seed) => window.localStorage.setItem('wingspan.ui', seed),
      JSON.stringify(SEED),
    )
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
  } finally {
    await browser.close()
    if (server) process.kill(-server.pid)
  }
  console.log(`screenshots: wrote ${OUT}/dashboard.png and ${OUT}/risk-graph.png`)
}

await main()
