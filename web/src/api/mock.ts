/**
 * Typed mock of the frozen `/api/v1` contract.
 *
 * Activated by `VITE_MOCK=1`, or automatically in dev when the API cannot be
 * reached (see `client.ts`), so every screen is demo-able and testable when no
 * dev API server is running.
 *
 * The data is *derived*, not hand-written: a small BSM surface prices the same
 * leg list into leg marks, position greeks, the risk-graph curves and the
 * what-if answer. That keeps the documented invariant true in mock mode —
 * the `t0` line evaluated at spot equals the dashboard `pnl_total` — so the UI
 * is exercised against numbers that behave like the server's.
 *
 * The book mirrors the wholly synthetic demo book in `fixtures/demo` (the short
 * October strangle, the November iron condor and the closed September put
 * spread the demo book shows) plus a few invented degraded trades —
 * a stale-quote butterfly, a cross-trade-netted calendar and a quarantined
 * closed hedge — so the warnings, quarantine and reconciliation surfaces stay
 * exercised. Every number here is made up; nothing is derived from, or
 * resembles, any real account, and no account identifiers exist here.
 */

import { bsmGreeks, bsmPrice, intrinsic, normCdf, ZERO_GREEKS } from '../lib/bsm'
import { addDays, daysBetween, todayIso, yearFraction } from '../lib/dates'
import type {
  ClosedTradesResponse,
  ConfigResponse,
  FolderProbe,
  Greeks,
  IsoDate,
  Leg,
  MarketSnapshot,
  OptionContract,
  PortfolioStats,
  ProblemJson,
  QuarantinedOrder,
  ReconciliationResponse,
  ReconciliationRow,
  ReloadResponse,
  Right,
  RiskGraphRequest,
  RiskGraphResponse,
  RiskGreeks,
  RiskLine,
  RiskLineKind,
  Settlement,
  Trade,
  TradesResponse,
  TradesRootResponse,
  Warning,
  WarningsResponse,
  WhatIfRequest,
  WhatIfResponse,
} from './types'

const MULT = 100
const RATE = 0.042
const DIV = 0.013
const WV_EXPONENT = 0.5

/**
 * `[trades] root` is what a config file alone would use. The ordinary source
 * is a folder the user chose on this computer, read in place and watched;
 * there is no route that lists this machine's own disk.
 */
const MOCK_CONFIG_ROOT = '/home/trader/workspace/tos-exports/Trades'
const MOCK_STATE_DIR = '/home/trader/.local/share/com.jbolinge.wingspan/state'
const MOCK_TOKEN_PATH = '/home/trader/.config/wingspan/schwab_token.json'

/** Mutable so the simulated SSE feed can walk the tape. */
let spot = 7750
let spotChange = -12.5
const BASE_SPOT = spot

/* ------------------------------------------------------------------ *
 * Volatility surface
 * ------------------------------------------------------------------ */

/** Rich near-dated, decaying to a long-dated floor — the demo book's term. */
const ATM_LONG = 0.13
const ATM_SPREAD = 0.055
const TERM_TAU = 0.6

/** Raw-SVI wing shape, put-skewed (`rho < 0`), as in the demo surface. */
const SVI_B = 0.6
const SVI_RHO = -0.5
const SVI_SIGMA = 1.5

/** ATM term structure: `0.185` short-dated decaying to `0.130`. */
function atmVol(t: number): number {
  return ATM_LONG + ATM_SPREAD * Math.exp(-Math.max(t, 0) / TERM_TAU)
}

/**
 * A replica of the server's fixture surface (`wingspan-marketdata`'s
 * `FixtureSource`, which prices `fixtures/demo`): raw SVI in standardised
 * moneyness around the forward, so the demo trades priced here land in the
 * same ballpark as the ones `serve --demo` reports.
 */
export function surfaceIv(strike: number, underlying: number, t: number): number {
  const tt = Math.max(t, 1 / 2920)
  const v = atmVol(tt)
  const forward = underlying * Math.exp((RATE - DIV) * tt)
  const k = Math.log(strike / forward)
  const c = -k / (v * Math.sqrt(tt))
  const factor = 1 + SVI_B * (-SVI_RHO * c + Math.sqrt(c * c + SVI_SIGMA * SVI_SIGMA) - SVI_SIGMA)
  return Math.min(1.5, Math.max(0.07, v * Math.sqrt(Math.max(factor, 0))))
}

/* ------------------------------------------------------------------ *
 * Contract helpers
 * ------------------------------------------------------------------ */

function osiSymbol(root: string, expiry: IsoDate, right: Right, strike: number): string {
  const yymmdd = expiry.slice(2).replaceAll('-', '')
  const thousandths = String(Math.round(strike * 1000)).padStart(8, '0')
  return `${root.padEnd(6, ' ')}${yymmdd}${right === 'call' ? 'C' : 'P'}${thousandths}`
}

function tosCode(root: string, expiry: IsoDate, right: Right, strike: number): string {
  const yymmdd = expiry.slice(2).replaceAll('-', '')
  return `${root}${yymmdd}${right === 'call' ? 'C' : 'P'}${strike}`
}

function contract(
  expiry: IsoDate,
  strike: number,
  right: Right,
  root = 'SPX',
  settlement: Settlement = 'am',
): OptionContract {
  return {
    underlying: 'SPX',
    root,
    expiry,
    settlement,
    strike,
    right,
    multiplier: MULT,
    osi: osiSymbol(root, expiry, right, strike),
    tos_code: tosCode(root, expiry, right, strike),
  }
}

/* ------------------------------------------------------------------ *
 * Trade specifications
 * ------------------------------------------------------------------ */

interface LegSpec {
  expiry: IsoDate
  strike: number
  right: Right
  qty: number
  root?: string
  settlement?: Settlement
  /** Overrides the modelled entry price where the demo book pins a fill. */
  avgCost?: number
}

interface TradeSpec {
  id: string
  name: string
  status: 'open' | 'closed'
  opened_on: IsoDate
  closed_on?: IsoDate
  structure: string
  /** Index level on the open date — drives the modelled cost basis. */
  spotAtOpen: number
  legs: LegSpec[]
  realized: number
  fees: number
  /** Age of the worst quote behind this row, seconds. */
  staleness: number
  warnings?: Warning[]
  quarantined?: QuarantinedOrder[]
}

