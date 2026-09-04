/**
 * R14: the shipped app must never show the SPA's DEV-mock latch.
 *
 * `StatusBar.tsx` renders a `mock` / `mock fallback` pill whenever
 * `getMockMode()` is anything but `'off'` — meaning `VITE_MOCK=1` was baked
 * into the bundle, or the SPA's own calls failed and it silently fell back to
 * `src/api/mock.ts`'s *fabricated* positions. Either would put invented
 * numbers in front of someone about to trade on them, so the word must not
 * appear in the status bar of a release build at all.
 *
 * Asserted with a book actually loaded (the synthetic demo book, which is
 * honestly labelled `DEMO · fixture` and is not the mock layer), because a
 * status bar with nothing in it proves much less.
 */
import { browser, expect } from '@wdio/globals'
import { loadDemoBook } from './helpers.ts'

describe('the status bar', () => {
  it('has no mock pill', async () => {
    await loadDemoBook()

    const header = await browser.$('header')
    const text = (await header.getText()).toLowerCase()
    expect(text.length).toBeGreaterThan(0)
    expect(text).not.toContain('mock')
  })
})
