import '@testing-library/jest-dom/vitest'
import { cleanup } from '@testing-library/react'
import { afterEach, vi } from 'vitest'

afterEach(() => {
  cleanup()
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

if (!('matchMedia' in globalThis)) {
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

// jsdom has no `crypto.randomUUID`; TanStack Query and the dialogs reach for
// it through `useId`-adjacent code paths.
if (!('crypto' in globalThis)) {
  ;(globalThis as unknown as { crypto: unknown }).crypto = {
    randomUUID: () => `00000000-0000-4000-8000-${Math.random().toString(16).slice(2, 14).padEnd(12, '0')}`,
  }
}

// The desktop transport. Every test drives these itself (`restoreMocks: true`
// wipes the implementation after each case, so re-seed them per test).
vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => {}) }))
