/**
 * Cache bookkeeping when the loaded book changes — the part of
 * `useSetTradeSource`/`useClearTradeSource` that has no UI of its own.
 *
 * Everything derived from the book has to be invalidated, and the selection
 * has to lose any trade the new book does not have: a clear leaves the
 * configured source (or nothing at all), and an analysis set still pointing
 * at the loaded book's ids would ask for trades that no longer exist.
 */

import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { invoke } from '@tauri-apps/api/core'
import { act, renderHook, waitFor } from '@testing-library/react'
import type { ReactNode } from 'react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { toModelLegs } from '../lib/model'
import { usePickedFile } from '../lib/pickedFile'
import { DEFAULT_MODEL, useAppStore, type ModelLeg as ModelLegState } from '../store/appStore'
import { ApiError, call, setMockMode } from './client'
import { useBookEpoch } from './epoch'
import { resetMockTradeSource } from './mock'
import {
  adoptNewSource,
  MODEL_SYNC_DEBOUNCE_MS,
  queryKeys,
  useClearTradeSource,
  useModelOrders,
  useModelSyncStatus,
  useModelUniverseSync,
  usePickCsvFile,
  useSetModelContracts,
  useSetTradeSource,
} from './queries'
import type { OptionContract, TradeSourceRequest, TradeSourceResponse } from './types'

// Wraps the real implementation so a test can watch (or replace) the
// commands a hook sends without losing the mock API underneath.
vi.mock('./client', async (importOriginal) => {
  const actual = await importOriginal<typeof import('./client')>()
  return { ...actual, call: vi.fn(actual.call) }
})

/** Cached, never expiring, and with nobody watching — so an invalidation is
 *  recorded on the entry instead of being erased by an immediate refetch. */
function cachingClient(): QueryClient {
  return new QueryClient({
    defaultOptions: {
      queries: { retry: false, staleTime: Infinity, gcTime: Infinity },
      mutations: { retry: false },
    },
  })
}

beforeEach(() => {
  setMockMode('forced')
  resetMockTradeSource()
  useAppStore.getState().setSelected([])
  useAppStore.setState({ model: DEFAULT_MODEL })
  vi.mocked(call).mockClear()
})

afterEach(() => {
  delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__
})

function harness() {
  const client = cachingClient()
  client.setQueryData(queryKeys.trades, { trades: [], as_of: null })
  client.setQueryData(queryKeys.closed, { trades: [], as_of: null })
  client.setQueryData(queryKeys.config, { resolved: {} })
  const wrapper = ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={client}>{children}</QueryClientProvider>
  )
  const invalidated = (key: readonly unknown[]): boolean =>
    client.getQueryState(key)?.isInvalidated === true
  return { client, wrapper, invalidated }
}

