/** TanStack Query bindings for the app's commands (see `client.ts`). */

import {
  useMutation,
  useQuery,
  useQueryClient,
  type QueryClient,
  type UseMutationResult,
  type UseQueryResult,
} from '@tanstack/react-query'
import { usePickedFile } from '../lib/pickedFile'
import { isTauri } from '../lib/tauri'
import { useAppStore } from '../store/appStore'
import { call, errorDetail } from './client'
import type {
  ClosedTradesResponse,
  ConfigResponse,
  DiagnosticsResponse,
  MarketSnapshot,
  PickedCsv,
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
  TradeSourceProbe,
  TradeSourceRequest,
  TradeSourceResponse,
  TradesResponse,
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
  tradeSource: ['trades', 'source'] as const,
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

/** Everything about a risk-graph request except which window it covers. */
function graphIdentity(request: RiskGraphRequest): string {
  return JSON.stringify({ ...request, price_range: null, points: 0 })
}

/**
 * One `/risk-graph` query. `null` is a request there is nothing to make yet —
 * the panel's *view* query while the trader is looking at the plain computed
 * range — and parks the hook on a key of its own without fetching.
 *
 * `windowOnlyPlaceholder` narrows the "keep the last response on screen while
 * the next one loads" rule to responses for the *same graph*, differing only
 * in the window they cover. The view query needs that: its data is drawn
 * under the base query's header numbers, and TanStack resolves a placeholder
 * from the last response this observer saw with data — including while the
 * query is disabled — so without it a coverage response for one selection
 * could be drawn beneath another selection's readouts.
 */
export function useRiskGraph(
  request: RiskGraphRequest | null,
  enabled: boolean,
  windowOnlyPlaceholder = false,
): UseQueryResult<RiskGraphResponse> {
  return useQuery({
    queryKey: queryKeys.riskGraph(request ?? ({} as RiskGraphRequest)),
    // Forwarding TanStack's own AbortSignal means a request superseded by
    // the next slider tick (a new query key) is actually cancelled, not just
    // ignored once it resolves.
    queryFn: ({ signal }) => call<RiskGraphResponse>('risk_graph', { request }, signal),
    enabled: enabled && request !== null && request.trade_ids.length > 0,
    placeholderData: windowOnlyPlaceholder
      ? (previous, previousQuery) => {
          if (previous === undefined || request === null) return undefined
          const key = previousQuery?.queryKey as [string, RiskGraphRequest] | undefined
          const previousRequest = key?.[1]
          if (!previousRequest?.trade_ids) return undefined
          return graphIdentity(previousRequest) === graphIdentity(request) ? previous : undefined
        }
      : (previous) => previous,
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

/* ---------- the trade source (sheet / CSV) ---------- */

/**
 * Everything the book feeds, refetched after the source changed, then any
 * selection or expansion pointing at a trade the new book does not have is
 * dropped. Called by `adoptNewSource` after it sets its own query data.
 */
async function adoptBook(client: QueryClient): Promise<void> {
  await Promise.all([
    ...TRADE_DEPENDENT_KEYS.map((queryKey) => client.invalidateQueries({ queryKey })),
    client.invalidateQueries({ queryKey: queryKeys.config }),
  ])
  // Fetched, not read from cache: the import dialog can be the only thing
  // mounted, and an inactive query is invalidated without being refetched.
  //
  // **A book that cannot be priced is still adopted.** `trades` answers 503
  // until some market source works, which is the ordinary state of a fresh
  // install — and letting that throw here would reject this promise, flip the
  // caller's mutation to `error`, and leave "Load & analyze" reporting a
  // market-data failure for a source it had already loaded. The new trade
  // list is only needed to prune stale selections and expansions, so without
  // it nothing is pruned and everything else stands.
  const open = await client
    .fetchQuery({ queryKey: queryKeys.trades, queryFn: fetchTrades })
    .catch(() => null)
  if (open === null) return
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
 * Everything the book feeds, refetched after the source changed, then any
 * selection or expansion pointing at a trade the new book does not have is
 * dropped.
 */
export async function adoptNewSource(client: QueryClient, source: TradeSourceResponse): Promise<void> {
  client.setQueryData(queryKeys.tradeSource, source)
  await adoptBook(client)
}

/** `trade_source` — what is loaded and how it got here. */
export function useTradeSource(): UseQueryResult<TradeSourceResponse> {
  return useQuery({
    queryKey: queryKeys.tradeSource,
    queryFn: () => call<TradeSourceResponse>('trade_source'),
  })
}

/** `probe_trade_source` — the dialog's summary. Persists nothing. */
export function useProbeTradeSource(): UseMutationResult<TradeSourceProbe, Error, TradeSourceRequest> {
  return useMutation({
    mutationFn: (source: TradeSourceRequest) =>
      call<TradeSourceProbe>('probe_trade_source', { request: { source } }),
    // A probe changes nothing, so nothing is invalidated.
  })
}

/** `set_trade_source` — commit to a sheet or a CSV: fetched (or read),
 *  cached and remembered, and applied like a restart. */
export function useSetTradeSource(): UseMutationResult<TradeSourceResponse, Error, TradeSourceRequest> {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (source: TradeSourceRequest) =>
      call<TradeSourceResponse>('set_trade_source', { request: { source } }),
    onSuccess: (data) => adoptNewSource(client, data),
  })
}

/**
 * `clear_trade_source` — Clear. It stops displaying the trades: the chosen
 * sheet or CSV is forgotten and the book falls back to config, the demo
 * book, or nothing. **No file of the user's is read, moved or deleted.**
 */
export function useClearTradeSource(): UseMutationResult<TradeSourceResponse, Error, void> {
  const client = useQueryClient()
  return useMutation({
    mutationFn: () => call<TradeSourceResponse>('clear_trade_source'),
    onSuccess: (data) => adoptNewSource(client, data),
  })
}

/**
 * The OS CSV file chooser (Tauri only). `null` = the user cancelled.
 *
 * The outcome is parked in `usePickedFile` from *mutation-level* callbacks,
 * never handed back through `mutate(vars, { onSuccess })`: the chooser can
 * outlive the dialog that opened it. See `lib/pickedFile.ts`.
 */
export function usePickCsvFile(): UseMutationResult<PickedCsv | null, Error, void> {
  return useMutation({
    mutationFn: () => call<PickedCsv | null>('pick_csv_file'),
    onSuccess: (picked) => {
      if (picked !== null) usePickedFile.getState().receive({ kind: 'file', path: picked.path })
    },
    onError: (error) => {
      usePickedFile.getState().receive({ kind: 'error', detail: errorDetail(error) })
    },
  })
}

/** `open_sheet` (desktop only) — opens the real sheet URL in the browser. */
export function useOpenSheet(): UseMutationResult<null, Error, void> {
  return useMutation({
    mutationFn: () => call<null>('open_sheet'),
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
 * The four path pickers: each opens a Rust-side dialog, and — unlike
 * `pick_csv_file` — the chosen path is already confirmed, persisted **and
 * applied** by the time the command returns, so a non-null result invalidates
 * `settings` and `config` rather than being probed first. `applied: false`
 * means the file was written but this session was not rebuilt on it; the card
 * shows `detail` beside the control rather than pretending it took.
 */
function usePathPicker(
  command: 'pick_python' | 'pick_env_file' | 'pick_token_file' | 'pick_tastytrade_env_file',
) {
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

export function usePickTastytradeEnvFile(): UseMutationResult<PickedPath | null, Error, void> {
  return usePathPicker('pick_tastytrade_env_file')
}

/** `load_demo` / `leave_demo` rebuild the runtime with the demo book (or back
 *  out of it), so they adopt the new source exactly like committing a sheet
 *  or CSV does — see docs/IPC.md §load_demo. */
export function useLoadDemo(): UseMutationResult<TradeSourceResponse, Error, void> {
  const client = useQueryClient()
  return useMutation({
    mutationFn: () => call<TradeSourceResponse>('load_demo'),
    onSuccess: (data) => adoptNewSource(client, data),
  })
}

export function useLeaveDemo(): UseMutationResult<TradeSourceResponse, Error, void> {
  const client = useQueryClient()
  return useMutation({
    mutationFn: () => call<TradeSourceResponse>('leave_demo'),
    onSuccess: (data) => adoptNewSource(client, data),
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