const OCT = '2026-10-16'
const NOV = '2026-11-20'
const DEC = '2026-12-18'
const JAN = '2027-01-15'

function quarantinedOrder(raw: string, claimedBy: string): QuarantinedOrder {
  return {
    raw,
    reason: 'duplicate execution claim',
    claimed_by: claimedBy,
  }
}

const TRADE_SPECS: TradeSpec[] = [
  // --- the demo book (`fixtures/demo`, mirrored fill for fill) ---------
  {
    id: 'oct-strangle-2026-08-24',
    name: 'Oct Strangle',
    status: 'open',
    opened_on: '2026-08-24',
    structure: '-1x STRG 8200/7200 OCT26',
    spotAtOpen: 7760,
    staleness: 14,
    legs: [
      { expiry: OCT, strike: 8200, right: 'call', qty: -1, avgCost: 56.45 },
      { expiry: OCT, strike: 7200, right: 'put', qty: -1, avgCost: 57.75 },
    ],
    realized: 0,
    fees: 2.52,
  },
  {
    id: 'nov-condor-2026-08-26',
    name: 'Nov Condor',
    status: 'open',
    opened_on: '2026-08-26',
    structure: '-2x IC 8400/8500/7000/6900 NOV26',
    spotAtOpen: 7742,
    staleness: 14,
    legs: [
      { expiry: NOV, strike: 8400, right: 'call', qty: -2, avgCost: 52.1 },
      { expiry: NOV, strike: 8500, right: 'call', qty: 2, avgCost: 39.0 },
      { expiry: NOV, strike: 7000, right: 'put', qty: -2, avgCost: 62.35 },
      { expiry: NOV, strike: 6900, right: 'put', qty: 2, avgCost: 53.25 },
    ],
    realized: 0,
    fees: 10.08,
  },
  {
    id: 'sep-put-spread-2026-08-10',
    name: 'Sep Put Spread',
    status: 'closed',
    opened_on: '2026-08-10',
    closed_on: '2026-08-21',
    structure: '-2x VERT 7400/7350P SEP26 (closed)',
    spotAtOpen: 7690,
    staleness: 14,
    legs: [],
    // Sold at 9.00, bought back at 6.80, two lots: +440 less 10.08 of fees.
    realized: 440,
    fees: 10.08,
  },
  // --- invented degraded trades: warnings, quarantine, reconciliation ---
  {
    id: 'dec-fly-2026-08-29',
    name: 'Dec Fly',
    status: 'open',
    opened_on: '2026-08-29',
    structure: '2x BF 7700/7500/7300 PUT DEC26 (rolled from SEP26 7500)',
    spotAtOpen: 7748,
    // The stale row: worst quote behind it is 21 minutes old.
    staleness: 1_260,
    legs: [
      { expiry: DEC, strike: 7700, right: 'put', qty: 2 },
      { expiry: DEC, strike: 7500, right: 'put', qty: -4 },
      { expiry: DEC, strike: 7300, right: 'put', qty: 2 },
    ],
    realized: 262,
    fees: 12.6,
    warnings: [
      {
        severity: 'warn',
        code: 'date_outside_window',
        message:
          'File name date 2026-08-29 is a Saturday; the matched fills are dated 2026-08-28. Opened-on (and therefore DIT) uses the file name, per your designation.',
        trade_id: 'dec-fly-2026-08-29',
        contract: null,
      },
      {
        severity: 'info',
        code: 'order_rolled',
        message:
          'The 18 SEP 26 7500 PUT body opened 2026-08-28 at 21.40 and closed 2026-08-31 at 18.75 inside this trade; realised P&L is carried on the trade, not the leg.',
        trade_id: 'dec-fly-2026-08-29',
        contract: 'SPX260918P7500',
      },
    ],
  },
  {
    id: 'jan-calendar-2026-08-20',
    name: 'Jan Calendar',
    status: 'open',
    opened_on: '2026-08-20',
    structure: '2x CAL 7400 PUT JAN27/DEC26',
    spotAtOpen: 7730,
    staleness: 14,
    legs: [
      { expiry: JAN, strike: 7400, right: 'put', qty: 2 },
      { expiry: DEC, strike: 7400, right: 'put', qty: -2 },
    ],
    realized: 0,
    fees: 5.04,
    warnings: [
      {
        severity: 'info',
        code: 'cross_trade_netting',
        message:
          'The account netted this contract across trades: Aug Hedge sold 1x 15 JAN 27 7400 PUT TO CLOSE on 2026-08-27 against this long. Per-trade attribution intentionally differs from broker/tax lots.',
        trade_id: 'jan-calendar-2026-08-20',
        contract: 'SPX270115P7400',
      },
    ],
  },
  {
    id: 'aug-hedge-2026-07-30',
    name: 'Aug Hedge',
    status: 'closed',
    opened_on: '2026-07-30',
    closed_on: '2026-08-27',
    structure: '4x SPXW 7050 PUT SEP26 (weekly round trip)',
    spotAtOpen: 7640,
    staleness: 14,
    legs: [],
    realized: -285,
    fees: 5.04,
    warnings: [
      {
        severity: 'error',
        code: 'duplicate_execution_claim',
        message:
          '6 orders in this file are byte-identical to orders already matched by Oct Strangle, Nov Condor, Dec Fly and Jan Calendar. The statements show the account never held the extra contracts (18 DEC 26 7500 PUT is -4, not -8), so they are quarantined: excluded from legs, cash flow and reconciliation.',
        trade_id: 'aug-hedge-2026-07-30',
        contract: 'SPX261218P7500',
      },
      {
        severity: 'info',
        code: 'settlement_inferred',
        message:
          'SPXW260918P7050 expires on a third Friday but the OPRA root says SPXW/PM. Settlement came from the OPRA code, never from the calendar.',
        trade_id: 'aug-hedge-2026-07-30',
        contract: 'SPXW260918P7050',
      },
    ],
    quarantined: [
      quarantinedOrder(
        'BUY +2 BUTTERFLY SPX 100 18 DEC 26 [AM] 7700/7500/7300 PUT @18.65 LMT FIXED',
        'Dec Fly (exec 2026-08-28 10:41:12, ref 9500000000021)',
      ),
      quarantinedOrder(
        'BUY +2 CALENDAR SPX 100 15 JAN 27 [AM]/18 DEC 26 [AM] 7400 PUT @21.30 LMT FIXED',
        'Jan Calendar (exec 2026-08-20 09:57:44)',
      ),
      quarantinedOrder(
        'SELL -2 IRON CONDOR SPX 100 20 NOV 26 [AM] 8400/8500/7000/6900 CALL/PUT @22.20 LMT FIXED',
        'Nov Condor (exec 2026-08-26 13:41:08)',
      ),
      quarantinedOrder(
        'SELL -1 STRANGLE SPX 100 16 OCT 26 [AM] 8200/7200 CALL/PUT @114.20 LMT FIXED',
        'Oct Strangle (exec 2026-08-24 10:12:35)',
      ),
      quarantinedOrder(
        'BUY +2 BUTTERFLY SPX 100 18 DEC 26 [AM] 7700/7500/7300 PUT @18.90 LMT FIXED',
        'Dec Fly (exec 2026-08-28 10:41:12)',
      ),
      quarantinedOrder(
        'BUY +2 CALENDAR SPX 100 15 JAN 27 [AM]/18 DEC 26 [AM] 7400 PUT @21.55 LMT FIXED',
        'Jan Calendar (exec 2026-08-20 09:57:44)',
      ),
    ],
  },
]

