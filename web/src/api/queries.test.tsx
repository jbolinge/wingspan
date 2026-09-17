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
import { renderHook, waitFor } from '@testing-library/react'
import type { ReactNode } from 'react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { usePickedFile } from '../lib/pickedFile'
import { useAppStore } from '../store/appStore'
import { ApiError, setMockMode } from './client'
import { resetMockTradeSource } from './mock'
import {
  adoptNewSource,
  queryKeys,
  useClearTradeSource,
  usePickCsvFile,
  useSetTradeSource,
} from './queries'
import type { TradeSourceRequest, TradeSourceResponse } from './types'

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
