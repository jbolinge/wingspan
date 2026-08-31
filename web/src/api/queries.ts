/** TanStack Query bindings for the `/api/v1` contract. */

import {
  useMutation,
  useQuery,
  useQueryClient,
  type UseMutationResult,
  type UseQueryResult,
} from '@tanstack/react-query'
import { apiGet, apiPost } from './client'
import type {
  ClosedTradesResponse,
  ConfigResponse,
  Health,
  MarketSnapshot,
  PortfolioStats,
  ReconciliationResponse,
  ReloadResponse,
  RiskGraphRequest,
  RiskGraphResponse,
  Trade,
  TradesResponse,
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

export function useTrades(): UseQueryResult<TradesResponse> {
  return useQuery({
    queryKey: queryKeys.trades,
    queryFn: () => apiGet<TradesResponse>('/trades'),
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

export function useReload(): UseMutationResult<ReloadResponse, Error, void> {
  const client = useQueryClient()
  return useMutation({
    mutationFn: () => apiPost<ReloadResponse>('/reload', {}),
    onSuccess: () => {
      void client.invalidateQueries()
    },
  })
}