/* ------------------------------------------------------------------ *
 * Derivation
 * ------------------------------------------------------------------ */

function legMark(spec: LegSpec, underlying: number, from: IsoDate, volShift = 0): number {
  const t = yearFraction(from, spec.expiry)
  if (t <= 0) return intrinsic(underlying, spec.strike, spec.right)
  const vol = Math.max(0.01, surfaceIv(spec.strike, BASE_SPOT, t) + volShift)
  return bsmPrice({
    spot: underlying,
    strike: spec.strike,
    t,
    vol,
    rate: RATE,
    divYield: DIV,
    right: spec.right,
  })
}

function legCostBasis(spec: LegSpec, spec_open: IsoDate, spotAtOpen: number): number {
  if (spec.avgCost !== undefined) return spec.avgCost
  const t = yearFraction(spec_open, spec.expiry)
  const vol = surfaceIv(spec.strike, spotAtOpen, t)
  const theo = bsmPrice({
    spot: spotAtOpen,
    strike: spec.strike,
    t,
    vol,
    rate: RATE,
    divYield: DIV,
    right: spec.right,
  })
  return Math.max(0.05, Number(theo.toFixed(2)))
}

function buildLeg(spec: LegSpec, trade: TradeSpec, today: IsoDate, asOf: string): Leg {
  const t = yearFraction(today, spec.expiry)
  const iv = surfaceIv(spec.strike, BASE_SPOT, t)
  const mark = legMark(spec, spot, today)
  const spread = Math.max(0.4, mark * 0.03)
  const greeks =
    t > 0
      ? bsmGreeks({
          spot,
          strike: spec.strike,
          t,
          vol: iv,
          rate: RATE,
          divYield: DIV,
          right: spec.right,
        })
      : ZERO_GREEKS
  return {
    contract: contract(spec.expiry, spec.strike, spec.right, spec.root, spec.settlement),
    qty: spec.qty,
    avg_cost: legCostBasis(spec, trade.opened_on, trade.spotAtOpen),
    mark: round2(mark),
    bid: round2(Math.max(0.05, mark - spread / 2)),
    ask: round2(mark + spread / 2),
    iv,
    greeks,
    dte: daysBetween(today, spec.expiry),
    quote_as_of: asOf,
    quote_source: 'yfinance',
  }
}

function round2(value: number): number {
  return Math.round(value * 100) / 100
}

// Gamma is dollars-of-delta per index point — routinely two orders of
// magnitude smaller than the other greeks for a single trade — while the
// strip renders it at 4 dp (`GREEK_DP.gamma` in `lib/format.ts`). Rounding
// it to 2 dp like the others quantises most single-trade gamma rows to a
// flat 0.
function round6(value: number): number {
  return Math.round(value * 1e6) / 1e6
}

function positionGreeks(legs: Leg[]): Greeks {
  return legs.reduce<Greeks>(
    (acc, leg) => {
      const f = leg.qty * leg.contract.multiplier
      return {
        delta: acc.delta + leg.greeks.delta * f,
        gamma: acc.gamma + leg.greeks.gamma * f,
        theta: acc.theta + leg.greeks.theta * f,
        vega: acc.vega + leg.greeks.vega * f,
      }
    },
    { ...ZERO_GREEKS },
  )
}

/** Σ leg vega x (REF_DTE / DTE_leg)^k — REF is the front expiry of the set. */
function weightedVega(legs: Leg[], refDte: number | null): number {
  if (refDte === null || refDte <= 0) return 0
  return legs.reduce((acc, leg) => {
    const dte = Math.max(1, leg.dte)
    const weight = (refDte / dte) ** WV_EXPONENT
    return acc + leg.greeks.vega * leg.qty * leg.contract.multiplier * weight
  }, 0)
}

function buildTrade(spec: TradeSpec, today: IsoDate, asOf: string): Trade {
  const legs = spec.legs.map((leg) => buildLeg(leg, spec, today, asOf))
  // Valued from the unrounded model price, not the two-decimal display mark,
  // so the dashboard total and the T+0 curve agree to the cent.
  const value = spec.legs.reduce((a, l) => a + legMark(l, spot, today) * l.qty * MULT, 0)
  const basis = legs.reduce((a, l) => a + (l.avg_cost ?? 0) * l.qty * l.contract.multiplier, 0)
  const pnlOpen = value - basis
  const pnlTotal = pnlOpen + spec.realized - spec.fees
  const cost = Math.abs(basis)
  const openDtes = legs.filter((l) => l.qty !== 0).map((l) => l.dte)
  const dte = openDtes.length > 0 ? Math.min(...openDtes) : null
  const end = spec.closed_on ?? today
  return {
    id: spec.id,
    name: spec.name,
    status: spec.status,
    opened_on: spec.opened_on,
    closed_on: spec.closed_on ?? null,
    dit: Math.max(0, daysBetween(spec.opened_on, end)),
    dte,
    structure: spec.structure,
    cost: round2(cost),
    value: round2(value),
    pnl_open: round2(pnlOpen),
    pnl_total: round2(pnlTotal),
    pnl_pct: cost > 0 ? round2((pnlTotal / cost) * 100) : null,
    realized: spec.realized,
    fees: spec.fees,
    greeks: positionGreeks(legs),
    weighted_vega: weightedVega(legs, dte),
    staleness_secs: spec.status === 'closed' ? null : spec.staleness,
    warnings: spec.warnings ?? [],
    legs,
    quarantined: spec.quarantined ?? [],
  }
}

