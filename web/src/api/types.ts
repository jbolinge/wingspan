/**
 * TypeScript mirror of `docs/API.md` — the frozen contract this SPA shares
 * with the desktop app's Tauri commands and with `wingspan-devserver`'s
 * `/api/v1`. Change `docs/API.md` first; this file follows it field for field.
 *
 * Money fields arrive as JSON numbers (serialized `Decimal`); vols and greeks
 * are floats. Timestamps are RFC 3339 UTC strings unless the field name says
 * "date", in which case it is a naive `YYYY-MM-DD`.
 */

/** RFC 3339 UTC instant, e.g. `2026-08-31T14:30:02Z`. */
export type Instant = string
/** Naive calendar date, `YYYY-MM-DD`. */
export type IsoDate = string

export interface Health {
  status: string
  version: string
  ready: boolean
}

export const SPOT_SOURCES = ['schwab', 'yfinance', 'cache', 'statement', 'fixture'] as const
export type QuoteSource = (typeof SPOT_SOURCES)[number]

export interface SpotQuote {
  symbol: string
  last: number
  change: number
  as_of: Instant
}

export interface MarketSnapshot {
  as_of: Instant
  spot_source: QuoteSource
  option_source: QuoteSource
  degraded: string[]
  spot: SpotQuote | null
  quote_count: number
  market_open: boolean
}

/**
 * delta/gamma per 1 index point; vega per 1.00 vol point;
 * theta = reprice at T-1 calendar day.
 */
export interface Greeks {
  delta: number
  gamma: number
  theta: number
  vega: number
}

export type Right = 'put' | 'call'
export type Settlement = 'am' | 'pm'

export interface OptionContract {
  underlying: string
  root: string
  expiry: IsoDate
  settlement: Settlement
  strike: number
  right: Right
  multiplier: number
  osi: string
  tos_code: string
}

export interface Leg {
  contract: OptionContract
  qty: number
  /** null for legs contributed by an unmatched order. */
  avg_cost: number | null
  mark: number | null
  bid: number | null
  ask: number | null
  /** Our solved IV, not the provider hint. */
  iv: number | null
  /** Per-contract greeks — NOT multiplied by qty. */
  greeks: Greeks
  dte: number
  quote_as_of: Instant | null
  quote_source: QuoteSource | null
}

export type WarningSeverity = 'info' | 'warn' | 'error'

export interface Warning {
  severity: WarningSeverity
  code: string
  message: string
  trade_id: string | null
  contract: string | null
}

export interface QuarantinedOrder {
  raw: string
  reason: string
  /** Execution the order collided with, when known. */
  claimed_by: string | null
}

export type TradeStatus = 'open' | 'closed'

export interface Trade {
  id: string
  name: string
  status: TradeStatus
  opened_on: IsoDate
  closed_on: IsoDate | null
  /** Calendar days in trade. */
  dit: number
  /** Days to the front expiry of open legs; null when nothing is open. */
  dte: number | null
  structure: string
  /** |net premium| — debit positive, credit negative per the label. */
  cost: number
  value: number
  pnl_open: number
  pnl_total: number
  /** % of cost basis; null when the basis is zero. */
  pnl_pct: number | null
  realized: number
  fees: number
  greeks: Greeks
  weighted_vega: number
  /** Age in seconds of the worst quote backing the P&L. */
  staleness_secs: number | null
  warnings: Warning[]
  legs: Leg[]
  quarantined: QuarantinedOrder[]
}

export interface PortfolioStats {
  count: number
  pnl_open: number
  pnl_total: number
  greeks: Greeks
  weighted_vega: number
  /** e.g. `front_expiry (2026-09-18)` — shown next to the W.V column. */
  weighted_vega_reference: string
}

export interface TradesResponse {
  as_of: Instant
  trades: Trade[]
  portfolio_all: PortfolioStats
  warning_count: number
}

export interface ClosedTotals {
  /** Σ of each trade's `realized` — pre-fee, like the per-row column. */
  realized: number
  fees: number
  /** Σ of each trade's `pnl_total`, i.e. `realized − fees`. */
  pnl_total: number
  count: number
  win_rate: number
  avg_days_held: number
}

export interface ClosedTradesResponse {
  trades: Trade[]
  totals: ClosedTotals
}

export type PriceRange = { min: number; max: number } | { pct: number }

