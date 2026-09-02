/**
 * Cache bookkeeping when the loaded book changes — the part of
 * `useUploadTrades`/`useClearTrades` that has no UI of its own.
 *
 * Everything derived from the book has to be invalidated, and the selection
 * has to lose any trade the new book does not have: a clear leaves the
 * configured folder (or nothing at all), and an analysis set still pointing
 * at the uploaded book's ids would ask for trades that no longer exist.
 */

import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderHook, waitFor } from '@testing-library/react'
import type { ReactNode } from 'react'
import { beforeEach, describe, expect, it } from 'vitest'
import { useAppStore } from '../store/appStore'
import { setMockMode } from './client'
import { resetMockTradesRoot } from './mock'
import { queryKeys, useClearTrades, useUploadTrades } from './queries'

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

/** One file in the trades layout, as the browser would send it. */
function upload(): FormData {
  const form = new FormData()
  form.append('file', new File(['abc'], 'strangle'), 'Open Trades/Oct Strangle Aug 24 2026')
  return form
}

describe('changing the loaded book', () => {
  it('invalidates everything the book feeds after an upload', async () => {
    const { wrapper, invalidated, client } = harness()
    const { result } = renderHook(() => useUploadTrades(), { wrapper })

    result.current.mutate(upload())
    await waitFor(() => expect(result.current.isSuccess).toBe(true))

    expect(invalidated(queryKeys.closed)).toBe(true)
    expect(invalidated(queryKeys.config)).toBe(true)
    expect(client.getQueryData(queryKeys.tradesRoot)).toMatchObject({ source: 'uploaded' })
  })

  it('does the same on a clear, and drops a selection the new book has not', async () => {
    const { wrapper, invalidated, client } = harness()
    useAppStore.getState().setSelected(['gone-with-the-upload'])
    const { result } = renderHook(() => useClearTrades(), { wrapper })

    result.current.mutate()
    await waitFor(() => expect(result.current.isSuccess).toBe(true))

    expect(invalidated(queryKeys.closed)).toBe(true)
    expect(client.getQueryData(queryKeys.tradesRoot)).toMatchObject({ source: 'config' })
    expect(useAppStore.getState().selected).toEqual([])
  })
})
