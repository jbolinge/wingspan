import '@testing-library/jest-dom/vitest'
import { cleanup } from '@testing-library/react'
import { afterEach, beforeEach, vi } from 'vitest'
import { resetPickedFile } from './src/lib/pickedFile'

/**
 * The wall clock every test starts on: Tuesday 2026-09-15, 12:00 New York.
 *
 * Without it every test that reads the browser clock read the real one, and
 * the suite rotted with the calendar — the mock book's fixed 2026 expiries
 * (now on `MOCK_NOW`, see `api/mock.ts`) went past expiry under it. A date of
 * its own, not the mock's 2026-08-31, so a component that wrongly reads the
 * browser's day instead of the server's shows up as a wrong date; and one on
 * which no `+Nd` offset a test uses lands on a book expiry. Only `Date` is
 * faked: timers stay real, so `waitFor` and the debounces behave as ever.
 *
 * Installed before each test rather than once, so a test that fakes timers
 * itself (`vi.useFakeTimers()` + `vi.setSystemTime`, in its own
 * `beforeEach`, which runs after this one) overrides it, and the
 * `vi.useRealTimers()` in its `afterEach` cannot leak the real clock into the
 * next test: that test re-pins here first.
 */
const TEST_NOW = new Date('2026-09-15T16:00:00Z')

beforeEach(() => {
  vi.useFakeTimers({ toFake: ['Date'] })
  vi.setSystemTime(TEST_NOW)
})

afterEach(() => {
  cleanup()
  // A module-level store outlives `cleanup()`; an unclaimed pick would
  // otherwise reopen the import dialog in the next test.
  resetPickedFile()
})

// jsdom implements neither of these; ECharts and the layout observers need them.
if (!('ResizeObserver' in globalThis)) {
  class ResizeObserverStub {
    observe() {}
    unobserve() {}
    disconnect() {}
  }
  ;(globalThis as unknown as { ResizeObserver: unknown }).ResizeObserver = ResizeObserverStub
}

// jsdom declares `matchMedia` but leaves it undefined, so test for a function,
// not for the property. Nothing matches: components render their narrow layout
// unless a test stubs a query to match.
if (typeof (globalThis as { matchMedia?: unknown }).matchMedia !== 'function') {
  ;(globalThis as unknown as { matchMedia: unknown }).matchMedia = (query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
    addListener: vi.fn(),
    removeListener: vi.fn(),
    dispatchEvent: vi.fn(),
  })
}

// EventSource is not in jsdom; components guard on its presence, but provide a
// no-op so the SSE hook can be exercised.
if (!('EventSource' in globalThis)) {
  class EventSourceStub {
    static readonly CONNECTING = 0
    static readonly OPEN = 1
    static readonly CLOSED = 2
    readyState = 0
    onerror: unknown = null
    onopen: unknown = null
    addEventListener() {}
    removeEventListener() {}
    close() {}
  }
  ;(globalThis as unknown as { EventSource: unknown }).EventSource = EventSourceStub
}

// The desktop transport. Every test drives these itself (`restoreMocks: true`
// wipes the implementation after each case, so re-seed them per test).
vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => {}) }))
