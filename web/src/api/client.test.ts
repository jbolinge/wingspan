/**
 * The transport, from both sides.
 *
 * The browser half: `call()` maps each command onto the devserver's HTTP
 * shape and forwards TanStack's AbortSignal so a superseded request is
 * actually cancelled, and a cancelled request must not be mistaken for the
 * API being down.
 *
 * The desktop half: `call()` hands each command the exact argument envelope
 * Rust deserialises, maps both rejection shapes onto `ApiError`, and keeps the
 * DEV mock fallback — a browser-only affordance — out of the desktop app.
 */

import { invoke } from '@tauri-apps/api/core'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { RiskGraphRequest, WhatIfRequest } from './types'
import { ApiError, call, getMockMode, setMockMode } from './client'
import { queryKeys } from './queries'

/** The desktop app is "there is a `__TAURI_INTERNALS__` on `window`". */
function enterTauri(): void {
  ;(window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {}
}

function leaveTauri(): void {
  delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__
}

describe('the browser transport', () => {
  afterEach(() => {
    vi.unstubAllGlobals()
    setMockMode('forced')
  })

  it('passes the given AbortSignal through to fetch', () => {
    setMockMode('off')
    const fetchSpy = vi.fn((_input: RequestInfo | URL, _init?: RequestInit) => new Promise<Response>(() => {}))
    vi.stubGlobal('fetch', fetchSpy)

    const controller = new AbortController()
    void call('risk_graph', { request: { trade_ids: [] } }, controller.signal)

    expect(fetchSpy).toHaveBeenCalledTimes(1)
    expect(fetchSpy.mock.calls[0]?.[0]).toBe('/api/v1/risk-graph')
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

    const promise = call('risk_graph', { request: { trade_ids: [] } }, controller.signal)
    controller.abort()

    await expect(promise).rejects.toMatchObject({ name: 'AbortError' })
    // A real network error would flip DEV mode into the mock fallback; a
    // cancelled-on-purpose request must not.
    expect(getMockMode()).toBe('off')
  })

  it('set_trades_root becomes a PUT with a JSON body', () => {
    setMockMode('off')
    const fetchSpy = vi.fn((_input: RequestInfo | URL, _init?: RequestInit) => new Promise<Response>(() => {}))
    vi.stubGlobal('fetch', fetchSpy)

    void call('set_trades_root', { request: { root: '/home/trader/Trades' } })

    expect(fetchSpy).toHaveBeenCalledTimes(1)
    expect(fetchSpy.mock.calls[0]?.[0]).toBe('/api/v1/trades/root')
    const init = fetchSpy.mock.calls[0]?.[1]
    expect(init?.method).toBe('PUT')
    expect(init?.body).toBe(JSON.stringify({ root: '/home/trader/Trades' }))
    const headers = (init?.headers ?? {}) as Record<string, string>
    expect(headers['Content-Type']).toBe('application/json')
  })
})

describe('the Tauri transport', () => {
  beforeEach(() => {
    setMockMode('off')
    enterTauri()
    vi.mocked(invoke).mockReset()
    vi.mocked(invoke).mockResolvedValue(undefined)
  })

  afterEach(() => {
    leaveTauri()
    vi.unstubAllGlobals()
    setMockMode('forced')
  })

  it('portfolio carries the ids as an array, not a query string', async () => {
    await call('portfolio', { request: { ids: ['m1-2026-07-14', 'c2-2026-08-01'] } })
    expect(invoke).toHaveBeenCalledWith('portfolio', {
      request: { ids: ['m1-2026-07-14', 'c2-2026-08-01'] },
    })
  })

  it('what_if carries the request unspread', async () => {
    const request: WhatIfRequest = {
      trade_ids: ['m1-2026-07-14'],
      price: 5400,
      date: '2026-07-20',
      vol_shift: 0.01,
      include_realized: true,
    }
    await call('what_if', { request })
    expect(invoke).toHaveBeenCalledWith('what_if', { request })
  })

  it('probe_folder and set_trades_root carry the root in a request envelope', async () => {
    await call('probe_folder', { request: { root: '/home/t/Trades' } })
    expect(invoke).toHaveBeenCalledWith('probe_folder', { request: { root: '/home/t/Trades' } })

    await call('set_trades_root', { request: { root: '/home/t/Trades' } })
    expect(invoke).toHaveBeenCalledWith('set_trades_root', { request: { root: '/home/t/Trades' } })
  })

  it('pick_folder carries the start folder', async () => {
    vi.mocked(invoke).mockResolvedValue('/home/t/Trades')
    await expect(call('pick_folder', { request: { start: '/home/t' } })).resolves.toBe(
      '/home/t/Trades',
    )
    expect(invoke).toHaveBeenCalledWith('pick_folder', { request: { start: '/home/t' } })
  })

  it('risk_graph sends { request, seq } and leaves the query key alone', async () => {
    const request: RiskGraphRequest = {
      trade_ids: ['m1-2026-07-14'],
      price_range: null,
      points: 41,
      dates: null,
      vol_shift: 0,
      vol_shift_by_expiry: {},
      spot_override: null,
      include_realized: false,
    }
    const keyBefore = queryKeys.riskGraph(request)

    await call('risk_graph', { request })
    await call('risk_graph', { request })

    expect(vi.mocked(invoke).mock.calls[0]).toEqual(['risk_graph', { request, seq: 1 }])
    expect(vi.mocked(invoke).mock.calls[1]).toEqual(['risk_graph', { request, seq: 2 }])
    // The counter is a second top-level argument, never part of the request —
    // a per-call field would make every slider tick a fresh cache key.
    expect('seq' in request).toBe(false)
    expect(queryKeys.riskGraph(request)).toEqual(keyBefore)
  })

  it('an object rejection becomes an ApiError carrying status and problem', async () => {
    const problem = {
      type: 'https://wingspan.invalid/problems/unprocessable',
      title: 'Unprocessable Entity',
      status: 422,
      detail: 'no Open Trades folder',
    }
    vi.mocked(invoke).mockRejectedValue(problem)

    const error = await call('trades').catch((raw: unknown) => raw)
    expect(error).toBeInstanceOf(ApiError)
    const api = error as ApiError
    expect(api.message).toBe('no Open Trades folder')
    expect(api.status).toBe(422)
    expect(api.problem).toEqual(problem)
  })

  it('a bare-string rejection becomes ApiError(String(raw), 0, null)', async () => {
    // What an ACL refusal or an argument-deserialisation failure looks like.
    vi.mocked(invoke).mockRejectedValue('probe_folder not allowed. Command not found')

    const error = await call('probe_folder', { request: { root: '/home/t' } }).catch(
      (raw: unknown) => raw,
    )
    expect(error).toBeInstanceOf(ApiError)
    const api = error as ApiError
    expect(api.message).toBe('probe_folder not allowed. Command not found')
    expect(api.status).toBe(0)
    expect(api.problem).toBeNull()
  })

  it('an aborted invoke rejects with AbortError and never latches mock mode', async () => {
    vi.mocked(invoke).mockReturnValue(new Promise<never>(() => {}))
    const controller = new AbortController()

    const promise = call('risk_graph', { request: { trade_ids: [] } }, controller.signal)
    controller.abort()

    await expect(promise).rejects.toMatchObject({ name: 'AbortError' })
    expect(getMockMode()).toBe('off')
  })

  it('the DEV mock never latches inside Tauri', async () => {
    const fetchSpy = vi.fn()
    vi.stubGlobal('fetch', fetchSpy)
    vi.mocked(invoke).mockRejectedValue('probe_folder not allowed. Command not found')

    await expect(call('probe_folder', { request: { root: '/home/t' } })).rejects.toBeInstanceOf(
      ApiError,
    )
    expect(getMockMode()).toBe('off')
    expect(fetchSpy).not.toHaveBeenCalled()
  })

  it('DEV + not Tauri + a reachable API leaves the mode off', async () => {
    leaveTauri()
    const response = {
      ok: true,
      status: 200,
      statusText: 'OK',
      headers: { get: () => 'application/json' },
      json: () => Promise.resolve({ warnings: [] }),
    } as unknown as Response
    vi.stubGlobal('fetch', vi.fn(() => Promise.resolve(response)))

    await expect(call('warnings')).resolves.toEqual({ warnings: [] })
    expect(getMockMode()).toBe('off')
  })
})

describe('desktop-only commands', () => {
  const DESKTOP_ONLY_COMMANDS = [
    'pick_folder',
    'pick_python',
    'pick_env_file',
    'pick_token_file',
    'settings_read',
    'settings_update',
    'probe_python',
    'diagnostics',
    'load_demo',
    'leave_demo',
    'reveal_folder',
    'open_log_folder',
    'open_config_file',
    'check_for_updates',
  ] as const

  afterEach(() => {
    leaveTauri()
    vi.unstubAllGlobals()
    setMockMode('forced')
  })

  it.each(DESKTOP_ONLY_COMMANDS)('%s rejects outside Tauri', async (command) => {
    setMockMode('off')
    leaveTauri()
    await expect(call(command, { request: {} })).rejects.toThrow(
      `${command} is only available in the desktop app`,
    )
  })

  it.each(DESKTOP_ONLY_COMMANDS)('%s reaches invoke with the request envelope in Tauri', async (command) => {
    setMockMode('off')
    enterTauri()
    vi.mocked(invoke).mockReset()
    vi.mocked(invoke).mockResolvedValue(null)

    await call(command, { request: { path: '/x' } })

    expect(invoke).toHaveBeenCalledWith(command, { request: { path: '/x' } })
  })
})