function nowIso(): string {
  return new Date().toISOString()
}

function openTrades(): Trade[] {
  const today = todayIso()
  const asOf = nowIso()
  return TRADE_SPECS.filter((s) => s.status === 'open').map((s) => buildTrade(s, today, asOf))
}

function closedTrades(): Trade[] {
  const today = todayIso()
  const asOf = nowIso()
  return TRADE_SPECS.filter((s) => s.status === 'closed').map((s) => buildTrade(s, today, asOf))
}

function portfolioOf(trades: Trade[]): PortfolioStats {
  const allLegs = trades.flatMap((t) => t.legs)
  const openDtes = allLegs.filter((l) => l.qty !== 0).map((l) => l.dte)
  const refDte = openDtes.length > 0 ? Math.min(...openDtes) : null
  const front = refDte === null ? null : addDays(todayIso(), refDte)
  return {
    count: trades.length,
    pnl_open: round2(trades.reduce((a, t) => a + t.pnl_open, 0)),
    pnl_total: round2(trades.reduce((a, t) => a + t.pnl_total, 0)),
    greeks: positionGreeks(allLegs),
    weighted_vega: weightedVega(allLegs, refDte),
    weighted_vega_reference: front ? `front_expiry (${front})` : 'front_expiry (—)',
  }
}

/* ------------------------------------------------------------------ *
 * Market snapshot
 * ------------------------------------------------------------------ */

const DEGRADED = [
  `Schwab: token file missing (schwab.token_path = ${MOCK_TOKEN_PATH})`,
  'yfinance quotes are ~15 min delayed',
]

export function mockSnapshot(): MarketSnapshot {
  return {
    as_of: nowIso(),
    // The mock ladder is live-ish: nothing behind it is older than the refresh.
    data_as_of: nowIso(),
    spot_source: 'yfinance',
    option_source: 'yfinance',
    degraded: [...DEGRADED],
    spot: { symbol: 'SPX', last: round2(spot), change: round2(spotChange), as_of: nowIso() },
    quote_count: TRADE_SPECS.reduce((a, s) => a + s.legs.length, 0),
    market_open: true,
  }
}

/* ------------------------------------------------------------------ *
 * Risk graph
 * ------------------------------------------------------------------ */

interface FlatLeg extends LegSpec {
  tradeId: string
}

function selectedSpecs(ids: string[]): TradeSpec[] {
  const wanted = new Set(ids)
  return TRADE_SPECS.filter((s) => wanted.has(s.id))
}

function flatten(specs: TradeSpec[]): FlatLeg[] {
  return specs.flatMap((s) => s.legs.map((l) => ({ ...l, tradeId: s.id })))
}

/**
 * The price grid a request asks for.
 *
 * The absolute branch is not just the 'abs' range mode: it is also what the
 * graph's *view* query sends while the trader is zoomed or panned — a tile
 * around the visible window, at up to the server's own 2001-point cap. Both
 * have to work here, or `just dev-ui` and the component tests would be
 * drawing something the real app never draws.
 */
function priceGrid(legs: FlatLeg[], req: RiskGraphRequest, refSpot: number): number[] {
  const points = Math.max(3, Math.min(2001, req.points || 241))
  let min: number
  let max: number
  const range = req.price_range
  if (range && 'min' in range) {
    min = range.min
    max = range.max
  } else {
    const pct = range && 'pct' in range ? range.pct : null
    if (pct !== null) {
      min = refSpot * (1 - pct)
      max = refSpot * (1 + pct)
    } else if (legs.length > 0) {
      const strikes = legs.map((l) => l.strike)
      min = Math.min(Math.min(...strikes) * 0.95, refSpot * 0.9)
      max = Math.max(Math.max(...strikes) * 1.05, refSpot * 1.1)
    } else {
      min = refSpot * 0.9
      max = refSpot * 1.1
    }
  }
  if (!(max > min)) {
    min = refSpot * 0.9
    max = refSpot * 1.1
  }
  const step = (max - min) / (points - 1)
  return Array.from({ length: points }, (_, i) => round2(min + i * step))
}

/** Resolve `today` | `+45d` | `expiry:2026-10-16` against the preset default. */
export function resolveGraphDates(specs: RiskGraphRequest['dates'], legs: FlatLeg[]): IsoDate[] {
  const today = todayIso()
  const expiries = [...new Set(legs.map((l) => l.expiry))].sort()
  const front = expiries[0]
  if (specs && specs.length > 0) {
    const out = specs.map((raw) => {
      if (raw === 'today') return today
      if (raw.startsWith('expiry:')) return raw.slice('expiry:'.length)
      const m = /^\+(\d+)d$/.exec(raw)
      if (m) return addDays(today, Number(m[1]))
      return raw
    })
    return [...new Set(out)].sort()
  }
  if (!front) return [today]
  const dte = Math.max(1, daysBetween(today, front))
  const preset = [0, Math.round(dte * 0.25), Math.round(dte * 0.5), Math.round(dte * 0.75), dte - 1]
  const dates = preset.map((d) => addDays(today, Math.max(0, d)))
  dates.push(front)
  return [...new Set(dates)].sort()
}

function lineKind(date: IsoDate, today: IsoDate, expiries: IsoDate[]): RiskLineKind {
  if (expiries.includes(date)) return 'expiry'
  return date === today ? 't0' : 'tplus'
}

/**
 * Mirrors the server's `line_key`: a stable identity per line, since two lines
 * can legitimately land on the same date (0-DTE puts T+0 and expiry together).
 */
