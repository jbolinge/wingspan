/** TanStack Query bindings for the app's commands (see `client.ts`). */

import {
  useMutation,
  useQuery,
  useQueryClient,
  type QueryClient,
  type UseMutationResult,
  type UseQueryResult,
} from '@tanstack/react-query'
import { useAppStore } from '../store/appStore'
import { call } from './client'
import type {
  ClosedTradesResponse,
  ConfigResponse,
  FolderProbe,
  Health,
  MarketSnapshot,
  PortfolioStats,
  ReconciliationResponse,
  ReloadResponse,
  RiskGraphRequest,
  RiskGraphResponse,
  Trade,
  TradesResponse,
  TradesRootResponse,
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
  return useQuery({ queryKey: queryKeys.health, queryFn: () => call<Health>('health') })
}

export function useSnapshot(): UseQueryResult<MarketSnapshot> {
  return useQuery({
    queryKey: queryKeys.snapshot,
    queryFn: () => call<MarketSnapshot>('market_snapshot'),
    // SSE is the primary channel; this is the belt to its braces.
    refetchInterval: 30_000,
  })
}

const fetchTrades = (): Promise<TradesResponse> => call<TradesResponse>('trades')

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
    queryFn: () => call<ClosedTradesResponse>('trades_closed'),
    enabled,
  })
}

export function useTrade(id: string | null): UseQueryResult<Trade> {
  return useQuery({
    queryKey: queryKeys.trade(id ?? ''),
    queryFn: () => call<Trade>('trade_by_id', { request: { id: id ?? '' } }),
    enabled: id !== null,
  })
}

export function usePortfolio(ids: readonly string[]): UseQueryResult<PortfolioStats> {
  return useQuery({
    queryKey: queryKeys.portfolio(ids),
    queryFn: () => call<PortfolioStats>('portfolio', { request: { ids: [...ids] } }),
    enabled: ids.length > 0,
  })
}

export function useRiskGraph(
  request: RiskGraphRequest,
  enabled: boolean,
): UseQueryResult<RiskGraphResponse> {
  return useQuery({
    queryKey: queryKeys.riskGraph(request),
    // Forwarding TanStack's own AbortSignal means a request superseded by
    // the next slider tick (a new query key) is actually cancelled, not just
    // ignored once it resolves.
    queryFn: ({ signal }) => call<RiskGraphResponse>('risk_graph', { request }, signal),
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
    queryFn: () => call<WhatIfResponse>('what_if', { request }),
    enabled: enabled && request !== null && request.trade_ids.length > 0,
    placeholderData: (previous) => previous,
  })
}

export function useReconciliation(enabled = true): UseQueryResult<ReconciliationResponse> {
  return useQuery({
    queryKey: queryKeys.reconciliation,
    queryFn: () => call<ReconciliationResponse>('reconciliation'),
    enabled,
  })
}

export function useWarnings(enabled = true): UseQueryResult<WarningsResponse> {
  return useQuery({
    queryKey: queryKeys.warnings,
    queryFn: () => call<WarningsResponse>('warnings'),
    enabled,
  })
}

export function useConfig(enabled = true): UseQueryResult<ConfigResponse> {
  return useQuery({
    queryKey: queryKeys.config,
    queryFn: () => call<ConfigResponse>('app_config'),
    enabled,
    staleTime: 5 * 60_000,
  })
}

/* ---------- which book is loaded ---------- */

export function useTradesRoot(): UseQueryResult<TradesRootResponse> {
  return useQuery({
    queryKey: queryKeys.tradesRoot,
    queryFn: () => call<TradesRootResponse>('trades_root'),
  })
}

/**
 * Everything the book feeds, refetched, then any selection or expansion
 * pointing at a trade the new book does not have is dropped.
 */
async function adoptNewRoot(client: QueryClient, root: TradesRootResponse): Promise<void> {
  client.setQueryData(queryKeys.tradesRoot, root)
  await Promise.all([
    ...TRADE_DEPENDENT_KEYS.map((queryKey) => client.invalidateQueries({ queryKey })),
    client.invalidateQueries({ queryKey: queryKeys.config }),
  ])
  // Fetched, not read from cache: the folder dialog can be the only thing
  // mounted, and an inactive query is invalidated without being refetched.
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

/**
 * `clear_trades_root` — Clear. It stops displaying the trades: the chosen
 * folder is forgotten and the book falls back to `[trades] root`, the demo
 * book, or nothing. **No file of the user's is read, moved or deleted.**
 */
export function useClearTrades(): UseMutationResult<TradesRootResponse, Error, void> {
  const client = useQueryClient()
  return useMutation({
    mutationFn: () => call<TradesRootResponse>('clear_trades_root'),
    onSuccess: (data) => adoptNewRoot(client, data),
  })
}

/** `probe_folder` — the dialog's summary. Persists nothing. */
export function useProbeFolder(): UseMutationResult<FolderProbe, Error, string> {
  return useMutation({
    mutationFn: (root: string) => call<FolderProbe>('probe_folder', { request: { root } }),
    // A probe changes nothing, so nothing is invalidated.
  })
}

/** `set_trades_root` — load the chosen folder. */
export function useSetTradesRoot(): UseMutationResult<TradesRootResponse, Error, string> {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (root: string) => call<TradesRootResponse>('set_trades_root', { request: { root } }),
    onSuccess: (data) => adoptNewRoot(client, data),
  })
}

/** The OS folder chooser (Tauri only). `null` = the user cancelled. */
export function usePickFolder(): UseMutationResult<string | null, Error, string | undefined> {
  return useMutation({
    mutationFn: (start?: string) => call<string | null>('pick_folder', { request: { start } }),
  })
}

export function useReload(): UseMutationResult<ReloadResponse, Error, void> {
  const client = useQueryClient()
  return useMutation({
    mutationFn: () => call<ReloadResponse>('reload'),
    onSuccess: () => {
      void client.invalidateQueries()
    },
  })
}
