/**
 * Typed mock of the frozen `/api/v1` contract.
 *
 * Activated by `VITE_MOCK=1`, or automatically in dev when the API cannot be
 * reached (see `client.ts`), so every screen is demo-able and testable before
 * `wingspan serve` exists on the machine.
 *
 * The data is *derived*, not hand-written: a small BSM surface prices the same
 * leg list into leg marks, position greeks, the risk-graph curves and the
 * what-if answer. That keeps the documented invariant true in mock mode —
 * the `t0` line evaluated at spot equals the dashboard `pnl_total` — so the UI
 * is exercised against numbers that behave like the server's.
 *
 * Structures, strikes and expiries mirror the shapes in `docs/PLAN.md`
 * (butterflies, calendars, diagonals, a crash-hedge diagonal, a quarantined
 * duplicate-claim trade). No account identifiers exist here.
 */

import { bsmGreeks, bsmPrice, intrinsic, normCdf, ZERO_GREEKS } from '../lib/bsm'
import { addDays, daysBetween, todayIso, yearFraction } from '../lib/dates'
import type {
  ClosedTradesResponse,
  ConfigResponse,
  FsDirEntry,
  FsDirsResponse,
  Greeks,
  Health,
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

/** Mutable so the simulated SSE feed can walk the tape. */
let spot = 7302.15
let spotChange = -18.35
const BASE_SPOT = spot

/* ------------------------------------------------------------------ *
 * Volatility surface
 * ------------------------------------------------------------------ */

/** ATM term structure: low near-dated, richer further out. */
function atmVol(t: number): number {
  return 0.115 + 0.055 * (1 - Math.exp(-1.2 * t))
}

/** Sticky-strike SPX-shaped surface: monotone put skew, flattening with time. */
export function surfaceIv(strike: number, underlying: number, t: number): number {
  const k = Math.log(strike / underlying)
  const tt = Math.max(t, 0.02)
  const iv = atmVol(tt) + (0.3 * -k) / Math.sqrt(Math.max(tt, 0.04)) + 0.15 * k * k
  return Math.min(1.5, Math.max(0.07, iv))
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
  /** Overrides the modelled entry price where the plan pins a known fill. */
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

const FEB = '2027-02-19'
const MAR = '2027-03-19'
const APR = '2027-04-16'

function quarantinedOrder(raw: string, claimedBy: string): QuarantinedOrder {
  return {
    raw,
    reason: 'duplicate execution claim',
    claimed_by: claimedBy,
  }
}

const TRADE_SPECS: TradeSpec[] = [
  {
    id: 'bsh-2026-07-14',
    name: 'BSH',
    status: 'open',
    opened_on: '2026-07-14',
    structure: '1x SPX 5450 PUT OCT26 (rolled from SEP 5050)',
    spotAtOpen: 7180,
    staleness: 14,
    legs: [{ expiry: '2026-10-16', strike: 5450, right: 'put', qty: 1, avgCost: 4.42 }],
    realized: -433,
    fees: 3.78,
    warnings: [
      {
        severity: 'info',
        code: 'order_rolled',
        message:
          'The 18 SEP 26 5050 PUT leg opened 2026-07-14 at 4.90 and closed 2026-08-19 at 0.57 inside this trade; realised P&L is carried on the trade, not the leg.',
        trade_id: 'bsh-2026-07-14',
        contract: 'SPX260918P5050',
      },
    ],
  },
  {
    id: 'm1-2026-07-14',
    name: 'M1',
    status: 'open',
    opened_on: '2026-07-14',
    structure: '2x BF 7325/6975/6625 FEB27 · 1x DIAG 8200/8250 MAR/FEB27',
    spotAtOpen: 7180,
    staleness: 14,
    legs: [
      { expiry: FEB, strike: 7325, right: 'put', qty: 2 },
      { expiry: FEB, strike: 6975, right: 'put', qty: -4 },
      { expiry: FEB, strike: 6625, right: 'put', qty: 2 },
      { expiry: MAR, strike: 8200, right: 'call', qty: 1 },
      { expiry: FEB, strike: 8250, right: 'call', qty: -1 },
    ],
    realized: 0,
    fees: 7.56,
  },
  {
    id: 'm2-2026-07-21',
    name: 'M2',
    status: 'open',
    opened_on: '2026-07-21',
    structure: '2x BF 7325/6975/6625 FEB27 · 1x CAL 8200 APR/FEB27',
    spotAtOpen: 7205,
    staleness: 14,
    legs: [
      { expiry: FEB, strike: 7325, right: 'put', qty: 2 },
      { expiry: FEB, strike: 6975, right: 'put', qty: -4 },
      { expiry: FEB, strike: 6625, right: 'put', qty: 2 },
      { expiry: APR, strike: 8200, right: 'call', qty: 1 },
      { expiry: FEB, strike: 8200, right: 'call', qty: -1 },
    ],
    realized: 0,
    fees: 7.56,
  },
  {
    id: 'm3-2026-07-28',
    name: 'M3',
    status: 'open',
    opened_on: '2026-07-28',
    structure: '2x BF 7200/6850/6500 FEB27 · 1x DIAG 8025/8100 MAR/FEB27',
    spotAtOpen: 7240,
    staleness: 14,
    legs: [
      { expiry: FEB, strike: 7200, right: 'put', qty: 2 },
      { expiry: FEB, strike: 6850, right: 'put', qty: -4 },
      { expiry: FEB, strike: 6500, right: 'put', qty: 2 },
      { expiry: MAR, strike: 8025, right: 'call', qty: 1 },
      { expiry: FEB, strike: 8100, right: 'call', qty: -1 },
    ],
    realized: 0,
    fees: 7.56,
  },
  {
    id: 'm4-2026-08-04',
    name: 'M4',
    status: 'open',
    opened_on: '2026-08-04',
    structure: '2x BF 7500/7150/6800 FEB27 · 1x DIAG 8350/8375 APR/FEB27',
    spotAtOpen: 7265,
    staleness: 14,
    legs: [
      { expiry: FEB, strike: 7500, right: 'put', qty: 2 },
      { expiry: FEB, strike: 7150, right: 'put', qty: -4 },
      { expiry: FEB, strike: 6800, right: 'put', qty: 2 },
      { expiry: APR, strike: 8350, right: 'call', qty: 1 },
      { expiry: FEB, strike: 8375, right: 'call', qty: -1 },
    ],
    realized: 0,
    fees: 7.56,
    warnings: [
      {
        severity: 'info',
        code: 'cross_trade_netting',
        message:
          'The account netted this contract across trades: Delta 1 sold 1x 19 FEB 27 6800 PUT TO CLOSE on 2026-08-28 against this long. Per-trade attribution intentionally differs from broker/tax lots.',
        trade_id: 'm4-2026-08-04',
        contract: 'SPX270219P6800',
      },
    ],
  },
  {
    id: 'm5-2026-08-15',
    name: 'M5',
    status: 'open',
    opened_on: '2026-08-15',
    structure: '2x BF 7600/7225/6850 MAR27 · 1x DIAG 8475/8525 APR/MAR27',
    spotAtOpen: 7290,
    staleness: 1_260,
    legs: [
      { expiry: MAR, strike: 7600, right: 'put', qty: 2 },
      { expiry: MAR, strike: 7225, right: 'put', qty: -4 },
      { expiry: MAR, strike: 6850, right: 'put', qty: 2 },
      { expiry: APR, strike: 8475, right: 'call', qty: 1 },
      { expiry: MAR, strike: 8525, right: 'call', qty: -1 },
    ],
    realized: 0,
    fees: 7.56,
    warnings: [
      {
        severity: 'warn',
        code: 'date_outside_window',
        message:
          'File name date 2026-08-15 is a Saturday; the matched fills are dated 2026-08-14. Opened-on (and therefore DIT) uses the file name, per your designation.',
        trade_id: 'm5-2026-08-15',
        contract: null,
      },
    ],
  },
  {
    id: 'delta-1-2026-08-25',
    name: 'Delta 1',
    status: 'closed',
    opened_on: '2026-08-25',
    closed_on: '2026-08-28',
    structure: '1x VERT 6800/6700 PUT FEB27 (closed)',
    spotAtOpen: 7310,
    staleness: 14,
    legs: [],
    realized: 140,
    fees: 4.8,
  },
  {
    id: 'lpta-2026-07-24',
    name: 'LPTA',
    status: 'closed',
    opened_on: '2026-07-24',
    closed_on: '2026-08-14',
    structure: '1x SPXW 5875 PUT AUG26 (weekly round trip)',
    spotAtOpen: 7215,
    staleness: 14,
    legs: [],
    realized: -160,
    fees: 2.35,
    warnings: [
      {
        severity: 'error',
        code: 'duplicate_execution_claim',
        message:
          '6 orders in this file are byte-identical to orders already matched by M1, M2 and M3. The statements show the account never held the extra contracts (7325 PUT is +4, not +6), so they are quarantined: excluded from legs, cash flow and reconciliation.',
        trade_id: 'lpta-2026-07-24',
        contract: 'SPX270219P7325',
      },
      {
        severity: 'info',
        code: 'settlement_inferred',
        message:
          'SPXW260821P5875 expires on a third Friday but the OPRA root says SPXW/PM. Settlement came from the OPRA code, never from the calendar.',
        trade_id: 'lpta-2026-07-24',
        contract: 'SPXW260821P5875',
      },
    ],
    quarantined: [
      quarantinedOrder(
        'BUY +1 CALENDAR SPX 100 16 APR 27 [AM]/19 FEB 27 [AM] 8200 CALL @64.40 LMT FIXED',
        'M2 (exec 2026-07-21 13:41:07, ref 1007170257443)',
      ),
      quarantinedOrder(
        'BUY +1 DIAGONAL SPX 100 19 MAR 27 [AM]/19 FEB 27 [AM] 8025/8100 CALL @57.45 LMT FIXED',
        'M3 (exec 2026-07-28 10:02:55)',
      ),
      quarantinedOrder(
        'BUY +2 BUTTERFLY SPX 100 19 FEB 27 [AM] 7325/6975/6625 PUT @22.10 LMT FIXED',
        'M1 (exec 2026-07-14 11:18:20)',
      ),
      quarantinedOrder(
        'BUY +2 BUTTERFLY SPX 100 19 FEB 27 [AM] 7200/6850/6500 PUT @21.45 LMT FIXED',
        'M3 (exec 2026-07-28 10:02:55)',
      ),
      quarantinedOrder(
        'BUY +2 BUTTERFLY SPX 100 19 FEB 27 [AM] 7325/6975/6625 PUT @22.40 LMT FIXED',
        'M2 (exec 2026-07-21 13:41:07)',
      ),
      quarantinedOrder(
        'BUY +1 DIAGONAL SPX 100 19 MAR 27 [AM]/19 FEB 27 [AM] 8200/8250 CALL @48.25 LMT FIXED',
        'M1 (exec 2026-07-14 11:18:20)',
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
  'Schwab: token file missing (schwab.token_path = ~/.config/wingspan/schwab-token.json)',
  'yfinance quotes are ~15 min delayed',
]

export function mockSnapshot(): MarketSnapshot {
  return {
    as_of: nowIso(),
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

/** Resolve `today` | `+45d` | `expiry:2027-02-19` against the preset default. */
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

const STATEMENT_AS_OF = '2026-08-28'

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
      // The account netted one FEB 6800 put away against Delta 1's close.
      const statement_qty = tos_code === 'SPX270219P6800' ? trades_qty - 1 : trades_qty
      return {
        tos_code,
        statement_qty,
        trades_qty,
        status: statement_qty === trades_qty ? 'match' : 'qty_mismatch',
      }
    })
  // A box spread nobody claims — informational, not an error.
  for (const [code, qty] of [
    ['SPX270716C6000', 1],
    ['SPX270716C6100', -1],
    ['SPX270716P6100', 1],
    ['SPX270716P6000', -1],
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
      'The 2026-08-28 statement lists a 16 JUL 27 6000/6100 box (4 legs) that no trade file claims. Reported, never netted.',
    trade_id: null,
    contract: 'SPX270716C6000',
  },
  {
    severity: 'info',
    code: 'statement_date_disagreement',
    message:
      'Statement header says "since 8/27/26 through 8/27/26" but every row is dated 8/28/26; as_of taken from the rows.',
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

const MOCK_CONFIG: ConfigResponse = {
  trades: {
    root: '/home/trader/workspace/example_trades/Trades',
    open_dir: 'Open Trades',
    closed_dir: 'Closed Trades',
    statements_dir: 'Daily Trade Statement',
    statement_timezone: 'America/Chicago',
  },
  server: { bind: '127.0.0.1:8787', allow_remote: false, auth_token: '***' },
  schwab: {
    token_path: '~/.config/wingspan/schwab-token.json',
    app_key_env: 'SCHWAB_APP_KEY',
    app_secret_env: 'SCHWAB_APP_SECRET',
    refresh: true,
  },
  yfinance: { python: '.venv/bin/python', enabled: true },
  market: {
    spot_sources: ['schwab', 'yfinance', 'cache'],
    option_sources: ['schwab', 'yfinance', 'cache', 'statement'],
    refresh_secs_open: 15,
    refresh_secs_closed: 300,
    state_dir: '~/.local/state/wingspan',
  },
  pricing: {
    risk_free_rate: RATE,
    dividend_yield: DIV,
    weighted_vega: { reference: 'front_expiry', exponent: WV_EXPONENT },
  },
  risk_graph: { points: 241, range_pct: 0.12, tplus_preset: ['t0', '25%', '50%', '75%', 'dte-1'] },
  '//': 'mock configuration — the real server serves the resolved config with secrets redacted',
}

/* ------------------------------------------------------------------ *
 * Trades root + fake server filesystem
 * ------------------------------------------------------------------ */

/**
 * A small in-memory directory tree standing in for the machine the server
 * runs on, so the folder picker is demo-able and testable without touching a
 * real filesystem. Keys are absolute paths, values are child directory names
 * (files never appear — `/fs/dirs` lists directories only).
 */
const MOCK_FS: Record<string, string[]> = {
  '/': ['home', 'mnt', 'tmp'],
  '/home': ['trader'],
  '/home/trader': ['.config', 'Documents', 'Downloads', 'workspace'],
  '/home/trader/.config': ['wingspan'],
  '/home/trader/.config/wingspan': [],
  '/home/trader/Documents': ['Taxes 2025'],
  '/home/trader/Documents/Taxes 2025': [],
  '/home/trader/Downloads': [],
  '/home/trader/workspace': ['example_trades', 'notes', 'wingspan'],
  '/home/trader/workspace/example_trades': ['.git', 'Trades', 'archive'],
  '/home/trader/workspace/example_trades/.git': [],
  '/home/trader/workspace/example_trades/Trades': [
    'Closed Trades',
    'Daily Trade Statement',
    'Open Trades',
  ],
  '/home/trader/workspace/example_trades/Trades/Closed Trades': [],
  '/home/trader/workspace/example_trades/Trades/Daily Trade Statement': [],
  '/home/trader/workspace/example_trades/Trades/Open Trades': [],
  '/home/trader/workspace/example_trades/archive': ['2025'],
  '/home/trader/workspace/example_trades/archive/2025': [],
  '/home/trader/workspace/notes': [],
  '/home/trader/workspace/wingspan': ['crates', 'web'],
  '/home/trader/workspace/wingspan/crates': [],
  '/home/trader/workspace/wingspan/web': [],
  '/mnt': ['backup'],
  // Lower-case on purpose: discovery matches folder names case-insensitively.
  '/mnt/backup': ['trades-2025'],
  '/mnt/backup/trades-2025': ['closed trades', 'daily trade statement', 'open trades'],
  '/mnt/backup/trades-2025/closed trades': [],
  '/mnt/backup/trades-2025/daily trade statement': [],
  '/mnt/backup/trades-2025/open trades': [],
  '/tmp': [],
}

const MOCK_HOME = '/home/trader'
const MOCK_CONFIG_ROOT = '/home/trader/workspace/example_trades/Trades'
const TRADE_DIR_NAMES = ['Open Trades', 'Closed Trades', 'Daily Trade Statement'] as const

/** The override the user picked in the app, if any. */
let mockRootOverride: string | null = null

/** Test seam: forget the folder the picker chose. */
export function resetMockTradesRoot(): void {
  mockRootOverride = null
}

function joinPath(parent: string, name: string): string {
  return parent === '/' ? `/${name}` : `${parent}/${name}`
}

function parentPath(path: string): string | null {
  if (path === '/') return null
  const cut = path.lastIndexOf('/')
  return cut <= 0 ? '/' : path.slice(0, cut)
}

function normalizePath(raw: string): string {
  const trimmed = raw.trim()
  if (trimmed === '') return MOCK_HOME
  const expanded = trimmed === '~' || trimmed.startsWith('~/')
    ? MOCK_HOME + trimmed.slice(1)
    : trimmed
  const stripped = expanded.replace(/\/+$/, '')
  return stripped === '' ? '/' : stripped
}

function children(path: string): string[] | null {
  return MOCK_FS[path] ?? null
}

/** One directory's own three-folder match count — the server's probe score. */
function layoutScore(path: string): number {
  const kids = children(path)
  if (kids === null) return 0
  const lower = kids.map((name) => name.toLowerCase())
  return TRADE_DIR_NAMES.filter((wanted) => lower.includes(wanted.toLowerCase())).length
}

/** `has_trades_layout`: the probe succeeds here or one level down. */
function hasTradesLayout(path: string): boolean {
  if (layoutScore(path) > 0) return true
  return (children(path) ?? []).some((name) => layoutScore(joinPath(path, name)) > 0)
}

/** Mirrors `wingspan_core::discovery::find_root_with`: best candidate wins. */
function resolveRoot(start: string): string | null {
  const candidates = [start, ...(children(start) ?? []).map((name) => joinPath(start, name))]
  let best: { score: number; path: string } | null = null
  for (const candidate of candidates) {
    const score = layoutScore(candidate)
    if (score > 0 && (best === null || score > best.score)) best = { score, path: candidate }
  }
  return best?.path ?? null
}

/** Mirrors `DiscoveryError`'s `Display` so the UI renders the real sentence. */
function discoveryError(start: string): string {
  if (children(start) === null) return `${start} is not a directory`
  return (
    `no trades folders under ${start} (looked for "Open Trades", "Closed Trades" ` +
    `or "Daily Trade Statement", and one level down)`
  )
}

function tradesRootResponse(): TradesRootResponse {
  return {
    root: mockRootOverride ?? MOCK_CONFIG_ROOT,
    source: mockRootOverride === null ? 'config' : 'override',
    config_root: MOCK_CONFIG_ROOT,
    open_trades: openTrades().length,
    closed_trades: closedTrades().length,
  }
}

function putTradesRoot(body: Record<string, unknown>): TradesRootResponse {
  const requested = normalizePath(typeof body.root === 'string' ? body.root : '')
  const resolved = resolveRoot(requested)
  if (resolved === null) throw new MockProblem(422, 'Unprocessable Entity', discoveryError(requested))
  mockRootOverride = resolved
  return tradesRootResponse()
}

function deleteTradesRoot(): TradesRootResponse {
  mockRootOverride = null
  return tradesRootResponse()
}

function fsDirs(search: string): FsDirsResponse {
  const params = new URLSearchParams(search)
  const rawPath = params.get('path')
  const hidden = params.get('hidden') === '1' || params.get('hidden') === 'true'
  const active = mockRootOverride ?? MOCK_CONFIG_ROOT
  const path = normalizePath(rawPath ?? parentPath(active) ?? MOCK_HOME)
  const kids = children(path)
  if (kids === null) {
    throw new MockProblem(422, 'Unprocessable Entity', `cannot read ${path}: no such directory`)
  }
  const entries: FsDirEntry[] = kids
    .filter((name) => hidden || !name.startsWith('.'))
    .map((name) => {
      const child = joinPath(path, name)
      return { name, path: child, has_trades_layout: hasTradesLayout(child) }
    })
    .sort((a, b) => a.name.localeCompare(b.name))
  return { path, parent: parentPath(path), home: MOCK_HOME, entries }
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
      realized: round2(trades.reduce((a, t) => a + t.pnl_total, 0)),
      fees: round2(trades.reduce((a, t) => a + t.fees, 0)),
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

  if (method === 'PUT') {
    if (route === '/trades/root') return putTradesRoot(body)
    throw new MockNotFound(`mock: no PUT route for ${route}`)
  }

  if (method === 'DELETE') {
    if (route === '/trades/root') return deleteTradesRoot()
    throw new MockNotFound(`mock: no DELETE route for ${route}`)
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
      default:
        throw new MockNotFound(`mock: no POST route for ${route}`)
    }
  }

  switch (route) {
    case '/health': {
      const health: Health = { status: 'ok', version: '0.1.0-mock' }
      return health
    }
    case '/market/snapshot':
      return mockSnapshot()
    case '/trades':
      return tradesResponse()
    case '/trades/closed':
      return closedResponse()
    case '/trades/root':
      return tradesRootResponse()
    case '/fs/dirs':
      return fsDirs(search)
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
    default: {
      const one = /^\/trades\/([^/]+)$/.exec(route)
      if (one) {
        const found = [...openTrades(), ...closedTrades()].find((t) => t.id === one[1])
        if (found) return found
      }
      throw new MockNotFound(`mock: no route for ${route}`)
    }
  }
}

/** Test seam: the trade ids the mock knows about. */
export const MOCK_TRADE_IDS = TRADE_SPECS.map((s) => s.id)
export const MOCK_SPOT = BASE_SPOT
