/**
 * Cache bookkeeping when the loaded book changes — the part of
 * `useSetTradesRoot`/`useClearTrades` that has no UI of its own.
 *
 * Everything derived from the book has to be invalidated, and the selection
 * has to lose any trade the new book does not have: a clear leaves the
 * configured folder (or nothing at all), and an analysis set still pointing
 * at the loaded book's ids would ask for trades that no longer exist.
 */

import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { invoke } from '@tauri-apps/api/core'
import { renderHook, waitFor } from '@testing-library/react'
import type { ReactNode } from 'react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { usePickedFile } from '../lib/pickedFile'
import { usePickedFolder } from '../lib/pickedFolder'
import { useAppStore } from '../store/appStore'
import { ApiError, setMockMode } from './client'
import { resetMockTradesRoot, resetMockTradeSource } from './mock'
import {
  adoptNewRoot,
  queryKeys,
  useClearTrades,
  useClearTradeSource,
  usePickCsvFile,
  usePickFolder,
  useSetTradesRoot,
  useSetTradeSource,
} from './queries'
import type { TradeSourceRequest, TradesRootResponse } from './types'

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
  resetMockTradesRoot()
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

describe('changing the loaded book', () => {
  it('invalidates everything the book feeds after loading a folder', async () => {
    const { wrapper, invalidated, client } = harness()
    const { result } = renderHook(() => useSetTradesRoot(), { wrapper })

    result.current.mutate('/home/trader/Trades')
    await waitFor(() => expect(result.current.isSuccess).toBe(true))

    expect(invalidated(queryKeys.closed)).toBe(true)
    expect(invalidated(queryKeys.config)).toBe(true)
    expect(client.getQueryData(queryKeys.tradesRoot)).toMatchObject({ source: 'local' })
  })

  it('does the same on a clear, and drops a selection the new book has not', async () => {
    const { wrapper, invalidated, client } = harness()
    useAppStore.getState().setSelected(['gone-with-the-folder'])
    const { result } = renderHook(() => useClearTrades(), { wrapper })

    result.current.mutate()
    await waitFor(() => expect(result.current.isSuccess).toBe(true))

    expect(invalidated(queryKeys.closed)).toBe(true)
    expect(client.getQueryData(queryKeys.tradesRoot)).toMatchObject({ source: 'config' })
    expect(useAppStore.getState().selected).toEqual([])
  })
})

