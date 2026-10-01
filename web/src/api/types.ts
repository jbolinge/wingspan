/**
 * TypeScript mirror of `docs/IPC.md` — the frozen contract this SPA shares
 * with the desktop app's Tauri commands and with `wingspan-devserver`'s
 * `/api/v1`. Change `docs/IPC.md` first; this file follows it field for field.
 *
 * Money fields arrive as JSON numbers (serialized `Decimal`); vols and greeks
 * are floats. Timestamps are RFC 3339 UTC strings unless the field name says
 * "date", in which case it is a naive `YYYY-MM-DD`.
 */

/** RFC 3339 UTC instant, e.g. `2026-08-31T14:30:02Z`. */
export type Instant = string
/** Naive calendar date, `YYYY-MM-DD`. */
export type IsoDate = string

export const SPOT_SOURCES = [
  'schwab',
  'tastytrade',
  'yfinance',
  'cache',
  'statement',
  'fixture',
] as const
export type QuoteSource = (typeof SPOT_SOURCES)[number]

export interface SpotQuote {
  symbol: string
  last: number
  change: number
  as_of: Instant
}

export interface MarketSnapshot {
  /** When the ladder last ran — a refresh time, never a data age: a snapshot
   *  served wholly from the cache still stamps *now* here. */
  as_of: Instant
  /** Oldest timestamp on the data actually behind the snapshot (the spot and
   *  every quote). This is what the provider pill ages off. */
  data_as_of: Instant
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
  /** Against `TradesResponse.portfolio_all.weighted_vega_reference`, so the
   *  rows add up to `portfolio_all.weighted_vega`. */
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
  /** e.g. `30d` (default) or `front_expiry (2026-09-18)` — shown next to the
   *  W.V column. Every `Trade.weighted_vega` in the same response is measured
   *  against `portfolio_all`'s. */
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
  /** Modeled legs priced at the current mark and drawn as one synthetic
   *  position (`trade_id: "model"` in `per_trade`). `[]` when the model is
   *  off — never absent, so the query key is the same shape either way. */
  model_legs: ModelLeg[]
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

/**
 * T+0 greeks over the shared price axis — one value per `prices[i]`, in
 * dollars (already scaled by qty × multiplier). Entries the engine could not
 * produce arrive as JSON `null`, so every read guards with `Number.isFinite`.
 */
export interface RiskGreeks {
  /** `key` of the line these belong to — always `t0`. */
  key: string
  /** $ per 1 index point. */
  delta: number[]
  /** $ of delta change per 1 index point. */
  gamma: number[]
  /** $ per calendar day (reprice at T+1d, ONE-style). */
  theta: number[]
  /** $ per 1.00 vol point. */
  vega: number[]
  /** $ per 1.00 vol point of the reference expiry. */
  weighted_vega: number[]
  /** e.g. `30d` or `front_expiry (2026-10-16)`. */
  weighted_vega_reference: string
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
  /** `null` when the date list has no T+0 line. */
  t0_greeks: RiskGreeks | null
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
  /** Same meaning as on `RiskGraphRequest`, so the what-if answer and the
   *  curves beside it describe the same position. */
  model_legs: ModelLeg[]
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
  /** Set on an honestly-empty reconciliation from a Legs sheet/CSV source —
   *  there is no account statement to check against, only the sheet's own
   *  values, so an empty `rows` list is not itself a clean bill of health. */
  source_note: string | null
}

export interface WarningsResponse {
  warnings: Warning[]
}

/** Resolved config with secrets redacted; shape is deliberately open. */
export type ConfigResponse = Record<string, unknown>

/* ---------- Settings (desktop only) ---------- */

/**
 * Every field optional: `settings_update` sends only what changed, so an
 * untouched field survives as a comment/value `toml_edit` leaves alone.
 * There is deliberately **no** `python`/`token_path`/`env_file`/
 * `tastytrade.env_file`/`token_config`/`script`/`state_dir`/`trades.root`
 * field — those are path fields, set only through the Rust-side pickers,
 * never written from JS.
 */
export interface SettingsPatch {
  trades?: {
    /** The clock leg dates and statement rows are read in (was `statement_timezone`). */
    timezone?: string
    /** The sheet's tab name (deployer default; the in-app choice outranks it). */
    tab?: string
    /** 0 = manual Reload only; floor 60 otherwise. */
    poll_secs?: number
    /** Whether the fetched bytes are cached under the state dir. */
    cache?: boolean
  }
  schwab?: {
    token_config_key?: string
    app_key_env?: string
    app_secret_env?: string
    refresh?: boolean
  }
  tastytrade?: {
    enabled?: boolean
    client_id_env?: string
    client_secret_env?: string
    refresh_token_env?: string
    sandbox?: boolean
    delayed?: boolean
    api_version?: string
  }
  yfinance?: { enabled?: boolean }
  market?: {
    spot_sources?: string[]
    option_sources?: string[]
    refresh_secs_open?: number
    refresh_secs_closed?: number
    pause_when_hidden?: boolean
  }
  pricing?: {
    risk_free_rate?: number
    dividend_yield?: number
    weighted_vega_reference?: string
    weighted_vega_exponent?: number
  }
  risk_graph?: { points?: number; range_pct?: number; tplus_preset?: string }
}

/** `settings_read` — the settings form's data source; read-only display paths
 *  live in `paths`, never in `settings` itself. */
export interface SettingsRead {
  /** Path `wingspan.toml` was read from, or `null` when none exists yet. */
  file: string | null
  settings: SettingsPatch
  paths: {
    /** The active trade source's display label, read-only. */
    source_label: string | null
    token_path: string | null
    /** Schwab's — the name is frozen. */
    env_file: string | null
    python: string | null
    tastytrade_env_file: string | null
  }
  /** Schwab's token file, and only Schwab's. */
  token_status: { present: boolean; world_readable: boolean; detail: string } | null
  /** `null` when tastytrade is disabled or unconfigured; never a credential. */
  tastytrade_status: {
    configured: boolean
    sandbox: boolean
    delayed: boolean
    authorized: boolean
    detail: string
  } | null
  /** What `wingspan.toml` alone would use, without ever naming the sheet id. */
  source_status: SourceStatus | null
}

/** `settings_update` response — the file the patch was written to. */
export interface SettingsUpdateResponse {
  file: string
}

export type DiagnosticsGrade = 'ok' | 'warn' | 'fail' | 'note'

export interface DiagnosticsRow {
  grade: DiagnosticsGrade
  section: string
  name: string
  detail: string
}

export interface DiagnosticsResponse {
  rows: DiagnosticsRow[]
}

export interface PythonProbe {
  ok: boolean
  detail: string
}

/**
 * What `pick_python` / `pick_env_file` / `pick_token_file` /
 * `pick_tastytrade_env_file` answer. `null` (rather than this) is a cancel.
 *
 * The path is written to `wingspan.toml` before the application is rebuilt on
 * it, so the two can disagree: `applied: false` means the file holds the new
 * path, this session is still running on the old one, and `detail` says why.
 */
export interface PickedPath {
  path: string
  applied: boolean
  detail: string | null
}

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

/* ---------- Event payloads (Tauri events; SSE in the dev harness) ---------- */

export interface ProviderEvent {
  spot_source: QuoteSource
  option_source: QuoteSource
  degraded: string[]
}

/* ---------- The trade source ---------- */

/**
 * How the active book is fed. `sheet` and `csv` are the two live paths (a
 * shared Google Sheet fetched unauthenticated, or a CSV of the same Legs
 * shape); `config` is a deployer-set default source not yet chosen in-app;
 * `demo` is the bundled fixture book; `empty` is nothing loaded at all.
 *
 * `'manual'` is the reserved (not yet built) hand-entry seam a client must
 * still render rather than drop — `set_trade_source` answers 422 for it today.
 */
export type TradeSourceKind = 'sheet' | 'csv' | 'config' | 'demo' | 'empty' | 'manual'

export interface RecentSource {
  kind: TradeSourceKind
  label: string
  tab: string | null
}

/** `GET`/`PUT`/`DELETE /api/v1/trades/source`. */
export interface TradeSourceResponse {
  kind: TradeSourceKind
  /** Masked for a sheet (never a full spreadsheet id); null when `kind` is `empty`. */
  label: string | null
  /** Non-null only for a sheet (including a `config` one). */
  tab: string | null
  /** The CSV's full path; null for a sheet. This — not `kind` — is what says
   *  whether the book came off a file or off a Google Sheet, because a
   *  deployer-configured source of either sort answers `kind: "config"`. */
  path: string | null
  open_trades: number
  closed_trades: number
  legs: number
  rows_skipped: number
  chosen_at: string | null
  fetched_at: string | null
  stale: boolean
  detail: string | null
  auto_refresh_secs: number | null
  recent: RecentSource[]
}

export interface ProbeTrades {
  open: number
  closed: number
}

/** `POST /api/v1/trades/source/probe` — validate-only: nothing is persisted,
 *  loaded or fetched into the running book. */
export interface TradeSourceProbe {
  label: string
  tab: string | null
  columns_found: string[]
  /** Non-empty ⇒ the import would be refused. */
  columns_missing: string[]
  /** Expected-but-blank ⇒ imported without cost basis / DIT. */
  columns_absent: string[]
  columns_ignored: string[]
  rows: number
  legs: number
  rows_skipped: number
  trades: ProbeTrades
  warnings: Warning[]
  /** Absent unless there is something worth reading first — the one field of
   *  this response that is omitted rather than `null` (`docs/IPC.md`). A
   *  published sheet link, which cannot select a tab by name, is what
   *  produces one today. */
  note?: string | null
}

/** `POST /trades/source/probe` and `PUT /trades/source` request body's `source` field. */
export type TradeSourceRequest =
  | { kind: 'sheet'; url: string; tab?: string }
  | { kind: 'csv'; path: string }
  | { kind: 'manual' }

/** `pick_csv_file` (desktop only): the native chooser's answer, or `null` on a cancel. */
export interface PickedCsv {
  path: string
}

/** Part of `settings_read`: what `wingspan.toml` alone would use, without
 *  ever naming the sheet id. */
export interface SourceStatus {
  kind: TradeSourceKind
  configured: boolean
  /** `"docs.google.com"` or `null` — never the spreadsheet id. */
  host: string | null
  tab: string | null
}

/* ---------- The option chain and modeled legs ---------- */

/**
 * How an expiration is listed. An open set — the server may grow a kind, and
 * the strip renders whatever arrives.
 */
export type ExpiryKind = 'daily' | 'weekly' | 'monthly' | 'quarterly' | 'eom' | 'unknown' | (string & {})

/**
 * One listed expiration. Identified by `(expiry, root)`: a third Friday is
 * listed twice, `SPX`/AM and `SPXW`/PM.
 */
export interface ChainExpiry {
  expiry: IsoDate
  /** Calendar days from the valuation date; 0 on expiry day. */
  dte: number
  root: string
  settlement: Settlement
  kind: ExpiryKind
}

/** `chain_expirations` — `GET /api/v1/chain/expirations`. */
export interface ChainExpirationsResponse {
  /** When this list was fetched. */
  as_of: Instant
  /** The provider that answered; `fixture` for the demo. */
  source: string
  underlying: string
  /** True when served from the in-memory cache after a failed refetch. */
  stale: boolean
  /** Every provider skipped or failed, one sentence each. */
  degraded: string[]
  /** Sorted by expiry, then root (`SPX` before `SPXW`); unexpired only. */
  expirations: ChainExpiry[]
}

/** `option_chain` — `POST /api/v1/chain`. */
export interface OptionChainRequest {
  expiry: IsoDate
  root: string
}

/** One side of one strike. `mark: null` means unpriceable — shown as "—" and
 *  never modeled. */
export interface ChainQuote {
  contract: OptionContract
  bid: number | null
  ask: number | null
  mark: number | null
  last: number | null
  /** OUR solved IV (the chain, the table and the graph agree); null when the
   *  solve failed. */
  iv: number | null
  /** The provider's own IV — diagnostics only, never displayed as ours. */
  iv_hint: number | null
  /** Per contract (qty 1), same units as `Leg.greeks`; null when `iv` is. */
  greeks: Greeks | null
  volume: number | null
  open_interest: number | null
  /** When the provider quoted this contract — always present. */
  quote_as_of: Instant
}

export interface ChainRow {
  strike: number
  call: ChainQuote | null
  put: ChainQuote | null
}

export interface OptionChainResponse {
  as_of: Instant
  source: string
  stale: boolean
  degraded: string[]
  underlying: string
  /** The current snapshot's spot — the chain never has its own. */
  spot: number
  expiry: IsoDate
  root: string
  settlement: Settlement
  dte: number
  /** The forward the IVs were solved against. */
  forward: number
  /** Strike ascending; every listed strike, even with only one side quoted. */
  rows: ChainRow[]
  /** Model-side only (`iv_solve_failed`, `iv_clamped`), keyed to a contract. */
  warnings: Warning[]
}

/**
 * A contract as the model sends it: the frozen `OptionContract` minus the
 * derived `osi`/`tos_code` (which the server ignores rather than refuses, so
 * a full `OptionContract` is accepted too).
 */
export interface ModelContract {
  underlying: string
  root: string
  expiry: IsoDate
  settlement: Settlement
  strike: number
  right: Right
  multiplier: number
}

/** A modeled leg on the wire: a non-zero integer quantity, |qty| ≤ 999. */
export interface ModelLeg {
  contract: ModelContract
  qty: number
}

/** `set_model_contracts` — `PUT /api/v1/model/contracts`. The complete set;
 *  an empty list clears the extra quote universe. */
export interface SetModelContractsRequest {
  contracts: ModelContract[]
}

export interface SetModelContractsResponse {
  contracts: number
  quoted: number
  /** TOS codes of the registered contracts the refreshed snapshot has no
   *  quote for — those are priced from neighbouring strikes. */
  missing: string[]
  refreshed: boolean
  /** One sentence when the refresh failed or timed out (the extras are still
   *  registered). */
  detail: string | null
}

/** `portfolio` — the Tauri command's request, and `POST /api/v1/portfolio`'s
 *  body. Empty `ids` with no model is the empty set here (the whole book for
 *  `risk_graph`/`what_if`); `{ ids: [], model_legs }` is the model alone, as
 *  in all three model-aware commands. */
export interface PortfolioRequest {
  ids: string[]
  model_legs: ModelLeg[]
}
