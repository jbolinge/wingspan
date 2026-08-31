/** TanStack Query bindings for the `/api/v1` contract. */

import {
  useMutation,
  useQuery,
  useQueryClient,
  type QueryClient,
  type UseMutationResult,
  type UseQueryResult,
} from '@tanstack/react-query'
import { useAppStore } from '../store/appStore'
import { apiDelete, apiGet, apiPost, apiPut, apiUpload } from './client'
import type {
  ClosedTradesResponse,
  ConfigResponse,
  FsDirsResponse,
  Health,
  MarketSnapshot,
  PortfolioStats,
  ReconciliationResponse,
  ReloadResponse,
  RiskGraphRequest,
  RiskGraphResponse,
  SetTradesRootRequest,
  Trade,
  TradesResponse,
  TradesRootResponse,
  TradesUploadResponse,
  WarningsResponse,
  WhatIfRequest,
  WhatIfResponse,
} from './types'

export const queryKeys = {
  health: ['health'] as const,
  snapshot: ['market', 'snapshot'] as const,
  trades: ['trades'] as const,
  closed: ['trades', 'closed'] as const,
  trade: (id: string) => ['trades', 'detail', id] as const,
  portfolio: (ids: readonly string[]) => ['portfolio', [...ids].sort().join(',')] as const,
  riskGraph: (request: RiskGraphRequest) => ['risk-graph', request] as const,
  whatIf: (request: WhatIfRequest) => ['what-if', request] as const,
  reconciliation: ['reconciliation'] as const,
  tradesRoot: ['trades', 'root'] as const,
  fsDirs: (path: string | null, hidden: boolean) => ['fs', 'dirs', path ?? '', hidden] as const,
  warnings: ['warnings'] as const,
  config: ['config'] as const,
}

/** Everything invalidated when the trade files change on disk. */
export const TRADE_DEPENDENT_KEYS = [
  queryKeys.trades,
  queryKeys.closed,
  queryKeys.reconciliation,
  queryKeys.warnings,
  ['risk-graph'],
  ['what-if'],
  ['portfolio'],
] as const

export function useHealth(): UseQueryResult<Health> {
  return useQuery({ queryKey: queryKeys.health, queryFn: () => apiGet<Health>('/health') })
}

export function useSnapshot(): UseQueryResult<MarketSnapshot> {
  return useQuery({
    queryKey: queryKeys.snapshot,
    queryFn: () => apiGet<MarketSnapshot>('/market/snapshot'),
    // SSE is the primary channel; this is the belt to its braces.
    refetchInterval: 30_000,
  })
}

const fetchTrades = (): Promise<TradesResponse> => apiGet<TradesResponse>('/trades')

export function useTrades(): UseQueryResult<TradesResponse> {
  return useQuery({
    queryKey: queryKeys.trades,
    queryFn: fetchTrades,
    refetchInterval: 30_000,
  })
}

export function useClosedTrades(enabled = true): UseQueryResult<ClosedTradesResponse> {
  return useQuery({
    queryKey: queryKeys.closed,
    queryFn: () => apiGet<ClosedTradesResponse>('/trades/closed'),
    enabled,
  })
}

export function useTrade(id: string | null): UseQueryResult<Trade> {
  return useQuery({
    queryKey: queryKeys.trade(id ?? ''),
    queryFn: () => apiGet<Trade>(`/trades/${encodeURIComponent(id ?? '')}`),
    enabled: id !== null,
  })
}

export function usePortfolio(ids: readonly string[]): UseQueryResult<PortfolioStats> {
  return useQuery({
    queryKey: queryKeys.portfolio(ids),
    queryFn: () => apiGet<PortfolioStats>(`/portfolio?ids=${ids.map(encodeURIComponent).join(',')}`),
    enabled: ids.length > 0,
  })
}

export function useRiskGraph(
  request: RiskGraphRequest,
  enabled: boolean,
): UseQueryResult<RiskGraphResponse> {
  return useQuery({
    queryKey: queryKeys.riskGraph(request),
    queryFn: () => apiPost<RiskGraphResponse>('/risk-graph', request),
    enabled: enabled && request.trade_ids.length > 0,
    placeholderData: (previous) => previous,
  })
}

export function useWhatIf(
  request: WhatIfRequest | null,
  enabled: boolean,
): UseQueryResult<WhatIfResponse> {
  return useQuery({
    queryKey: queryKeys.whatIf(request ?? ({} as WhatIfRequest)),
    queryFn: () => apiPost<WhatIfResponse>('/what-if', request),
    enabled: enabled && request !== null && request.trade_ids.length > 0,
    placeholderData: (previous) => previous,
  })
}