describe('changing the trade source (sheet / CSV)', () => {
  it('useSetTradeSource writes queryKeys.tradeSource and invalidates the trade-dependent keys', async () => {
    const { wrapper, invalidated, client } = harness()
    // Seeded so `invalidated()` (which reads query *state*) has something to
    // report on — an untouched key with no cached data has no state at all.
    client.setQueryData(queryKeys.tradesRoot, { source: 'config' })
    const { result } = renderHook(() => useSetTradeSource(), { wrapper })
    const request: TradeSourceRequest = { kind: 'csv', path: '/home/trader/Downloads/legs.csv' }

    result.current.mutate(request)
    await waitFor(() => expect(result.current.isSuccess).toBe(true))

    expect(invalidated(queryKeys.closed)).toBe(true)
    expect(invalidated(queryKeys.config)).toBe(true)
    expect(invalidated(queryKeys.tradesRoot)).toBe(true)
    expect(client.getQueryData(queryKeys.tradeSource)).toMatchObject({ kind: 'csv' })
  })

  it('useClearTradeSource does the same, on the fallback source', async () => {
    const { wrapper, invalidated, client } = harness()
    client.setQueryData(queryKeys.tradesRoot, { source: 'config' })
    const { result } = renderHook(() => useClearTradeSource(), { wrapper })

    result.current.mutate()
    await waitFor(() => expect(result.current.isSuccess).toBe(true))

    expect(invalidated(queryKeys.closed)).toBe(true)
    expect(invalidated(queryKeys.config)).toBe(true)
    expect(invalidated(queryKeys.tradesRoot)).toBe(true)
    expect(client.getQueryData(queryKeys.tradeSource)).toMatchObject({ kind: 'demo' })
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

describe('the folder chooser', () => {
  it('usePickFolder resolves the path the command returns', async () => {
    // The chooser is a Rust dialog: no route, and no mock in front of it.
    ;(window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {}
    vi.mocked(invoke).mockReset()
    vi.mocked(invoke).mockResolvedValue('/home/trader/Picked')
    const { wrapper } = harness()

    const { result } = renderHook(() => usePickFolder(), { wrapper })
    result.current.mutate('/home/trader')
    await waitFor(() => expect(result.current.isSuccess).toBe(true))

    expect(result.current.data).toBe('/home/trader/Picked')
    expect(invoke).toHaveBeenCalledWith('pick_folder', { request: { start: '/home/trader' } })
  })

  /**
   * The reported bug, at its root: the chooser outlives whatever asked for
   * the folder. Query-core runs `mutate()`'s own callbacks only while the
   * observer still has listeners, so the answer is parked from the
   * mutation's callbacks instead — those run whether or not anyone is left.
   */
  it('parks the picked folder even when nothing is listening any more', async () => {
    ;(window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {}
    let answer: (path: string) => void = () => {}
    vi.mocked(invoke).mockReset()
    vi.mocked(invoke).mockImplementation(
      () =>
        new Promise((resolve) => {
          answer = resolve as (path: string) => void
        }),
    )
    const { wrapper } = harness()

    const { result, unmount } = renderHook(() => usePickFolder(), { wrapper })
    result.current.mutate(undefined)
    await waitFor(() => expect(invoke).toHaveBeenCalled())
    unmount()
    answer('/home/trader/Picked')

    await waitFor(() =>
      expect(usePickedFolder.getState().pending?.outcome).toEqual({
        kind: 'folder',
        path: '/home/trader/Picked',
      }),
    )
  })

  it('parks the reason a chooser failed, so it can still be shown', async () => {
    ;(window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {}
    vi.mocked(invoke).mockReset()
    vi.mocked(invoke).mockRejectedValue({
      type: 'https://wingspan.invalid/problems/unprocessable',
      title: 'Unprocessable Entity',
      status: 422,
      detail: 'that location is not a folder on this computer',
    })
    const { wrapper } = harness()

    const { result } = renderHook(() => usePickFolder(), { wrapper })
    result.current.mutate(undefined)
    await waitFor(() => expect(result.current.isError).toBe(true))

    expect(usePickedFolder.getState().pending?.outcome).toEqual({
      kind: 'error',
      detail: 'that location is not a folder on this computer',
    })
  })

  it('a cancel parks nothing at all', async () => {
    ;(window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {}
    vi.mocked(invoke).mockReset()
    vi.mocked(invoke).mockResolvedValue(null)
    const { wrapper } = harness()

    const { result } = renderHook(() => usePickFolder(), { wrapper })
    result.current.mutate(undefined)
    await waitFor(() => expect(result.current.isSuccess).toBe(true))

    expect(usePickedFolder.getState().pending).toBeNull()
  })
})

/**
 * A book is adopted even when it cannot be priced. On a fresh install with
 * no working market source `trades` answers 503, and letting that reject
 * `adoptNewRoot` used to flip "Load & analyze" to an error — a folder that
 * had in fact been loaded, reported as a market-data failure.
 */
describe('adopting a book that cannot be priced', () => {
  it('resolves, keeps the new root, and prunes nothing', async () => {
    const { client } = harness()
    useAppStore.getState().setSelected(['still-selected'])
    vi.spyOn(client, 'fetchQuery').mockRejectedValue(
      new ApiError('no market snapshot yet', 503, null),
    )
    const root: TradesRootResponse = {
      root: '/home/trader/Trades',
      source: 'local',
      config_root: null,
      open_trades: 2,
      closed_trades: 1,
      watching: true,
      recent: [],
    }

    await expect(adoptNewRoot(client, root)).resolves.toBeUndefined()

    expect(client.getQueryData(queryKeys.tradesRoot)).toMatchObject({ root: root.root })
    expect(useAppStore.getState().selected).toEqual(['still-selected'])
  })
})