export interface RiskGraphRequest {
  trade_ids: string[]
  price_range: PriceRange | null
  points: number
  /** `null` = server preset; otherwise `today` | `+12d` | `expiry:2027-02-19`. */
  dates: string[] | null
  /** Global vol shift in vol points (0.01 = 1 vol pt). */
  vol_shift: number
  vol_shift_by_expiry: Record<string, number>
  spot_override: number | null
  include_realized: boolean
}

export type RiskLineKind = 't0' | 'tplus' | 'expiry'

export interface RiskLine {
  date: IsoDate
  kind: RiskLineKind
  /**
   * Stable, unique line identity (`t0`, `t+12`, `expiry`,
   * `expiry:2027-02-19`) — also the `breakevens` key. The date is NOT unique:
   * a 0-DTE position puts the T+0 and expiration lines on the same day.
   */
  key: string
  /** Legend text the server suggests; the UI renders `lineLabel` instead. */
  label: string
  pnl: number[]
}

export interface PerTradeLine {
  trade_id: string
  date: IsoDate
  /** `key` of the aggregate line this curve decomposes. */
  key: string
  pnl: number[]
}

export interface SigmaBands {
  one: [number, number]
  two: [number, number]
}

export interface RiskGraphResponse {
  as_of: Instant
  spot: number
  prices: number[]
  lines: RiskLine[]
  per_trade: PerTradeLine[]
  breakevens: Record<string, number[]>
  max_profit_on_range: number
  max_loss_on_range: number
  /** P(profit) at expiry, lognormal. */
  pop: number
  sigma_bands: SigmaBands
  front_atm_iv: number
  /** Model-side, keyed to a contract — e.g. `iv_solve_failed`, `missing_quote`. */
  warnings: Warning[]
}

export interface WhatIfRequest {
  trade_ids: string[]
  price: number
  date: IsoDate
  vol_shift: number
  include_realized: boolean
}

export interface WhatIfResponse {
  pnl: number
  greeks: Greeks
}

export type ReconciliationStatus = 'match' | 'qty_mismatch' | 'unassigned' | 'missing'

export interface ReconciliationRow {
  tos_code: string
  statement_qty: number
  trades_qty: number
  status: ReconciliationStatus
}

export interface ReconciliationResponse {
  statement_as_of: IsoDate | null
  rows: ReconciliationRow[]
  note: string
}

export interface WarningsResponse {
  warnings: Warning[]
}

/** Resolved config with secrets redacted; shape is deliberately open. */
export type ConfigResponse = Record<string, unknown>

export interface ReloadResponse {
  reloaded: boolean
  trades: number
}

/** RFC 9457 problem document. */
export interface ProblemJson {
  type?: string
  title?: string
  status?: number
  detail?: string
}

/* ---------- SSE payloads (GET /api/v1/events) ---------- */

export type TradesChangedReason = 'fs' | 'reload'

export interface TradesChangedEvent {
  reason: TradesChangedReason
}

export interface ProviderEvent {
  spot_source: QuoteSource
  option_source: QuoteSource
  degraded: string[]
}

/* ---------- Which book is loaded ---------- */

/**
 * How the loaded book was decided. `local` is a folder on this computer, read
 * in place and watched — the ordinary one. `config` and `demo` are the
 * deployer's own folder and `--demo`'s bundled book, and `empty` means
 * nothing is loaded at all (the state a Clear leaves behind with neither of
 * those to fall back to).
 */
export type TradesRootSource = 'config' | 'demo' | 'local' | 'empty'

/** `GET`/`DELETE /api/v1/trades/root`. */
export interface TradesRootResponse {
  /**
   * The active root, resolved by the same discovery rules as startup; null
   * when `source` is `empty`.
   */
  root: string | null
  source: TradesRootSource
  /** What config alone would use; null when config sets none. */
  config_root: string | null
  open_trades: number
  closed_trades: number
  /** RFC 3339 instant the folder was chosen; present only while `source` is `local`. */
  chosen_at?: string | null
  /** Whether the folder is being watched for changes. */
  watching: boolean
  /** Recently forgotten folders, most-recent-first; may be empty. */
  recent: string[]
}

/** `POST /api/v1/trades/root/probe` — validate-only: nothing is persisted,
 *  loaded or watched. 422 `invalid-trades-root` carries the discovery sentence. */
export interface FolderProbe {
  root: string
  open_trades: number
  closed_trades: number
  statements: number
  note?: string | null
}

/** `PUT /api/v1/trades/root`. */
export interface TradesRootRequest {
  root: string
}