function lineKeyLabel(
  kind: RiskLineKind,
  date: IsoDate,
  today: IsoDate,
  seen: { expiry: boolean },
): { key: string; label: string } {
  if (kind === 't0') return { key: 't0', label: 'T+0' }
  if (kind === 'tplus') {
    const offset = daysBetween(today, date)
    return { key: `t+${offset}`, label: `T+${offset}` }
  }
  const key = seen.expiry ? `expiry:${date}` : 'expiry'
  seen.expiry = true
  return { key, label: `Exp ${date}` }
}

function curveFor(
  legs: FlatLeg[],
  prices: number[],
  date: IsoDate,
  req: RiskGraphRequest,
  basisAdjust: number,
): number[] {
  return prices.map((price) => {
    let value = 0
    for (const leg of legs) {
      const shift = (req.vol_shift ?? 0) + (req.vol_shift_by_expiry?.[leg.expiry] ?? 0)
      const t = yearFraction(date, leg.expiry)
      const v =
        t <= 0
          ? intrinsic(price, leg.strike, leg.right)
          : bsmPrice({
              spot: price,
              strike: leg.strike,
              t,
              vol: Math.max(0.01, surfaceIv(leg.strike, BASE_SPOT, t) + shift),
              rate: RATE,
              divYield: DIV,
              right: leg.right,
            })
      value += v * leg.qty * MULT
    }
    return round2(value + basisAdjust)
  })
}

/**
 * T+0 greeks over the price grid, built from the same `bsmGreeks` bumps the
 * leg marks already use (see the file header) rather than a second,
 * hand-rolled bump size — that is what keeps this internally consistent with
 * `curveFor` instead of merely close to it.
 */
function t0GreeksFor(
  legs: FlatLeg[],
  prices: number[],
  today: IsoDate,
  req: RiskGraphRequest,
  front: IsoDate | undefined,
): RiskGreeks | null {
  const delta = new Array(prices.length).fill(0)
  const gamma = new Array(prices.length).fill(0)
  const theta = new Array(prices.length).fill(0)
  const vega = new Array(prices.length).fill(0)
  const weighted_vega = new Array(prices.length).fill(0)

  for (const leg of legs) {
    const t = yearFraction(today, leg.expiry)
    if (!(t > 0)) continue
    const shift = (req.vol_shift ?? 0) + (req.vol_shift_by_expiry?.[leg.expiry] ?? 0)
    const vol = Math.max(0.01, surfaceIv(leg.strike, BASE_SPOT, t) + shift)
    const refDte = Math.max(1, daysBetween(today, front ?? today))
    const legDte = Math.max(1, daysBetween(today, leg.expiry))
    const weight = Math.pow(refDte / legDte, WV_EXPONENT)
    const scale = leg.qty * MULT
    prices.forEach((price, i) => {
      const g = bsmGreeks({
        spot: price,
        strike: leg.strike,
        t,
        vol,
        rate: RATE,
        divYield: DIV,
        right: leg.right,
      })
      delta[i] += g.delta * scale
      gamma[i] += g.gamma * scale
      theta[i] += g.theta * scale
      vega[i] += g.vega * scale
      weighted_vega[i] += g.vega * scale * weight
    })
  }

  return {
    key: 't0',
    delta: delta.map(round2),
    gamma: gamma.map(round6),
    theta: theta.map(round2),
    vega: vega.map(round2),
    weighted_vega: weighted_vega.map(round2),
    weighted_vega_reference: `front_expiry (${front ?? 'none'})`,
  }
}

function basisFor(specs: TradeSpec[], includeRealized: boolean): number {
  let adjust = 0
  for (const spec of specs) {
    for (const leg of spec.legs) {
      adjust -= legCostBasis(leg, spec.opened_on, spec.spotAtOpen) * leg.qty * MULT
    }
    if (includeRealized) adjust += spec.realized - spec.fees
  }
  return adjust
}

function breakevensOf(prices: number[], pnl: number[]): number[] {
  const out: number[] = []
  for (let i = 1; i < prices.length; i += 1) {
    const a = pnl[i - 1]
    const b = pnl[i]
    const pa = prices[i - 1]
    const pb = prices[i]
    if (a === undefined || b === undefined || pa === undefined || pb === undefined) continue
    if (a === 0) out.push(pa)
    else if ((a < 0 && b > 0) || (a > 0 && b < 0)) {
      out.push(round2(pa + ((pb - pa) * (0 - a)) / (b - a)))
    }
  }
  return out
}

/** Lognormal P(profit at expiry) integrated over the profitable price regions. */
function probabilityOfProfit(
  prices: number[],
  pnl: number[],
  t: number,
  iv: number,
  refSpot: number,
): number {
  if (!(t > 0) || !(iv > 0) || prices.length < 2) return 0
  const drift = (RATE - DIV - 0.5 * iv * iv) * t
  const sd = iv * Math.sqrt(t)
  const cdf = (price: number): number => {
    if (price <= 0) return 0
    return normCdf((Math.log(price / refSpot) - drift) / sd)
  }
  let p = 0
  for (let i = 1; i < prices.length; i += 1) {
    const a = pnl[i - 1]
    const b = pnl[i]
    const pa = prices[i - 1]
    const pb = prices[i]
    if (a === undefined || b === undefined || pa === undefined || pb === undefined) continue
    if ((a + b) / 2 > 0) p += cdf(pb) - cdf(pa)
  }
  // Mass beyond the plotted range keeps the sign of the nearest endpoint.
  const first = pnl[0]
  const last = pnl[pnl.length - 1]
  const lo = prices[0]
  const hi = prices[prices.length - 1]
  if (first !== undefined && first > 0 && lo !== undefined) p += cdf(lo)
  if (last !== undefined && last > 0 && hi !== undefined) p += 1 - cdf(hi)
  return Math.min(1, Math.max(0, p))
}

