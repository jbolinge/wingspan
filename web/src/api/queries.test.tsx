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
import { useAppStore } from '../store/appStore'
import { setMockMode } from './client'
import { resetMockTradesRoot } from './mock'
import { queryKeys, useClearTrades, usePickFolder, useSetTradesRoot } from './queries'

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
})
