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
import type { RiskGraphRequest, TradeSourceRequest, WhatIfRequest } from './types'
import { ApiError, call, errorSlug, getMockMode, setMockMode } from './client'
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

  it('trade_source is a GET on /trades/source', () => {
    setMockMode('off')
    const fetchSpy = vi.fn((_input: RequestInfo | URL, _init?: RequestInit) => new Promise<Response>(() => {}))
    vi.stubGlobal('fetch', fetchSpy)

    void call('trade_source')

    expect(fetchSpy.mock.calls[0]?.[0]).toBe('/api/v1/trades/source')
    expect(fetchSpy.mock.calls[0]?.[1]?.method).toBe('GET')
  })

  it('probe_trade_source is a POST on /trades/source/probe with a { source } body', () => {
    setMockMode('off')
    const fetchSpy = vi.fn((_input: RequestInfo | URL, _init?: RequestInit) => new Promise<Response>(() => {}))
    vi.stubGlobal('fetch', fetchSpy)
    const source: TradeSourceRequest = { kind: 'csv', path: '/home/trader/legs.csv' }

    void call('probe_trade_source', { request: { source } })

    expect(fetchSpy.mock.calls[0]?.[0]).toBe('/api/v1/trades/source/probe')
    const init = fetchSpy.mock.calls[0]?.[1]
    expect(init?.method).toBe('POST')
    expect(init?.body).toBe(JSON.stringify({ source }))
  })

  it('set_trade_source is a PUT on /trades/source with the same { source } body', () => {
    setMockMode('off')
    const fetchSpy = vi.fn((_input: RequestInfo | URL, _init?: RequestInit) => new Promise<Response>(() => {}))
    vi.stubGlobal('fetch', fetchSpy)
    const source: TradeSourceRequest = { kind: 'sheet', url: 'https://docs.google.com/x', tab: 'Legs' }

    void call('set_trade_source', { request: { source } })

    expect(fetchSpy.mock.calls[0]?.[0]).toBe('/api/v1/trades/source')
    const init = fetchSpy.mock.calls[0]?.[1]
    expect(init?.method).toBe('PUT')
    expect(init?.body).toBe(JSON.stringify({ source }))
  })

  it('clear_trade_source is a DELETE on /trades/source', () => {
    setMockMode('off')
    const fetchSpy = vi.fn((_input: RequestInfo | URL, _init?: RequestInit) => new Promise<Response>(() => {}))
    vi.stubGlobal('fetch', fetchSpy)

    void call('clear_trade_source')

    expect(fetchSpy.mock.calls[0]?.[0]).toBe('/api/v1/trades/source')
    expect(fetchSpy.mock.calls[0]?.[1]?.method).toBe('DELETE')
  })

  /** The browser half of the model's three new commands, and `portfolio`'s
   *  move to a POST body (always, so there is one code path). */
  it.each([
    ['chain_expirations', undefined, '/api/v1/chain/expirations', 'GET', undefined],
    [
      'option_chain',
      { expiry: '2026-10-16', root: 'SPX' },
      '/api/v1/chain',
      'POST',
      { expiry: '2026-10-16', root: 'SPX' },
    ],
    [
      'set_model_contracts',
      { contracts: [] },
      '/api/v1/model/contracts',
      'PUT',
      { contracts: [] },
    ],
    ['portfolio', { ids: ['a', 'b'] }, '/api/v1/portfolio', 'POST', { ids: ['a', 'b'], model_legs: [] }],
    [
      'portfolio',
      { ids: [], model_legs: [{ qty: -1 }] },
      '/api/v1/portfolio',
      'POST',
      { ids: [], model_legs: [{ qty: -1 }] },
    ],
  ] as const)('%s → %s %s', (command, request, path, method, body) => {
    setMockMode('off')
    const fetchSpy = vi.fn((_input: RequestInfo | URL, _init?: RequestInit) => new Promise<Response>(() => {}))
    vi.stubGlobal('fetch', fetchSpy)

    void call(command, request === undefined ? undefined : { request })

    expect(fetchSpy.mock.calls[0]?.[0]).toBe(path)
    const init = fetchSpy.mock.calls[0]?.[1]
    expect(init?.method).toBe(method)
    expect(init?.body).toBe(body === undefined ? undefined : JSON.stringify(body))
  })
})