export function mockRiskGraph(req: RiskGraphRequest): RiskGraphResponse {
  const specs = selectedSpecs(req.trade_ids)
  const legs = flatten(specs).filter((l) => l.qty !== 0)
  const today = todayIso()
  // A spot override re-centres the scenario without moving the live tape.
  const refSpot = req.spot_override ?? spot
  const prices = priceGrid(legs, req, refSpot)
  const expiries = [...new Set(legs.map((l) => l.expiry))].sort()
  const dates = resolveGraphDates(req.dates, legs)
  const includeRealized = req.include_realized !== false
  const adjust = basisFor(specs, includeRealized)

  const seen = { expiry: false }
  const lines: RiskLine[] = dates.map((date) => {
    const kind = lineKind(date, today, expiries)
    return {
      date,
      kind,
      ...lineKeyLabel(kind, date, today, seen),
      pnl: curveFor(legs, prices, date, req, adjust),
    }
  })

  // The overlay decomposes the T+0 curve (or the first line, when the trader
  // has edited T+0 out of the date list), so it is emitted for that line.
  const overlay = lines.find((l) => l.kind === 't0') ?? lines[0]
  // Greeks only exist for an actual T+0 line — no synthesizing them for a
  // book where the trader has edited T+0 out of the date list.
  const t0Line = lines.find((l) => l.kind === 't0')
  const t0_greeks = t0Line ? t0GreeksFor(legs, prices, today, req, expiries[0]) : null
  const per_trade = specs.map((spec) => {
    const specLegs = flatten([spec]).filter((l) => l.qty !== 0)
    return {
      trade_id: spec.id,
      date: overlay?.date ?? today,
      key: overlay?.key ?? 't0',
      pnl: curveFor(
        specLegs,
        prices,
        overlay?.date ?? today,
        req,
        basisFor([spec], includeRealized),
      ),
    }
  })

  const expiryLine = lines.find((l) => l.kind === 'expiry') ?? lines[lines.length - 1]
  const reference = expiryLine?.pnl ?? []
  const front = expiries[0] ?? today
  const tFront = yearFraction(today, front)
  const frontAtmIv = surfaceIv(refSpot, BASE_SPOT, Math.max(tFront, 0.02))
  const sd = frontAtmIv * Math.sqrt(Math.max(tFront, 1 / 365))
  const drift = -0.5 * frontAtmIv * frontAtmIv * Math.max(tFront, 1 / 365)

  return {
    as_of: nowIso(),
    spot: round2(refSpot),
    prices,
    lines,
    t0_greeks,
    per_trade,
    breakevens: { expiry: breakevensOf(prices, reference) },
    max_profit_on_range: reference.length > 0 ? round2(Math.max(...reference)) : 0,
    max_loss_on_range: reference.length > 0 ? round2(Math.min(...reference)) : 0,
    pop: probabilityOfProfit(prices, reference, tFront, frontAtmIv, refSpot),
    sigma_bands: {
      one: [round2(refSpot * Math.exp(drift - sd)), round2(refSpot * Math.exp(drift + sd))],
      two: [
        round2(refSpot * Math.exp(drift - 2 * sd)),
        round2(refSpot * Math.exp(drift + 2 * sd)),
      ],
    },
    front_atm_iv: frontAtmIv,
    // Same per-trade warnings the trade list carries — a demo stand-in for the
    // server's model-side iv_solve_failed/missing_quote warnings.
    warnings: specs.flatMap((s) => s.warnings ?? []),
  }
}

export function mockWhatIf(req: WhatIfRequest): WhatIfResponse {
  const specs = selectedSpecs(req.trade_ids)
  const legs = flatten(specs).filter((l) => l.qty !== 0)
  let value = 0
  const greeks: Greeks = { ...ZERO_GREEKS }
  for (const leg of legs) {
    const t = yearFraction(req.date, leg.expiry)
    const vol = Math.max(0.01, surfaceIv(leg.strike, BASE_SPOT, Math.max(t, 0.02)) + req.vol_shift)
    const input = {
      spot: req.price,
      strike: leg.strike,
      t,
      vol,
      rate: RATE,
      divYield: DIV,
      right: leg.right,
    }
    value += (t <= 0 ? intrinsic(req.price, leg.strike, leg.right) : bsmPrice(input)) * leg.qty * MULT
    if (t > 0) {
      const g = bsmGreeks(input)
      const f = leg.qty * MULT
      greeks.delta += g.delta * f
      greeks.gamma += g.gamma * f
      greeks.theta += g.theta * f
      greeks.vega += g.vega * f
    }
  }
  return { pnl: round2(value + basisFor(specs, req.include_realized !== false)), greeks }
}

/* ------------------------------------------------------------------ *
 * Reconciliation / warnings / config
 * ------------------------------------------------------------------ */

/** A mocked Friday statement, two days after the demo book's last fill. */
const STATEMENT_AS_OF = '2026-08-28'

/** The one contract the statement and the trade files disagree about. */
const NETTED_TOS_CODE = 'SPX270115P7400'

function mockReconciliation(): ReconciliationResponse {
  const totals = new Map<string, number>()
  for (const spec of TRADE_SPECS) {
    if (spec.status !== 'open') continue
    for (const leg of spec.legs) {
      const code = tosCode(leg.root ?? 'SPX', leg.expiry, leg.right, leg.strike)
      totals.set(code, (totals.get(code) ?? 0) + leg.qty)
    }
  }
  const rows: ReconciliationRow[] = [...totals.entries()]
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([tos_code, trades_qty]) => {
      // The account netted one JAN 7400 put away against Aug Hedge's close,
      // so the statement is one short of what Jan Calendar's legs claim.
      const statement_qty = tos_code === NETTED_TOS_CODE ? trades_qty - 1 : trades_qty
      return {
        tos_code,
        statement_qty,
        trades_qty,
        status: statement_qty === trades_qty ? 'match' : 'qty_mismatch',
      }
    })
  // A MAR 27 box spread nobody claims — informational, not an error.
  for (const [code, qty] of [
    ['SPX270319C7000', 1],
    ['SPX270319C7100', -1],
    ['SPX270319P7100', 1],
    ['SPX270319P7000', -1],
  ] as const) {
    rows.push({ tos_code: code, statement_qty: qty, trades_qty: 0, status: 'unassigned' })
  }
  return {
    statement_as_of: STATEMENT_AS_OF,
    rows,
    note: 'Per-trade attribution intentionally differs from broker/tax lots.',
  }
}

const GLOBAL_WARNINGS: Warning[] = [
  {
    severity: 'warn',
    code: 'unassigned_account_position',
    message:
      'The 2026-08-28 statement lists a 19 MAR 27 7000/7100 box (4 legs) that no trade file claims. Reported, never netted.',
    trade_id: null,
    contract: 'SPX270319C7000',
  },
  {
    severity: 'info',
    code: 'statement_date_disagreement',
    message:
      'Statement header says "since 8/26/26 through 8/27/26" but every row is dated 8/28/26; as_of taken from the rows.',
    trade_id: null,
    contract: null,
  },
  {
    severity: 'info',
    code: 'statement_section_skipped',
    message:
      'Skipped non-option sections: Futures Statements, Forex Statements, Equities. A stray "Total Cash" row between sections was ignored.',
    trade_id: null,
    contract: null,
  },
]

