/** TanStack Query bindings for the app's commands (see `client.ts`). */

import {
  useMutation,
  useQuery,
  useQueryClient,
  type QueryClient,
  type UseMutationResult,
  type UseQueryResult,
} from '@tanstack/react-query'
import { isTauri } from '../lib/tauri'
import { useAppStore } from '../store/appStore'
import { call } from './client'
import type {
  ClosedTradesResponse,
  ConfigResponse,
  DiagnosticsResponse,
  FolderProbe,
  MarketSnapshot,
  PickedPath,
  PortfolioStats,
  PythonProbe,
  ReconciliationResponse,
  ReloadResponse,
  RiskGraphRequest,
  RiskGraphResponse,
  SettingsPatch,
  SettingsRead,
  SettingsUpdateResponse,
  TradesResponse,
  TradesRootResponse,
  WarningsResponse,
  WhatIfRequest,
  WhatIfResponse,
} from './types'

export const queryKeys = {
  snapshot: ['market', 'snapshot'] as const,
  trades: ['trades'] as const,
  closed: ['trades', 'closed'] as const,
  portfolio: (ids: readonly string[]) => ['portfolio', [...ids].sort().join(',')] as const,
  riskGraph: (request: RiskGraphRequest) => ['risk-graph', request] as const,
  whatIf: (request: WhatIfRequest) => ['what-if', request] as const,
  reconciliation: ['reconciliation'] as const,
  tradesRoot: ['trades', 'root'] as const,
  warnings: ['warnings'] as const,
  config: ['config'] as const,
  settings: ['settings'] as const,
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
export async function adoptNewRoot(client: QueryClient, root: TradesRootResponse): Promise<void> {
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

/* ---------- Settings & diagnostics (desktop only) ---------- */

/**
 * `settings_read` — the current `wingspan.toml` values plus the read-only
 * paths and the Schwab token permission check. Desktop only: there is no
 * HTTP route, so the query is disabled outside Tauri and `SettingsCard`
 * renders a read-only note there instead.
 */
export function useSettings(): UseQueryResult<SettingsRead> {
  return useQuery({
    queryKey: queryKeys.settings,
    queryFn: () => call<SettingsRead>('settings_read'),
    enabled: isTauri(),
  })
}

/**
 * `settings_update` — a typed, partial patch; only the changed groups/fields
 * are sent so a comment or a field this form doesn't know about survives.
 * Writing settings rebuilds the whole runtime on the Rust side (root, market
 * data, pricing can all change), so every trade-dependent key is invalidated
 * alongside `settings` and `config`.
 */
export function useUpdateSettings(): UseMutationResult<SettingsUpdateResponse, Error, SettingsPatch> {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (request: SettingsPatch) =>
      call<SettingsUpdateResponse>('settings_update', { request }),
    onSuccess: () => {
      void client.invalidateQueries({ queryKey: queryKeys.settings })
      void client.invalidateQueries({ queryKey: queryKeys.config })
      for (const queryKey of TRADE_DEPENDENT_KEYS) void client.invalidateQueries({ queryKey })
    },
  })
}

/**
 * `diagnostics` — the checklist `wingspan-cli doctor` also renders. `probe`
 * re-runs the live checks (Schwab reachability, yfinance) rather than only
 * the cheap static ones; `staleTime: 0` so "Run checks" always re-queries
 * instead of reusing a stale probe.
 */
export function useDiagnostics(probe: boolean, enabled: boolean): UseQueryResult<DiagnosticsResponse> {
  return useQuery({
    queryKey: ['diagnostics', probe] as const,
    queryFn: () => call<DiagnosticsResponse>('diagnostics', { request: { probe } }),
    enabled: enabled && isTauri(),
    staleTime: 0,
  })
}

/** `probe_python` — never blocks Save; the card renders `{ok, detail}` as a
 *  pass/warn line next to the interpreter path. */
export function useProbePython(): UseMutationResult<PythonProbe, Error, void> {
  return useMutation({
    mutationFn: () => call<PythonProbe>('probe_python'),
  })
}

/**
 * The three path pickers: each opens a Rust-side dialog, and — unlike
 * `pick_folder` — the chosen path is already confirmed, persisted **and
 * applied** by the time the command returns, so a non-null result invalidates
 * `settings` and `config` rather than being probed first. `applied: false`
 * means the file was written but this session was not rebuilt on it; the card
 * shows `detail` beside the control rather than pretending it took.
 */
function usePathPicker(command: 'pick_python' | 'pick_env_file' | 'pick_token_file') {
  const client = useQueryClient()
  return useMutation({
    mutationFn: () => call<PickedPath | null>(command),
    onSuccess: (picked) => {
      if (picked === null) return
      void client.invalidateQueries({ queryKey: queryKeys.settings })
      void client.invalidateQueries({ queryKey: queryKeys.config })
    },
  })
}

export function usePickPython(): UseMutationResult<PickedPath | null, Error, void> {
  return usePathPicker('pick_python')
}

export function usePickEnvFile(): UseMutationResult<PickedPath | null, Error, void> {
  return usePathPicker('pick_env_file')
}

export function usePickTokenFile(): UseMutationResult<PickedPath | null, Error, void> {
  return usePathPicker('pick_token_file')
}

/** `load_demo` / `leave_demo` rebuild the runtime with the demo book (or back
 *  out of it), so they adopt the new root exactly like a folder pick does. */
export function useLoadDemo(): UseMutationResult<TradesRootResponse, Error, void> {
  const client = useQueryClient()
  return useMutation({
    mutationFn: () => call<TradesRootResponse>('load_demo'),
    onSuccess: (data) => adoptNewRoot(client, data),
  })
}

export function useLeaveDemo(): UseMutationResult<TradesRootResponse, Error, void> {
  const client = useQueryClient()
  return useMutation({
    mutationFn: () => call<TradesRootResponse>('leave_demo'),
    onSuccess: (data) => adoptNewRoot(client, data),
  })
}

export function useRevealFolder(): UseMutationResult<null, Error, string> {
  return useMutation({
    mutationFn: (path: string) => call<null>('reveal_folder', { request: { path } }),
  })
}

export function useOpenLogFolder(): UseMutationResult<null, Error, void> {
  return useMutation({ mutationFn: () => call<null>('open_log_folder') })
}

export function useOpenConfigFile(): UseMutationResult<null, Error, void> {
  return useMutation({ mutationFn: () => call<null>('open_config_file') })
}
