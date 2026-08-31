import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { render, type RenderOptions, type RenderResult } from '@testing-library/react'
import type { ReactElement, ReactNode } from 'react'
import { setMockMode } from '../api/client'
import { DEFAULT_GRAPH, useAppStore } from '../store/appStore'

export function createTestQueryClient(): QueryClient {
  return new QueryClient({
    defaultOptions: {
      queries: { retry: false, staleTime: 0, gcTime: 0, refetchOnWindowFocus: false },
      mutations: { retry: false },
    },
  })
}

/** Renders with a fresh QueryClient and the API client pinned to the mock. */
export function renderWithClient(
  ui: ReactElement,
  options?: Omit<RenderOptions, 'wrapper'>,
): RenderResult & { client: QueryClient } {
  setMockMode('forced')
  const client = createTestQueryClient()
  const wrapper = ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={client}>{children}</QueryClientProvider>
  )
  return { ...render(ui, { wrapper, ...options }), client }
}

/** Puts the persisted UI store back to its defaults between tests. */
export function resetAppStore(): void {
  localStorage.clear()
  useAppStore.setState({
    theme: 'dark',
    tab: 'positions',
    selected: [],
    sortKey: 'opened_on',
    sortDir: 'asc',
    expanded: [],
    graph: DEFAULT_GRAPH,
  })
}