function allWarnings(): Warning[] {
  const perTrade = TRADE_SPECS.flatMap((s) => s.warnings ?? [])
  return [...perTrade, ...GLOBAL_WARNINGS].sort(
    (a, b) => severityRank(b.severity) - severityRank(a.severity),
  )
}

function severityRank(severity: Warning['severity']): number {
  return severity === 'error' ? 2 : severity === 'warn' ? 1 : 0
}

/**
 * The same shape `app_config` answers with — field for field with
 * `web/e2e/fixtures/contract/config.json`, including the `resolved` block the
 * Resolved config card renders. Only the values are invented.
 */
const MOCK_CONFIG: ConfigResponse = {
  trades: {
    root: MOCK_CONFIG_ROOT,
    open_dir: 'Open Trades',
    closed_dir: 'Closed Trades',
    statements_dir: 'Daily Trade Statement',
    statement_timezone: 'America/Chicago',
    watch_debounce_ms: 750,
    watch_poll_secs: 0,
  },
  schwab: {
    token_path: MOCK_TOKEN_PATH,
    token_config: null,
    token_config_key: 'paths.token_file',
    env_file: null,
    app_key_env: 'SCHWAB_APP_KEY',
    app_secret_env: 'SCHWAB_APP_SECRET',
    refresh: true,
  },
  tastytrade: {
    enabled: false,
    env_file: null,
    client_id_env: 'TASTYTRADE_CLIENT_ID',
    client_secret_env: 'TASTYTRADE_CLIENT_SECRET',
    refresh_token_env: 'TASTYTRADE_REFRESH_TOKEN',
    sandbox: false,
    delayed: false,
    api_version: '20250904',
  },
  yfinance: { python: '.venv/bin/python', script: null, enabled: true },
  market: {
    spot_sources: ['schwab', 'yfinance', 'cache'],
    option_sources: ['schwab', 'yfinance', 'cache', 'statement'],
    refresh_secs_open: 15,
    refresh_secs_closed: 300,
    pause_when_hidden: false,
    state_dir: MOCK_STATE_DIR,
  },
  pricing: {
    risk_free_rate: RATE,
    dividend_yield: DIV,
    weighted_vega: { reference: 'front_expiry', exponent: WV_EXPONENT },
  },
  risk_graph: { points: 241, range_pct: 0.1, tplus_preset: 'quarters' },
  resolved: {
    trades_root: MOCK_CONFIG_ROOT,
    open_dir: `${MOCK_CONFIG_ROOT}/Open Trades`,
    closed_dir: `${MOCK_CONFIG_ROOT}/Closed Trades`,
    statements_dir: `${MOCK_CONFIG_ROOT}/Daily Trade Statement`,
    state_dir: MOCK_STATE_DIR,
    valuation_date: todayIso(),
    demo: false,
    version: '0.1.0-mock',
    provider_notes: [],
  },
  '//': 'mock configuration — every value here is invented',
}

/* ------------------------------------------------------------------ *
 * Which book is loaded
 * ------------------------------------------------------------------ */


/**
 * The one folder the mock treats as "the layout is one level down".
 *
 * Real discovery descends a level, so a user who picks the *parent* of their
 * trades folder is a supported (and common) pick — and the two halves of the
 * two-step flow disagree about the path unless the commit carries the folder
 * the user actually picked. Without a folder of this shape in the mock, no
 * test can tell the two apart.
 */
const MOCK_PARENT_ROOT = '/home/trader/Archive/2025'

/**
 * The folder the user chose on this computer, if any. Wins over config.
 *
 * `picked` is the path they pointed at; `root` is the folder discovery
 * settled on, which is what the server reports and what the book is read
 * from. They differ for [`MOCK_PARENT_ROOT`].
 */
let mockLocalRoot: { picked: string; root: string; chosen_at: string } | null = null

/** Recently forgotten folders, most-recent-first (cap 5, de-duplicated). */
const MOCK_RECENT: string[] = []

/** Test seam: forget any chosen folder and its recent list, as a fresh boot would. */
export function resetMockTradesRoot(): void {
  mockLocalRoot = null
  MOCK_RECENT.length = 0
}

function tradesRootResponse(): TradesRootResponse {
  return {
    root: mockLocalRoot?.root ?? MOCK_CONFIG_ROOT,
    source: mockLocalRoot !== null ? 'local' : 'config',
    config_root: MOCK_CONFIG_ROOT,
    open_trades: openTrades().length,
    closed_trades: closedTrades().length,
    ...(mockLocalRoot !== null ? { chosen_at: mockLocalRoot.chosen_at } : {}),
    watching: mockLocalRoot !== null,
    recent: [...MOCK_RECENT],
  }
}

/**
 * `DELETE /trades/root` — Clear. Stops displaying the trades: the chosen
 * folder is forgotten (pushed onto the front of `recent`) and the book falls
 * back to the configured folder. Nothing on disk is touched — the mock never
 * held any bytes to begin with.
 */
function deleteTradesRoot(): TradesRootResponse {
  if (mockLocalRoot !== null) {
    // The folder the user picked, not the one discovery settled on: that is
    // what the record held, and what clicking it again has to re-pick.
    const forgotten = mockLocalRoot.picked
    const deduped = [forgotten, ...MOCK_RECENT.filter((path) => path !== forgotten)]
    MOCK_RECENT.length = 0
    MOCK_RECENT.push(...deduped.slice(0, 5))
  }
  mockLocalRoot = null
  return tradesRootResponse()
}

/**
 * The mock's version of discovery: a good path is the configured root,
 * [`MOCK_PARENT_ROOT`], or anything ending in `/Trades` (mirroring the
 * fixture layouts real discovery would find); anything else is the 422 every
 * unknown pick gets.
 */
