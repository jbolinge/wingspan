/**
 * Cache bookkeeping when the trades root moves — the part of
 * `useSetTradesRoot`/`useResetTradesRoot` that has no UI of its own.
 *
 * Everything derived from the book has to be invalidated, and so do the
 * folder listings: `GET /fs/dirs` with no `path` answers relative to the
 * *active* root, so a listing cached under the old one is about a folder the
 * app has left.
 */

import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderHook, waitFor } from '@testing-library/react'
import type { ReactNode } from 'react'
import { beforeEach, describe, expect, it } from 'vitest'
import { setMockMode } from './client'
import { resetMockTradesRoot } from './mock'
import { queryKeys, useResetTradesRoot, useSetTradesRoot } from './queries'
import type { FsDirsResponse } from './types'

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

const STALE_LISTING: FsDirsResponse = {
  path: '/home/trader/workspace/tos-exports',
  parent: '/home/trader/workspace',
  home: '/home/trader',
  entries: [],
}

beforeEach(() => {
  setMockMode('forced')
  resetMockTradesRoot()
})

function harness() {
  const client = cachingClient()
  client.setQueryData(queryKeys.fsDirs(null, false), STALE_LISTING)
  client.setQueryData(queryKeys.trades, { trades: [], as_of: null })
  client.setQueryData(queryKeys.closed, { trades: [], as_of: null })
  const wrapper = ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={client}>{children}</QueryClientProvider>
  )
  const invalidated = (key: readonly unknown[]): boolean =>
    client.getQueryState(key)?.isInvalidated === true
  return { client, wrapper, invalidated }
}

describe('moving the trades root', () => {
  it('invalidates the folder listings as well as the book', async () => {
    const { wrapper, invalidated } = harness()
    const { result } = renderHook(() => useSetTradesRoot(), { wrapper })

    result.current.mutate('/mnt/backup')
    await waitFor(() => expect(result.current.isSuccess).toBe(true))

    expect(invalidated(queryKeys.closed)).toBe(true)
    expect(invalidated(queryKeys.fsDirs(null, false))).toBe(true)
  })

  it('does the same when the override is dropped', async () => {
    const { wrapper, invalidated } = harness()
    const { result } = renderHook(() => useResetTradesRoot(), { wrapper })

    result.current.mutate()
    await waitFor(() => expect(result.current.isSuccess).toBe(true))

    expect(invalidated(queryKeys.fsDirs(null, false))).toBe(true)
  })
})