describe('changing the trade source (sheet / CSV)', () => {
  it('useSetTradeSource writes queryKeys.tradeSource and invalidates the trade-dependent keys', async () => {
    const { wrapper, invalidated, client } = harness()
    const { result } = renderHook(() => useSetTradeSource(), { wrapper })
    const request: TradeSourceRequest = { kind: 'csv', path: '/home/trader/Downloads/legs.csv' }

    result.current.mutate(request)
    await waitFor(() => expect(result.current.isSuccess).toBe(true))

    expect(invalidated(queryKeys.closed)).toBe(true)
    expect(invalidated(queryKeys.config)).toBe(true)
    expect(client.getQueryData(queryKeys.tradeSource)).toMatchObject({ kind: 'csv' })
  })

  it('useClearTradeSource does the same, on the fallback source, and drops a selection the new book has not', async () => {
    const { wrapper, invalidated, client } = harness()
    useAppStore.getState().setSelected(['gone-with-the-source'])
    const { result } = renderHook(() => useClearTradeSource(), { wrapper })

    result.current.mutate()
    await waitFor(() => expect(result.current.isSuccess).toBe(true))

    expect(invalidated(queryKeys.closed)).toBe(true)
    expect(invalidated(queryKeys.config)).toBe(true)
    expect(client.getQueryData(queryKeys.tradeSource)).toMatchObject({ kind: 'demo' })
    expect(useAppStore.getState().selected).toEqual([])
  })

  it('usePickCsvFile parks a path into usePickedFile even when the observer has unsubscribed', async () => {
    ;(window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {}
    let answer: (picked: { path: string } | null) => void = () => {}
    vi.mocked(invoke).mockReset()
    vi.mocked(invoke).mockImplementation(
      () =>
        new Promise((resolve) => {
          answer = resolve as (picked: { path: string } | null) => void
        }),
    )
    const { wrapper } = harness()

    const { result, unmount } = renderHook(() => usePickCsvFile(), { wrapper })
    result.current.mutate()
    await waitFor(() => expect(invoke).toHaveBeenCalled())
    unmount()
    answer({ path: '/home/trader/Downloads/legs.csv' })

    await waitFor(() =>
      expect(usePickedFile.getState().pending?.outcome).toEqual({
        kind: 'file',
        path: '/home/trader/Downloads/legs.csv',
      }),
    )
  })
})

/**
 * A book is adopted even when it cannot be priced. On a fresh install with
 * no working market source `trades` answers 503, and letting that reject
 * `adoptNewSource` used to flip "Load & analyze" to an error — a source that
 * had in fact been loaded, reported as a market-data failure.
 */
describe('adopting a book that cannot be priced', () => {
  it('resolves, keeps the new source, and prunes nothing', async () => {
    const { client } = harness()
    useAppStore.getState().setSelected(['still-selected'])
    vi.spyOn(client, 'fetchQuery').mockRejectedValue(
      new ApiError('no market snapshot yet', 503, null),
    )
    const source: TradeSourceResponse = {
      kind: 'csv',
      label: '/home/trader/Downloads/legs.csv',
      tab: null,
      path: '/home/trader/Downloads/legs.csv',
      open_trades: 2,
      closed_trades: 1,
      legs: 5,
      rows_skipped: 0,
      chosen_at: '2026-08-31T00:00:00Z',
      fetched_at: '2026-08-31T00:00:00Z',
      stale: false,
      detail: null,
      auto_refresh_secs: null,
      recent: [],
    }

    await expect(adoptNewSource(client, source)).resolves.toBeUndefined()

    expect(client.getQueryData(queryKeys.tradeSource)).toMatchObject({ path: source.path })
    expect(useAppStore.getState().selected).toEqual(['still-selected'])
  })
})

/* ---------- the model ---------- */

function contract(strike: number): OptionContract {
  return {
    underlying: 'SPX',
    root: 'SPXW',
    expiry: '2026-10-16',
    settlement: 'pm',
    strike,
    right: 'put',
    multiplier: 100,
    osi: `SPXW  261016P0${strike}000`,
    tos_code: `SPXW261016P${strike}`,
  }
}

describe('useSetModelContracts', () => {
  it('refetches every model-dependent query once it settles', async () => {
    const { wrapper, invalidated, client } = harness()
    client.setQueryData(['risk-graph', { trade_ids: ['a'] }], {})
    client.setQueryData(['what-if', { trade_ids: ['a'] }], {})
    client.setQueryData(queryKeys.portfolio(['a']), {})
    client.setQueryData(queryKeys.modelOrders([]), {})
    const { result } = renderHook(() => useSetModelContracts(), { wrapper })

    result.current.mutate([])
    await waitFor(() => expect(result.current.isSuccess).toBe(true))

    expect(invalidated(['risk-graph', { trade_ids: ['a'] }])).toBe(true)
    expect(invalidated(['what-if', { trade_ids: ['a'] }])).toBe(true)
    expect(invalidated(queryKeys.portfolio(['a']))).toBe(true)
    // A contract newly quoted is no longer priced off a neighbour.
    expect(invalidated(queryKeys.modelOrders([]))).toBe(true)
    // The book itself is untouched by a registration.
    expect(invalidated(queryKeys.trades)).toBe(false)
  })

  it('refetches them after a failure too — the graph is still right, just priced off neighbours', async () => {
    const { wrapper, invalidated, client } = harness()
    client.setQueryData(['risk-graph', { trade_ids: ['a'] }], {})
    vi.mocked(call).mockRejectedValueOnce(new ApiError('refresh timed out', 500, null))
    const { result } = renderHook(() => ({ set: useSetModelContracts(), status: useModelSyncStatus() }), {
      wrapper,
    })

    result.current.set.mutate([])
    await waitFor(() => expect(result.current.set.isError).toBe(true))

    expect(invalidated(['risk-graph', { trade_ids: ['a'] }])).toBe(true)
    expect(result.current.status).toEqual({
      state: 'unquoted',
      message: 'Model quotes unavailable — priced from neighbouring strikes',
      detail: 'refresh timed out',
    })
  })
})

describe('useModelOrders', () => {
  function leg(strike: number, qty: number): ModelLegState {
    return {
      contract: {
        underlying: 'SPX',
        root: 'SPX',
        expiry: '2026-11-20',
        settlement: 'am',
        strike,
        right: 'put',
        multiplier: 100,
        osi: `SPX   261120P0${strike}000`,
        tos_code: `SPX261120P${strike}`,
      },
      qty,
    }
  }
  const orderCalls = () => vi.mocked(call).mock.calls.filter(([command]) => command === 'model_orders')

  // Keyed like `portfolio`, on the sorted request legs: the same model typed
  // in another order is the same cache entry, not a second fetch.
  it('keys on what is modeled, not the order it was typed in', () => {
    const typed = toModelLegs([leg(7500, -1), leg(7400, 1)])
    const retyped = toModelLegs([leg(7400, 1), leg(7500, -1)])
    expect(queryKeys.modelOrders(typed)).toEqual(queryKeys.modelOrders(retyped))
    expect(queryKeys.modelOrders(typed)).not.toEqual(queryKeys.modelOrders(toModelLegs([leg(7500, -2), leg(7400, 1)])))
  })

  it('asks for nothing with no legs', async () => {
    const { wrapper } = harness()
    const { result } = renderHook(() => useModelOrders([]), { wrapper })
    await new Promise((resolve) => setTimeout(resolve, 20))
    expect(result.current.fetchStatus).toBe('idle')
    expect(orderCalls()).toEqual([])
  })

  it('sends the legs in the { request: { model_legs } } envelope', async () => {
    const { wrapper } = harness()
    const legs = toModelLegs([leg(7500, -1), leg(7400, 1)])
    const { result } = renderHook(() => useModelOrders(legs), { wrapper })
    await waitFor(() => expect(result.current.isSuccess).toBe(true))
    expect(orderCalls()).toEqual([['model_orders', { request: { model_legs: legs } }]])
    expect(result.current.data?.orders).toHaveLength(2)
  })
})

describe('useModelSyncStatus', () => {
  async function settle(outcome: () => Promise<unknown>) {
    const { wrapper } = harness()
    vi.mocked(call).mockImplementationOnce(outcome as never)
    const { result } = renderHook(() => ({ set: useSetModelContracts(), status: useModelSyncStatus() }), {
      wrapper,
    })
    result.current.set.mutate([])
    await waitFor(() => expect(result.current.set.isIdle).toBe(false))
    await waitFor(() => expect(result.current.set.isPending).toBe(false))
    return result.current.status
  }

  it('is quiet when every modeled contract came back quoted', async () => {
    const status = await settle(async () => ({ contracts: 2, quoted: 2, missing: [], refreshed: true, detail: null }))
    expect(status).toEqual({ state: 'ok', message: null, detail: null })
  })

  // A 4xx registered nothing — "priced from neighbouring strikes" would be
  // untrue; the same legs are refused on the graph too.
  it('says the server refused the set, in its own words, for a 4xx', async () => {
    const status = await settle(async () => {
      throw new ApiError('2026-08-28 expired before 2026-08-31; a modeled leg must be live', 422, null)
    })
    expect(status).toEqual({
      state: 'refused',
      message: 'Model refused: 2026-08-28 expired before 2026-08-31; a modeled leg must be live',
      detail: '2026-08-28 expired before 2026-08-31; a modeled leg must be live',
    })
  })

  it('counts the contracts left without a quote and names them in the detail', async () => {
    const status = await settle(async () => ({
      contracts: 3,
      quoted: 1,
      missing: ['SPXW261016P7400', 'SPXW261016P7500'],
      refreshed: true,
      detail: null,
    }))
    expect(status).toEqual({
      state: 'unquoted',
      message: 'No quote for 2 modeled contracts — priced from neighbouring strikes',
      detail: "no quote for SPXW261016P7400, SPXW261016P7500 — priced from the neighbouring strikes' IV",
    })
  })

  it("passes on the server's sentence when the refresh itself failed", async () => {
    const status = await settle(async () => ({
      contracts: 1,
      quoted: 0,
      missing: ['SPXW261016P7400'],
      refreshed: false,
      detail: 'the market refresh did not finish within 60 s',
    }))
    expect(status).toEqual({
      state: 'unquoted',
      message: 'Model quotes not refreshed — priced from neighbouring strikes',
      detail: 'the market refresh did not finish within 60 s',
    })
  })
})

describe('useModelUniverseSync', () => {
  const sent = () =>
    vi
      .mocked(call)
      .mock.calls.filter(([command]) => command === 'set_model_contracts')
      .map(([, args]) => (args as { request: { contracts: { strike: number }[] } }).request.contracts)

  /** The server's clock, as the snapshot stamps it: 31 Aug 2026, UTC.
   *  `'pending'` never answers; `null` fails (no market at all). */
  let snapshotAsOf: string | null | 'pending' = '2026-08-31T16:00:00Z'

  beforeEach(() => {
    setMockMode('off')
    snapshotAsOf = '2026-08-31T16:00:00Z'
    vi.mocked(call).mockImplementation(async (command) => {
      if (command === 'set_model_contracts') {
        return { contracts: 0, quoted: 0, missing: [], refreshed: true, detail: null } as never
      }
      if (command === 'market_snapshot') {
        if (snapshotAsOf === 'pending') return new Promise(() => {}) as never
        if (snapshotAsOf === null) throw new ApiError('no market snapshot yet', 503, null)
        return { as_of: snapshotAsOf } as never
      }
      return undefined as never
    })
  })

  afterEach(async () => {
    const actual = await vi.importActual<typeof import('./client')>('./client')
    vi.mocked(call).mockImplementation(actual.call)
    setMockMode('forced')
  })

  /** Long enough for the debounce to have fired, had it been armed. */
  const pastDebounce = () => new Promise((resolve) => setTimeout(resolve, MODEL_SYNC_DEBOUNCE_MS + 150))

  it('sends nothing at launch with nothing modeled', async () => {
    const { wrapper } = harness()
    renderHook(() => useModelUniverseSync(), { wrapper })
    await pastDebounce()
    expect(sent()).toEqual([])
  })

  it('sends the distinct contracts once the set holds still, and not on a quantity change', async () => {
    const { wrapper } = harness()
    renderHook(() => useModelUniverseSync(), { wrapper })
    act(() => {
      useAppStore.getState().setModelQty(contract(7500), -1)
      useAppStore.getState().setModelQty(contract(7400), 1)
    })
    await waitFor(() => expect(sent()).toHaveLength(1))
    expect(sent()[0]?.map((c) => c.strike)).toEqual([7400, 7500])
    expect(sent()[0]?.[0]).not.toHaveProperty('tos_code')

    act(() => useAppStore.getState().setModelQty(contract(7500), -4))
    await pastDebounce()
    expect(sent()).toHaveLength(1)
  })

  it('clears the extras when the model is emptied', async () => {
    const { wrapper } = harness()
    renderHook(() => useModelUniverseSync(), { wrapper })
    act(() => useAppStore.getState().setModelQty(contract(7500), -1))
    await waitFor(() => expect(sent()).toHaveLength(1))
    act(() => useAppStore.getState().clearModel())
    await waitFor(() => expect(sent()).toHaveLength(2))
    expect(sent()[1]).toEqual([])
  })

  it('re-registers after a rebuilt runtime (app_ready / trades_changed)', async () => {
    const { wrapper } = harness()
    act(() => useAppStore.getState().setModelQty(contract(7500), -1))
    renderHook(() => useModelUniverseSync(), { wrapper })
    await waitFor(() => expect(sent()).toHaveLength(1))
    act(() => useBookEpoch.getState().bump())
    await waitFor(() => expect(sent()).toHaveLength(2))
    expect(sent()[1]?.map((c) => c.strike)).toEqual([7500])
  })

  // Yesterday's 0DTE, persisted overnight: the server would refuse every
  // request carrying it, so it leaves the store as soon as the snapshot's
  // date shows it expired — by the server's clock, not the browser's.
  it('drops a leg that expired before the snapshot date, and registers only the live ones', async () => {
    const yesterday = { ...contract(7600), expiry: '2026-08-28', tos_code: 'SPXW260828P7600' }
    act(() => {
      useAppStore.getState().setModelQty(yesterday, -1)
      useAppStore.getState().setModelQty(contract(7500), 1)
    })
    const { wrapper } = harness()
    renderHook(() => useModelUniverseSync(), { wrapper })
    await waitFor(() =>
      expect(useAppStore.getState().model.legs.map((leg) => leg.contract.tos_code)).toEqual(['SPXW261016P7500']),
    )
    await waitFor(() => expect(sent()).toHaveLength(1))
    expect(sent()[0]?.map((c) => c.strike)).toEqual([7500])
  })

  // The demo's clock is pinned to 31 Aug 2026: a September leg is live
  // there whatever the browser's calendar says.
  it('keeps a leg the server still prices, on the server\'s date', async () => {
    act(() => useAppStore.getState().setModelQty({ ...contract(7500), expiry: '2026-08-31' }, -1))
    const { wrapper } = harness()
    renderHook(() => useModelUniverseSync(), { wrapper })
    await waitFor(() => expect(sent()).toHaveLength(1))
    expect(useAppStore.getState().model.legs).toHaveLength(1)
  })

  it('sends nothing until a snapshot says what day the server thinks it is', async () => {
    snapshotAsOf = 'pending'
    act(() => useAppStore.getState().setModelQty(contract(7500), -1))
    const { wrapper } = harness()
    renderHook(() => useModelUniverseSync(), { wrapper })
    await pastDebounce()
    expect(sent()).toEqual([])
    expect(useAppStore.getState().model.legs).toHaveLength(1)
  })

  // No market at all: there is no date coming to wait for. The set goes out
  // as it is, so the contracts are quoted once a market answers (and a stale
  // leg is refused in the server's own words rather than silently held).
  it('sends the set as it is when there is no market to take a date from', async () => {
    snapshotAsOf = null
    act(() => useAppStore.getState().setModelQty(contract(7500), -1))
    const { wrapper } = harness()
    renderHook(() => useModelUniverseSync(), { wrapper })
    await waitFor(() => expect(sent()).toHaveLength(1))
    expect(useAppStore.getState().model.legs).toHaveLength(1)
  })

  it('does nothing in mock mode, where every contract is priced anyway', async () => {
    setMockMode('forced')
    const { wrapper } = harness()
    renderHook(() => useModelUniverseSync(), { wrapper })
    act(() => useAppStore.getState().setModelQty(contract(7500), -1))
    await pastDebounce()
    expect(sent()).toEqual([])
  })
})
