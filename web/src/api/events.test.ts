/**
 * The desktop event tape.
 *
 * The Tauri branch has no `open` event to key the status dot off, so it goes
 * live the moment the subscriptions are registered and stays there on the
 * heartbeat; 45 s of silence means the market loop has stopped. `app_ready` is
 * belt-and-braces — arriving before (or never) must cost nothing.
 */

import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { listen } from '@tauri-apps/api/event'
import { act, renderHook } from '@testing-library/react'
import { createElement, type ReactNode } from 'react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { setMockMode } from './client'
import { useBookEpoch } from './epoch'
import { useEventStream } from './events'
import { queryKeys } from './queries'

type Handler = (event: { payload: unknown }) => void

const handlers = new Map<string, Handler>()
const unlisten = vi.fn()
/** Events whose `listen()` promise is left hanging for the test. */
let pending: string[] = []

function harness(): { client: QueryClient; wrapper: ({ children }: { children: ReactNode }) => ReactNode } {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  const wrapper = ({ children }: { children: ReactNode }) =>
    createElement(QueryClientProvider, { client }, children)
  return { client, wrapper }
}

/** Let the `listen()` promises (and the `Promise.all` over them) settle. */
async function settle(): Promise<void> {
  for (let i = 0; i < 6; i += 1) {
    await act(async () => {
      await Promise.resolve()
    })
  }
}

beforeEach(() => {
  setMockMode('off')
  ;(window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {}
  handlers.clear()
  pending = []
  unlisten.mockReset()
  vi.mocked(listen).mockReset()
  vi.mocked(listen).mockImplementation((event: string, handler: unknown) => {
    handlers.set(event, handler as Handler)
    if (pending.includes(event)) return new Promise(() => {})
    return Promise.resolve(unlisten)
  })
  vi.useFakeTimers()
})

afterEach(() => {
  vi.useRealTimers()
  delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__
  setMockMode('forced')
})

describe('useEventStream inside Tauri', () => {
  it('the dot goes live as soon as the subscriptions resolve', async () => {
    const { wrapper } = harness()
    const { result } = renderHook(() => useEventStream(), { wrapper })
    expect(result.current).toBe('connecting')

    await settle()

    // No heartbeat has arrived; the dot is live all the same.
    expect(result.current).toBe('open')
    expect(handlers.has('snapshot')).toBe(true)
    expect(handlers.has('provider')).toBe(true)
    expect(handlers.has('trades_changed')).toBe(true)
  })

  it('45 s of silence closes the dot, and the next heartbeat reopens it', async () => {
    const { wrapper } = harness()
    const { result } = renderHook(() => useEventStream(), { wrapper })
    await settle()
    expect(result.current).toBe('open')

    act(() => {
      vi.advanceTimersByTime(45_000)
    })
    expect(result.current).toBe('closed')

    act(() => {
      handlers.get('heartbeat')?.({ payload: {} })
    })
    expect(result.current).toBe('open')
  })

  it('app_ready invalidates every query', async () => {
    const { client, wrapper } = harness()
    const invalidate = vi.spyOn(client, 'invalidateQueries')
    renderHook(() => useEventStream(), { wrapper })
    await settle()
    invalidate.mockClear()

    act(() => {
      handlers.get('app_ready')?.({ payload: undefined })
    })

    expect(invalidate).toHaveBeenCalledWith()
  })

  it('a snapshot marks the option chain stale, and only the chain', async () => {
    const { client, wrapper } = harness()
    const invalidate = vi.spyOn(client, 'invalidateQueries')
    renderHook(() => useEventStream(), { wrapper })
    await settle()
    invalidate.mockClear()

    act(() => {
      handlers.get('snapshot')?.({ payload: { spot: { last: 7750 } } })
    })

    expect(client.getQueryData(queryKeys.snapshot)).toEqual({ spot: { last: 7750 } })
    expect(invalidate.mock.calls.map(([filters]) => filters?.queryKey)).toEqual([
      ['chain-expirations'],
      ['option-chain'],
    ])
  })

  it('app_ready and trades_changed move the book epoch, so the model is re-registered', async () => {
    const { wrapper } = harness()
    renderHook(() => useEventStream(), { wrapper })
    await settle()
    const before = useBookEpoch.getState().epoch

    act(() => {
      handlers.get('app_ready')?.({ payload: undefined })
    })
    expect(useBookEpoch.getState().epoch).toBe(before + 1)
    act(() => {
      handlers.get('trades_changed')?.({ payload: { reason: 'reload' } })
    })
    expect(useBookEpoch.getState().epoch).toBe(before + 2)
  })

  it('a ready that arrives before listen registered is harmless', async () => {
    // Tauri does not replay events: the `app_ready` subscription never lands
    // here at all, and the tape is live regardless.
    pending = ['app_ready']
    const { wrapper } = harness()
    const { result } = renderHook(() => useEventStream(), { wrapper })

    await settle()

    expect(result.current).toBe('open')
    act(() => {
      handlers.get('snapshot')?.({ payload: { spot: 5400 } })
    })
    expect(result.current).toBe('open')
  })

  it('tearing down unlistens everything and stops the silence timer', async () => {
    const { wrapper } = harness()
    const { unmount } = renderHook(() => useEventStream(), { wrapper })
    await settle()

    unmount()

    // snapshot, provider, trades_changed, heartbeat, app_ready, app_error
    expect(unlisten).toHaveBeenCalledTimes(6)
    expect(vi.getTimerCount()).toBe(0)
  })
})
