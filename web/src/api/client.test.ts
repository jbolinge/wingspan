/**
 * The risk-graph query forwards TanStack's AbortSignal through `apiPost` so a
 * superseded request is actually cancelled — these pin that wiring and make
 * sure a cancelled request doesn't get mistaken for the API being down.
 */

import { afterEach, describe, expect, it, vi } from 'vitest'
import { apiPost, apiPut, getMockMode, setMockMode } from './client'

describe('apiPost signal forwarding', () => {
  afterEach(() => {
    vi.unstubAllGlobals()
    setMockMode('forced')
  })

  it('passes the given AbortSignal through to fetch', () => {
    setMockMode('off')
    const fetchSpy = vi.fn((_input: RequestInfo | URL, _init?: RequestInit) => new Promise<Response>(() => {}))
    vi.stubGlobal('fetch', fetchSpy)

    const controller = new AbortController()
    void apiPost('/risk-graph', { trade_ids: [] }, controller.signal)

    expect(fetchSpy).toHaveBeenCalledTimes(1)
    const init = fetchSpy.mock.calls[0]?.[1]
    expect(init?.signal).toBe(controller.signal)
  })

  it('rejects with the abort error instead of latching into mock fallback', async () => {
    setMockMode('off')
    const controller = new AbortController()
    vi.stubGlobal(
      'fetch',
      vi.fn(
        () =>
          new Promise<Response>((_resolve, reject) => {
            controller.signal.addEventListener('abort', () =>
              reject(new DOMException('aborted', 'AbortError')),
            )
          }),
      ),
    )

    const promise = apiPost('/risk-graph', { trade_ids: [] }, controller.signal)
    controller.abort()

    await expect(promise).rejects.toMatchObject({ name: 'AbortError' })
    // A real network error would flip DEV mode into the mock fallback; a
    // cancelled-on-purpose request must not.
    expect(getMockMode()).toBe('off')
  })

  it('apiPut sends a PUT with a JSON body', () => {
    setMockMode('off')
    const fetchSpy = vi.fn((_input: RequestInfo | URL, _init?: RequestInit) => new Promise<Response>(() => {}))
    vi.stubGlobal('fetch', fetchSpy)

    void apiPut('/trades/root', { root: '/home/trader/Trades' })

    expect(fetchSpy).toHaveBeenCalledTimes(1)
    const init = fetchSpy.mock.calls[0]?.[1]
    expect(init?.method).toBe('PUT')
    expect(init?.body).toBe(JSON.stringify({ root: '/home/trader/Trades' }))
    const headers = (init?.headers ?? {}) as Record<string, string>
    expect(headers['Content-Type']).toBe('application/json')
  })
})