describe('errorSlug', () => {
  it('returns the last segment of a problem document’s type', () => {
    const error = new ApiError('not shared', 422, {
      type: 'https://wingspan.invalid/problems/sheet-not-shared',
      title: 'Unprocessable Entity',
      status: 422,
      detail: 'not shared',
    })
    expect(errorSlug(error)).toBe('sheet-not-shared')
  })

  it('returns null for a throw that carries no problem document', () => {
    expect(errorSlug('just a string')).toBeNull()
    expect(errorSlug(new Error('plain error'))).toBeNull()
    expect(errorSlug(new ApiError('no problem', 0, null))).toBeNull()
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

  it('the chain and model commands carry their request unspread', async () => {
    await call('chain_expirations')
    expect(invoke).toHaveBeenLastCalledWith('chain_expirations', undefined)
    await call('option_chain', { request: { expiry: '2026-10-16', root: 'SPXW' } })
    expect(invoke).toHaveBeenLastCalledWith('option_chain', {
      request: { expiry: '2026-10-16', root: 'SPXW' },
    })
    const contracts = [
      {
        underlying: 'SPX',
        root: 'SPXW',
        expiry: '2026-10-16',
        settlement: 'pm',
        strike: 7700,
        right: 'call',
        multiplier: 100,
      },
    ]
    await call('set_model_contracts', { request: { contracts } })
    expect(invoke).toHaveBeenLastCalledWith('set_model_contracts', { request: { contracts } })
    await call('portfolio', { request: { ids: [], model_legs: [{ contract: contracts[0], qty: 1 }] } })
    expect(invoke).toHaveBeenLastCalledWith('portfolio', {
      request: { ids: [], model_legs: [{ contract: contracts[0], qty: 1 }] },
    })
  })

  it('what_if carries the request unspread', async () => {
    const request: WhatIfRequest = {
      trade_ids: ['m1-2026-07-14'],
      price: 5400,
      date: '2026-07-20',
      vol_shift: 0.01,
      include_realized: true,
      model_legs: [],
    }
    await call('what_if', { request })
    expect(invoke).toHaveBeenCalledWith('what_if', { request })
  })

  it('each trade-source command is invoked with the { request: { source } } envelope', async () => {
    const source: TradeSourceRequest = { kind: 'csv', path: '/home/t/legs.csv' }

    await call('probe_trade_source', { request: { source } })
    expect(invoke).toHaveBeenCalledWith('probe_trade_source', { request: { source } })

    await call('set_trade_source', { request: { source } })
    expect(invoke).toHaveBeenCalledWith('set_trade_source', { request: { source } })

    await call('clear_trade_source')
    expect(invoke).toHaveBeenCalledWith('clear_trade_source', undefined)

    await call('trade_source')
    expect(invoke).toHaveBeenCalledWith('trade_source', undefined)
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
      model_legs: [],
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
      detail: 'no trades loaded',
    }
    vi.mocked(invoke).mockRejectedValue(problem)

    const error = await call('trades').catch((raw: unknown) => raw)
    expect(error).toBeInstanceOf(ApiError)
    const api = error as ApiError
    expect(api.message).toBe('no trades loaded')
    expect(api.status).toBe(422)
    expect(api.problem).toEqual(problem)
  })

  it('a bare-string rejection becomes ApiError(String(raw), 0, null)', async () => {
    // What an ACL refusal or an argument-deserialisation failure looks like.
    vi.mocked(invoke).mockRejectedValue('probe_trade_source not allowed. Command not found')

    const source: TradeSourceRequest = { kind: 'csv', path: '/home/t/legs.csv' }
    const error = await call('probe_trade_source', { request: { source } }).catch(
      (raw: unknown) => raw,
    )
    expect(error).toBeInstanceOf(ApiError)
    const api = error as ApiError
    expect(api.message).toBe('probe_trade_source not allowed. Command not found')
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
    vi.mocked(invoke).mockRejectedValue('probe_trade_source not allowed. Command not found')

    const source: TradeSourceRequest = { kind: 'csv', path: '/home/t/legs.csv' }
    await expect(
      call('probe_trade_source', { request: { source } }),
    ).rejects.toBeInstanceOf(ApiError)
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
    'pick_csv_file',
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
    'open_sheet',
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