function checkFolderPath(root: string): void {
  const known = root === MOCK_CONFIG_ROOT || root === MOCK_PARENT_ROOT || root.endsWith('/Trades')
  if (!known) {
    throw new MockProblem(
      422,
      'Unprocessable Entity',
      `no trades folders under "${root}" (looked for "Open Trades", "Closed Trades" ` +
        'or "Daily Trade Statement", and one level down)',
    )
  }
}

/**
 * The folder a pick is actually read from: the pick itself, or one level
 * down. Throws the 422 for anything that is not a trades folder at all.
 */
function discoverFolder(picked: string): string {
  checkFolderPath(picked)
  return picked === MOCK_PARENT_ROOT ? `${MOCK_PARENT_ROOT}/Trades` : picked
}

/** `PUT /trades/root` — load the folder the user chose on this computer. */
function putTradesRoot(picked: string): TradesRootResponse {
  const root = discoverFolder(picked)
  mockLocalRoot = { picked, root, chosen_at: nowIso() }
  return tradesRootResponse()
}

/**
 * Test seam: the path the last `PUT /trades/root` actually carried.
 *
 * `tradesRootResponse().root` reports the folder discovery settled on, so it
 * cannot tell a commit of the user's pick from a commit of the discovered
 * root — and only the pick keeps the server's symlink-containment boundary
 * the same in both steps of the flow.
 */
export function mockPickedRoot(): string | null {
  return mockLocalRoot?.picked ?? null
}

/**
 * `POST /trades/root/probe` — validate-only preview of a folder: the same
 * checks `PUT /trades/root` runs, but nothing is persisted, loaded or
 * watched.
 */
function probeFolder(picked: string): FolderProbe {
  const root = discoverFolder(picked)
  return {
    root,
    open_trades: openTrades().length,
    closed_trades: closedTrades().length,
    statements: 3,
    ...(root === picked ? {} : { note: `the trades layout is one level down, in ${root}` }),
  }
}

/* ------------------------------------------------------------------ *
 * Simulated event stream
 * ------------------------------------------------------------------ */

type MockEventHandler = (event: string, payload: unknown) => void

/** Walks spot a little every few seconds so SSE wiring is visible in demo mode. */
export function startMockEvents(handler: MockEventHandler): () => void {
  const timer = setInterval(() => {
    const drift = (Math.random() - 0.5) * 3
    spot = round2(spot + drift)
    spotChange = round2(spotChange + drift)
    handler('snapshot', mockSnapshot())
  }, 5000)
  handler('snapshot', mockSnapshot())
  return () => clearInterval(timer)
}

/* ------------------------------------------------------------------ *
 * Router
 * ------------------------------------------------------------------ */

export class MockNotFound extends Error {}

/** A mocked RFC 9457 failure — `client.ts` turns it into an `ApiError`. */
export class MockProblem extends Error {
  readonly problem: ProblemJson

  constructor(status: number, title: string, detail: string) {
    super(detail)
    this.name = 'MockProblem'
    this.problem = { title, status, detail }
  }
}

function tradesResponse(): TradesResponse {
  const trades = openTrades()
  return {
    as_of: nowIso(),
    trades,
    portfolio_all: portfolioOf(trades),
    warning_count: allWarnings().length,
  }
}

function closedResponse(): ClosedTradesResponse {
  const trades = closedTrades()
  const wins = trades.filter((t) => t.pnl_total > 0).length
  return {
    trades,
    totals: {
      // Pre-fee, matching the server: `realized - fees === pnl_total`.
      realized: round2(trades.reduce((a, t) => a + t.realized, 0)),
      fees: round2(trades.reduce((a, t) => a + t.fees, 0)),
      pnl_total: round2(trades.reduce((a, t) => a + t.pnl_total, 0)),
      count: trades.length,
      win_rate: trades.length > 0 ? wins / trades.length : 0,
      avg_days_held:
        trades.length > 0 ? round2(trades.reduce((a, t) => a + t.dit, 0) / trades.length) : 0,
    },
  }
}

/**
 * Resolve one API call against the mock. `path` is everything after
 * `/api/v1`, including any query string.
 */
export function mockRequest(path: string, init?: RequestInit): unknown {
  const [rawPath, search = ''] = path.split('?')
  const route = (rawPath ?? '').replace(/\/+$/, '') || '/'
  const method = (init?.method ?? 'GET').toUpperCase()
  const body = init?.body ? (JSON.parse(String(init.body)) as Record<string, unknown>) : {}

  if (method === 'DELETE') {
    if (route === '/trades/root') return deleteTradesRoot()
    throw new MockNotFound(`mock: no DELETE route for ${route}`)
  }

  if (method === 'PUT') {
    if (route === '/trades/root') return putTradesRoot(String(body.root ?? ''))
    throw new MockNotFound(`mock: no PUT route for ${route}`)
  }

  if (method === 'POST') {
    switch (route) {
      case '/risk-graph':
        return mockRiskGraph(body as unknown as RiskGraphRequest)
      case '/what-if':
        return mockWhatIf(body as unknown as WhatIfRequest)
      case '/reload': {
        const reloaded: ReloadResponse = { reloaded: true, trades: openTrades().length }
        return reloaded
      }
      case '/trades/root/probe':
        return probeFolder(String(body.root ?? ''))
      default:
        throw new MockNotFound(`mock: no POST route for ${route}`)
    }
  }

  // Every remaining route is a GET.
  if (method !== 'GET') {
    throw new MockNotFound(`mock: no ${method} route for ${route}`)
  }

  switch (route) {
    case '/market/snapshot':
      return mockSnapshot()
    case '/trades':
      return tradesResponse()
    case '/trades/closed':
      return closedResponse()
    case '/trades/root':
      return tradesRootResponse()
    case '/portfolio': {
      const ids = new URLSearchParams(search).get('ids')
      const wanted = ids ? ids.split(',').filter(Boolean) : []
      const set = new Set(wanted)
      return portfolioOf([...openTrades(), ...closedTrades()].filter((t) => set.has(t.id)))
    }
    case '/reconciliation':
      return mockReconciliation()
    case '/warnings': {
      const warnings: WarningsResponse = { warnings: allWarnings() }
      return warnings
    }
    case '/config':
      return MOCK_CONFIG
    default:
      throw new MockNotFound(`mock: no route for ${route}`)
  }
}

export const MOCK_SPOT = BASE_SPOT