export function useReconciliation(enabled = true): UseQueryResult<ReconciliationResponse> {
  return useQuery({
    queryKey: queryKeys.reconciliation,
    queryFn: () => apiGet<ReconciliationResponse>('/reconciliation'),
    enabled,
  })
}

export function useWarnings(enabled = true): UseQueryResult<WarningsResponse> {
  return useQuery({
    queryKey: queryKeys.warnings,
    queryFn: () => apiGet<WarningsResponse>('/warnings'),
    enabled,
  })
}

export function useConfig(enabled = true): UseQueryResult<ConfigResponse> {
  return useQuery({
    queryKey: queryKeys.config,
    queryFn: () => apiGet<ConfigResponse>('/config'),
    enabled,
    staleTime: 5 * 60_000,
  })
}

/* ---------- trades root + server-side folder picker ---------- */

export function useTradesRoot(): UseQueryResult<TradesRootResponse> {
  return useQuery({
    queryKey: queryKeys.tradesRoot,
    queryFn: () => apiGet<TradesRootResponse>('/trades/root'),
  })
}

/** `path === null` lets the server pick (active root's parent, else home). */
export function useFsDirs(
  path: string | null,
  hidden: boolean,
  enabled: boolean,
): UseQueryResult<FsDirsResponse> {
  return useQuery({
    queryKey: queryKeys.fsDirs(path, hidden),
    queryFn: () => {
      const params = new URLSearchParams()
      if (path !== null) params.set('path', path)
      if (hidden) params.set('hidden', '1')
      const query = params.toString()
      return apiGet<FsDirsResponse>(`/fs/dirs${query ? `?${query}` : ''}`)
    },
    enabled,
    // A bad path is a 422 the user must read and fix, not something to retry.
    retry: false,
    placeholderData: (previous) => previous,
  })
}

/**
 * Everything the book feeds, refetched, then any selection or expansion
 * pointing at a trade the new root does not have is dropped.
 */
async function adoptNewRoot(client: QueryClient, root: TradesRootResponse): Promise<void> {
  client.setQueryData(queryKeys.tradesRoot, root)
  await Promise.all([
    ...TRADE_DEPENDENT_KEYS.map((queryKey) => client.invalidateQueries({ queryKey })),
    client.invalidateQueries({ queryKey: queryKeys.config }),
    // `GET /fs/dirs` with no `path` answers relative to the *active* root, so
    // every cached listing is now about the folder we just left: without this
    // the picker reopens on the old root's parent until the 5s staleTime runs
    // out.
    client.invalidateQueries({ queryKey: ['fs', 'dirs'] }),
  ])
  // Fetched, not read from cache: the picker can be the only thing mounted,
  // and an inactive query is invalidated without being refetched.
  const open = await client.fetchQuery({ queryKey: queryKeys.trades, queryFn: fetchTrades })
  const openIds = open.trades.map((trade) => trade.id)
  useAppStore.getState().pruneSelection(openIds)
  // Closed trades are only cached once that tab has been opened; without them
  // an expanded closed row cannot be told from a stale one, so leave it be.
  const closed = client.getQueryData<ClosedTradesResponse>(queryKeys.closed)
  if (closed) {
    useAppStore.getState().pruneExpanded([...openIds, ...closed.trades.map((t) => t.id)])
  }
}

/** `PUT /trades/root` — 422 problem+json when discovery fails there. */
export function useSetTradesRoot(): UseMutationResult<TradesRootResponse, Error, string> {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (root: string) => {
      const body: SetTradesRootRequest = { root }
      return apiPut<TradesRootResponse>('/trades/root', body)
    },
    onSuccess: (data) => adoptNewRoot(client, data),
  })
}

/** `DELETE /trades/root` — back to whatever config says. */
export function useResetTradesRoot(): UseMutationResult<TradesRootResponse, Error, void> {
  const client = useQueryClient()
  return useMutation({
    mutationFn: () => apiDelete<TradesRootResponse>('/trades/root'),
    onSuccess: (data) => adoptNewRoot(client, data),
  })
}

/**
 * `POST /trades/upload` — a snapshot of the folder the user picked *on this
 * device*. Success switches the active root to the uploaded copy, so the book
 * is adopted exactly as if a server-side folder had been picked.
 */
export function useUploadTradesRoot(): UseMutationResult<TradesUploadResponse, Error, FormData> {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (form: FormData) => apiUpload<TradesUploadResponse>('/trades/upload', form),
    onSuccess: (data) => adoptNewRoot(client, data),
  })
}

export function useReload(): UseMutationResult<ReloadResponse, Error, void> {
  const client = useQueryClient()
  return useMutation({
    mutationFn: () => apiPost<ReloadResponse>('/reload', {}),
    onSuccess: () => {
      void client.invalidateQueries()
    },
  })
}
