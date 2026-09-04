/**
 * The window opens and the demo book's positions render.
 *
 * The same wholly synthetic strangle and condor `web/e2e/demo-book.spec.ts`
 * exercises against the dev server, this time rendered by the real shipped
 * binary in a real WebKitGTK window — which is the one thing no vitest or
 * Playwright run can tell you.
 */
import { expect } from '@wdio/globals'
import { loadDemoBook, tradeNames } from './helpers.ts'

describe('the shipped binary opens its window', () => {
  it('renders the demo book in the positions table', async () => {
    await loadDemoBook()

    const names = await tradeNames()
    expect(names.length).toBeGreaterThanOrEqual(2)
    expect(names).toContain('Oct Strangle')
    expect(names).toContain('Nov Condor')
  })
})
