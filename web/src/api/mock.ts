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
 *
 * The option chain (`/chain/expirations`, `/chain`) is priced off the *same*
 * surface as the book, so a held contract shows the same mark, bid and ask on
 * the Model tab as in the positions table — as the server's fixture chain
 * does. Modeled legs (`model_legs`) join the risk graph, the what-if and the
 * portfolio stats as one synthetic position entered at today's mark, so their
 * T+0 contribution at spot is exactly zero, exactly as the server prices them.
 */

import { bsmGreeks, bsmPrice, intrinsic, normCdf, ZERO_GREEKS } from '../lib/bsm'
import { addDays, daysBetween, formatExpiry, yearFraction } from '../lib/dates'
import type {
  ChainExpirationsResponse,
  ChainExpiry,
  ChainQuote,
  ClosedTradesResponse,
  ConfigResponse,
  Greeks,
  IsoDate,
  Leg,
  MarketSnapshot,
  ModelContract,
  ModelLeg as ModelLegDto,
  ModelOrdersRequest,
  ModelOrdersResponse,
  OptionChainRequest,
  OptionChainResponse,
  OptionContract,
  PortfolioRequest,
  PortfolioStats,
  ProblemJson,
  QuarantinedOrder,
  ReconciliationResponse,
  ReconciliationRow,
  ReloadResponse,
  RecentSource,
  Right,
  RiskGraphRequest,
  RiskGraphResponse,
  RiskGreeks,
  RiskLine,
  RiskLineKind,
  RiskTerminal,
  SetModelContractsRequest,
  SetModelContractsResponse,
  Settlement,
  TosOrderDto,
  Trade,
  TradeSourceProbe,
  TradeSourceRequest,
  TradeSourceResponse,
  TradesResponse,
  Warning,
  WarningsResponse,
  WhatIfRequest,
  WhatIfResponse,
} from './types'

const MULT = 100
const RATE = 0.042
const DIV = 0.013
const WV_EXPONENT = 0.5
/** The server's default weighted-vega reference: a fixed 30 days. */
const WV_REFERENCE = '30d'
const WV_REFERENCE_DAYS = 30

const MOCK_STATE_DIR = '/home/trader/.local/share/com.jbolinge.wingspan/state'
const MOCK_TOKEN_PATH = '/home/trader/.config/wingspan/schwab_token.json'

/**
 * A masked sheet label, matching the shape `label` always takes: never a
 * full spreadsheet id, only its first-and-last few characters either side
 * of an ellipsis. Placeholder id and gid only — never a real one.
 */
const MOCK_SHEET_LABEL = 'docs.google.com/…/1AbCdE…abcd · Legs'
/** A plausible CSV pick — also the example `mock.test.ts` commits against. */
export const MOCK_CSV_PATH = '/home/trader/Downloads/legs.csv'

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

/** The two indices the book can hold. */
type IndexName = 'SPX' | 'XSP'

/** XSP (Mini-SPX) is one tenth of SPX: the mock, like the server, prices it
 *  off the SPX surface and spot. */
const XSP_DIVISOR = 10

function indexOf(underlying: string | undefined): IndexName {
  return underlying === 'XSP' ? 'XSP' : 'SPX'
}

/**
 * The SPX contract an option is priced as, and the factor its *values* are
 * scaled by: an XSP strike 650 is the SPX strike 6500 with every price (and,
 * in SPX-point exposure, every greek) times 0.1. SPX is its own proxy. The
 * Rust twin is `OptionContract::pricing_proxy`.
 */
function proxyOf(c: { underlying?: string; strike: number }): { strike: number; scale: number } {
  return c.underlying === 'XSP'
    ? { strike: c.strike * XSP_DIVISOR, scale: 1 / XSP_DIVISOR }
    : { strike: c.strike, scale: 1 }
}

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
  underlying: IndexName = 'SPX',
): OptionContract {
  return {
    underlying,
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
  /** `XSP` for a Mini-SPX leg (its `strike` is the XSP strike); absent = SPX. */
  underlying?: IndexName
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
  // An XSP (Mini-SPX) hedge: ten contracts of the 650 put, one tenth of an SPX
  // 6500 put each — mirrors the Rust demo book's "XSP Hedge".
  {
    id: 'xsp-hedge-2026-08-26',
    name: 'XSP Hedge',
    status: 'open',
    opened_on: '2026-08-26',
    structure: '10x XSP 650P DEC26',
    spotAtOpen: 7740,
    staleness: 14,
    legs: [
      { expiry: DEC, strike: 650, right: 'put', qty: 10, root: 'XSP', settlement: 'pm', underlying: 'XSP', avgCost: 3.8 },
    ],
    realized: 0,
    fees: 12.6,
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
  const { strike, scale } = proxyOf(spec)
  if (t <= 0) return intrinsic(underlying, strike, spec.right) * scale
  const vol = Math.max(0.01, surfaceIv(strike, BASE_SPOT, t) + volShift)
  return (
    bsmPrice({
      spot: underlying,
      strike,
      t,
      vol,
      rate: RATE,
      divYield: DIV,
      right: spec.right,
    }) * scale
  )
}

function legCostBasis(spec: LegSpec, spec_open: IsoDate, spotAtOpen: number): number {
  if (spec.avgCost !== undefined) return spec.avgCost
  const t = yearFraction(spec_open, spec.expiry)
  const { strike, scale } = proxyOf(spec)
  const vol = surfaceIv(strike, spotAtOpen, t)
  const theo =
    bsmPrice({
      spot: spotAtOpen,
      strike,
      t,
      vol,
      rate: RATE,
      divYield: DIV,
      right: spec.right,
    }) * scale
  return Math.max(0.05, Number(theo.toFixed(2)))
}

/**
 * A bid/ask around a model mark. Shared by the book's legs and the chain so a
 * held contract quotes identically in the positions table and on the Model
 * tab.
 */
function quoteSides(mark: number): { bid: number; ask: number } {
  const spread = Math.max(0.4, mark * 0.03)
  return { bid: round2(Math.max(0.05, mark - spread / 2)), ask: round2(mark + spread / 2) }
}

function buildLeg(spec: LegSpec, trade: TradeSpec, today: IsoDate, asOf: string): Leg {
  const t = yearFraction(today, spec.expiry)
  const { strike, scale } = proxyOf(spec)
  const iv = surfaceIv(strike, BASE_SPOT, t)
  // The quote is the *proxy's* — the SPXW contract at the proxy strike, quoted
  // exactly as the chain quotes it — scaled down for a proxied (XSP) leg, the
  // way the server's `quote_for` answers the leg row and the chain alike.
  // Spreading a fresh 0.40-minimum bid/ask around the already-scaled mark
  // gave the XSP Hedge a quote ten times too wide and a mark rounded to cents
  // that the XSP chain, which is the proxy's ÷ 10, never showed.
  const proxyMark = legMark({ expiry: spec.expiry, strike, right: spec.right, qty: spec.qty }, spot, today)
  const proxySides = quoteSides(proxyMark)
  const toLeg = (value: number): number => (scale === 1 ? value : value / XSP_DIVISOR)
  const greeks =
    t > 0
      ? scaleGreeks(
          bsmGreeks({
            spot,
            strike,
            t,
            vol: iv,
            rate: RATE,
            divYield: DIV,
            right: spec.right,
          }),
          scale,
        )
      : ZERO_GREEKS
  return {
    contract: contract(spec.expiry, spec.strike, spec.right, spec.root, spec.settlement, spec.underlying),
    qty: spec.qty,
    avg_cost: legCostBasis(spec, trade.opened_on, trade.spotAtOpen),
    mark: toLeg(round2(proxyMark)),
    bid: toLeg(proxySides.bid),
    ask: toLeg(proxySides.ask),
    iv,
    greeks,
    dte: daysBetween(today, spec.expiry),
    quote_as_of: asOf,
    quote_source: 'yfinance',
  }
}

/** SPX-point exposure of a proxied leg: `scale` × the proxy's greeks. */
function scaleGreeks(g: Greeks, scale: number): Greeks {
  return { delta: g.delta * scale, gamma: g.gamma * scale, theta: g.theta * scale, vega: g.vega * scale }
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

/** Σ leg vega x (REF_DTE / DTE_leg)^k — REF is a fixed 30 days, the
 *  server's default, so every row, selection and book shares it. */
function weightedVega(legs: Leg[]): number {
  return legs.reduce((acc, leg) => {
    const dte = Math.max(1, leg.dte)
    const weight = (WV_REFERENCE_DAYS / dte) ** WV_EXPONENT
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
    weighted_vega: weightedVega(legs),
    staleness_secs: spec.status === 'closed' ? null : spec.staleness,
    staleness_partial: false,
    warnings: spec.warnings ?? [],
    legs,
    quarantined: spec.quarantined ?? [],
  }
}

/**
 * The mock's own valuation instant — Monday 2026-08-31, 12:00 New York, the
 * Rust demo's pinned clock (`wingspan-app` `demo::clock`) — and its New York
 * day. Every date and instant the mock prices or reports comes from here,
 * never from the browser clock: the book holds fixed 2026 expiries, and
 * priced against a real `now` it rotted as the calendar moved (on 2026-10-17
 * the Oct Strangle expired under every test that read it). The browser clock
 * is left to the product itself — quote ages, the market banner — exactly as
 * it is against `wingspan-devserver serve --demo`.
 */
export const MOCK_NOW = '2026-08-31T16:00:00.000Z'
export const MOCK_TODAY: IsoDate = '2026-08-31'

function nowIso(): string {
  return MOCK_NOW
}

function openTrades(): Trade[] {
  const today = MOCK_TODAY
  const asOf = nowIso()
  return TRADE_SPECS.filter((s) => s.status === 'open').map((s) => buildTrade(s, today, asOf))
}

function closedTrades(): Trade[] {
  const today = MOCK_TODAY
  const asOf = nowIso()
  return TRADE_SPECS.filter((s) => s.status === 'closed').map((s) => buildTrade(s, today, asOf))
}

function portfolioOf(trades: Trade[]): PortfolioStats {
  const allLegs = trades.flatMap((t) => t.legs)
  return {
    count: trades.length,
    pnl_open: round2(trades.reduce((a, t) => a + t.pnl_open, 0)),
    pnl_total: round2(trades.reduce((a, t) => a + t.pnl_total, 0)),
    greeks: positionGreeks(allLegs),
    weighted_vega: weightedVega(allLegs),
    weighted_vega_reference: WV_REFERENCE,
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
      const strikes = legs.map((l) => proxyOf(l).strike)
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
  const uniform = Array.from({ length: points }, (_, i) => round2(min + i * step))
  // The server's grid is the uniform samples plus spot (exactly the `spot`
  // the response reports) and every leg's proxy strike inside the range,
  // inserted in order — so the expiration kink sits on a node and "at spot"
  // reads the spot sample itself. Mirrored here so the component tests and
  // `just dev-ui` exercise the same non-uniform shape (docs/IPC.md).
  const lo = uniform[0]!
  const hi = uniform[uniform.length - 1]!
  const nodes = [round2(refSpot), ...legs.map((leg) => proxyOf(leg).strike)].filter(
    (price) => Number.isFinite(price) && price >= lo && price <= hi,
  )
  return [...new Set([...uniform, ...nodes])].sort((a, b) => a - b)
}

/** A resolved graph date: the calendar day, and whether the spec named an
 *  expiration — `expiry:` always does, as on the server (`resolve_date`),
 *  whether or not any leg here expires that day. */
export interface GraphDay {
  date: IsoDate
  expiry: boolean
}

/** The `expiry:<date>` specs before the valuation day: the server drops
 *  them (that expiration has settled) with a `graph_date_dropped` warning. */
export function droppedGraphDates(specs: RiskGraphRequest['dates']): string[] {
  return (specs ?? []).filter((raw) => raw.startsWith('expiry:') && raw.slice('expiry:'.length) < MOCK_TODAY)
}

/** Resolve `today` | `+45d` | `expiry:2026-10-16` against the preset default.
 *  A past `expiry:` spec is dropped (see `droppedGraphDates`); a list left
 *  with nothing is the preset. */
export function resolveGraphDays(requested: RiskGraphRequest['dates'], legs: FlatLeg[]): GraphDay[] {
  const today = MOCK_TODAY
  const dropped = new Set(droppedGraphDates(requested))
  const specs = requested?.filter((raw) => !dropped.has(raw)) ?? null
  const expiries = [...new Set(legs.map((l) => l.expiry))].sort()
  const front = expiries[0]
  const dedupe = (days: GraphDay[]): GraphDay[] => {
    const byDate = new Map<IsoDate, GraphDay>()
    // Two specs on one day keep the expiration reading, as the server does.
    for (const day of days) {
      const expiry = day.expiry || (byDate.get(day.date)?.expiry ?? false)
      byDate.set(day.date, { date: day.date, expiry })
    }
    return [...byDate.values()].sort((a, b) => a.date.localeCompare(b.date))
  }
  if (specs && specs.length > 0) {
    return dedupe(
      specs.map((raw) => {
        if (raw === 'today') return { date: today, expiry: false }
        if (raw.startsWith('expiry:')) return { date: raw.slice('expiry:'.length), expiry: true }
        const m = /^\+(\d+)d$/.exec(raw)
        return { date: m ? addDays(today, Number(m[1])) : raw, expiry: false }
      }),
    )
  }
  if (!front) return [{ date: today, expiry: false }]
  const dte = Math.max(1, daysBetween(today, front))
  const preset = [0, Math.round(dte * 0.25), Math.round(dte * 0.5), Math.round(dte * 0.75), dte - 1]
  const days: GraphDay[] = preset.map((d) => ({ date: addDays(today, Math.max(0, d)), expiry: false }))
  days.push({ date: front, expiry: true })
  return dedupe(days)
}

/** The calendar days alone — what the what-if and the tests look at. */
export function resolveGraphDates(specs: RiskGraphRequest['dates'], legs: FlatLeg[]): IsoDate[] {
  return resolveGraphDays(specs, legs).map((day) => day.date)
}

function lineKind(day: GraphDay, today: IsoDate): RiskLineKind {
  if (day.expiry) return 'expiry'
  return day.date === today ? 't0' : 'tplus'
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
      const { strike, scale } = proxyOf(leg)
      const v =
        t <= 0
          ? intrinsic(price, strike, leg.right)
          : bsmPrice({
              spot: price,
              strike,
              t,
              vol: Math.max(0.01, surfaceIv(strike, BASE_SPOT, t) + shift),
              rate: RATE,
              divYield: DIV,
              right: leg.right,
            })
      value += v * scale * leg.qty * MULT
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
    const proxy = proxyOf(leg)
    const vol = Math.max(0.01, surfaceIv(proxy.strike, BASE_SPOT, t) + shift)
    const legDte = Math.max(1, daysBetween(today, leg.expiry))
    const weight = Math.pow(WV_REFERENCE_DAYS / legDte, WV_EXPONENT)
    const scale = leg.qty * MULT * proxy.scale
    prices.forEach((price, i) => {
      const g = bsmGreeks({
        spot: price,
        strike: proxy.strike,
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
    weighted_vega_reference: WV_REFERENCE,
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

/** Lognormal P(profit at the terminal date) integrated over the profitable
 *  price regions. `null` where the engine's `probability_of_profit` answers
 *  `None`: no time left to the terminal date, no positive vol, or fewer than
 *  two prices — never a 0 that reads as "certain loss". */
function probabilityOfProfit(
  prices: number[],
  pnl: number[],
  t: number,
  iv: number,
  refSpot: number,
): number | null {
  if (!(t > 0) || !(iv > 0) || prices.length < 2 || prices.length !== pnl.length) return null
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

/**
 * The trades a graph request covers, plus the model position when it carries
 * modeled legs. `trade_ids: []` with a model is the model alone — the
 * server's rule (`service::positions_with_model`); without a model the mock
 * keeps treating an empty list as nothing, as it always has.
 */
function graphSpecs(tradeIds: readonly string[], modelLegs: ModelLegDto[] | undefined): TradeSpec[] {
  const model = modelSpecFrom(modelLegs)
  const trades = selectedSpecs([...tradeIds])
  return model === null ? trades : [...trades, model]
}

export function mockRiskGraph(req: RiskGraphRequest): RiskGraphResponse {
  const specs = graphSpecs(req.trade_ids, req.model_legs)
  const legs = flatten(specs).filter((l) => l.qty !== 0)
  const today = MOCK_TODAY
  // A spot override re-centres the scenario without moving the live tape.
  const refSpot = req.spot_override ?? spot
  const prices = priceGrid(legs, req, refSpot)
  const expiries = [...new Set(legs.map((l) => l.expiry))].sort()
  const days = resolveGraphDays(req.dates, legs)
  const includeRealized = req.include_realized !== false
  const adjust = basisFor(specs, includeRealized)

  const seen = { expiry: false }
  const lines: RiskLine[] = days.map(({ date, ...day }) => {
    const kind = lineKind({ date, ...day }, today)
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
  const t0_greeks = t0Line ? t0GreeksFor(legs, prices, today, req) : null
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

  // The mock has no settlement instants: an expiry dated today counts as
  // live, as it does on the server until its settlement.
  const frontExpiry = expiries.find((expiry) => expiry >= today) ?? expiries[0] ?? null
  const breakevens: Record<string, number[]> = Object.fromEntries(
    lines.map((line) => [line.key, breakevensOf(prices, line.pnl)]),
  )

  // The terminal line every response-level readout is read from: the
  // canonical (earliest) expiration line; with none drawn, the front
  // expiration measured off-screen (`drawn: false`) so the header still
  // describes an expiration; with no legs at all, the last line drawn.
  const canonical = lines.find((l) => l.key === 'expiry')
  let terminal: RiskTerminal | null = null
  let reference: number[] = []
  if (canonical) {
    terminal = { key: canonical.key, date: canonical.date, kind: canonical.kind, drawn: true }
    reference = canonical.pnl
  } else if (legs.length > 0 && frontExpiry !== null) {
    reference = curveFor(legs, prices, frontExpiry, req, adjust)
    terminal = { key: 'expiry', date: frontExpiry, kind: 'expiry', drawn: false }
    breakevens.expiry = breakevensOf(prices, reference)
  } else {
    const last = lines[lines.length - 1]
    if (last) {
      terminal = { key: last.key, date: last.date, kind: last.kind, drawn: true }
      reference = last.pnl
    }
  }

  const front = frontExpiry ?? today
  const tFront = yearFraction(today, front)
  // No legs, no front, no vol: null, as the engine answers (`Option`), never 0.
  const frontAtmIv = frontExpiry === null ? null : surfaceIv(refSpot, BASE_SPOT, Math.max(tFront, 0.02))
  const bandIv = frontAtmIv ?? surfaceIv(refSpot, BASE_SPOT, 0.02)
  const sd = bandIv * Math.sqrt(Math.max(tFront, 1 / 365))
  const drift = -0.5 * bandIv * bandIv * Math.max(tFront, 1 / 365)
  const tTerminal = terminal ? yearFraction(today, terminal.date) : 0
  const droppedWarnings: Warning[] = droppedGraphDates(req.dates).map((raw) => ({
    severity: 'warn',
    code: 'graph_date_dropped',
    message: `${raw} is before the valuation date ${today}: that expiration has settled, so the line was dropped`,
    trade_id: null,
    contract: null,
  }))

  return {
    as_of: nowIso(),
    spot: round2(refSpot),
    prices,
    lines,
    t0_greeks,
    per_trade,
    // One entry per line key, as the server answers; `expiry` (the earliest
    // expiration line) is the one the chart marks and the header reads.
    breakevens,
    max_profit_on_range: reference.length > 0 ? round2(Math.max(...reference)) : 0,
    max_loss_on_range: reference.length > 0 ? round2(Math.min(...reference)) : 0,
    pop: frontAtmIv === null ? null : probabilityOfProfit(prices, reference, tTerminal, frontAtmIv, refSpot),
    sigma_bands: {
      one: [round2(refSpot * Math.exp(drift - sd)), round2(refSpot * Math.exp(drift + sd))],
      two: [
        round2(refSpot * Math.exp(drift - 2 * sd)),
        round2(refSpot * Math.exp(drift + 2 * sd)),
      ],
    },
    front_atm_iv: frontAtmIv,
    front_expiry: frontExpiry,
    terminal,
    // Same per-trade warnings the trade list carries — a demo stand-in for the
    // server's model-side iv_solve_failed/missing_quote warnings — plus one
    // per dropped date spec.
    warnings: [...specs.flatMap((s) => s.warnings ?? []), ...droppedWarnings],
  }
}

export function mockWhatIf(req: WhatIfRequest): WhatIfResponse {
  // The server refuses a date before its valuation date: there is no "past"
  // to reprice at — an expiry already settled is banked cash.
  if (typeof req?.date === 'string' && req.date < MOCK_TODAY) {
    throw outOfRange(`a what-if date (${req.date}) before the valuation date ${MOCK_TODAY}`)
  }
  const specs = graphSpecs(req.trade_ids, req.model_legs)
  const legs = flatten(specs).filter((l) => l.qty !== 0)
  let value = 0
  const greeks: Greeks = { ...ZERO_GREEKS }
  for (const leg of legs) {
    const t = yearFraction(req.date, leg.expiry)
    // The surface at the leg's own `t`, as `legMark` and `curveFor` read it:
    // a floor here (it once was 0.02y) repriced every leg inside a week of
    // expiry off a different vol, and at-spot-today stopped landing on
    // `pnl_total`. `surfaceIv` floors `t` itself.
    const { strike, scale } = proxyOf(leg)
    // The per-expiry shift on top of the global one, exactly as `curveFor`
    // applies the risk-graph request's.
    const shift = (req.vol_shift ?? 0) + (req.vol_shift_by_expiry?.[leg.expiry] ?? 0)
    const vol = Math.max(0.01, surfaceIv(strike, BASE_SPOT, t) + shift)
    const input = {
      spot: req.price,
      strike,
      t,
      vol,
      rate: RATE,
      divYield: DIV,
      right: leg.right,
    }
    value += (t <= 0 ? intrinsic(req.price, strike, leg.right) : bsmPrice(input)) * scale * leg.qty * MULT
    if (t > 0) {
      const g = bsmGreeks(input)
      const f = leg.qty * MULT * scale
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
    // The mock always has a canned statement to reconcile against; the
    // sheet-source honestly-empty case is only real off a live server.
    source_note: null,
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
    cache: true,
    csv_path: '',
    fetch_timeout_secs: 20,
    poll_secs: 0,
    // Deprecated, unread, still typed — the config-file field is only ever
    // reported back as null in the mock.
    root: null,
    sheet_url: '',
    statement_timezone: null,
    tab: 'Legs',
    timezone: null,
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
    weighted_vega: { reference: WV_REFERENCE, exponent: WV_EXPONENT },
  },
  risk_graph: { points: 241, range_pct: 0.1, tplus_preset: 'quarters' },
  resolved: {
    source: {
      kind: 'demo',
      label: 'fixtures/demo/legs.csv',
      tab: null,
      fetched_at: nowIso(),
      stale: false,
    },
    state_dir: MOCK_STATE_DIR,
    valuation_date: MOCK_TODAY,
    demo: false,
    version: '0.1.0-mock',
    provider_notes: [],
  },
  '//': 'mock configuration — every value here is invented',
}

/* ------------------------------------------------------------------ *
 * The trade source (sheet / CSV)
 *
 * The mock never parses a real CSV or fetches a real sheet — it fabricates a
 * plausible probe/commit response from its own canned book (`TRADE_SPECS`),
 * so the dialog and status bar can be developed and tested without a Rust
 * backend. Validation mirrors decisions 1-5 of the trade-source plan closely
 * enough to exercise every SPA branch, not to be a parser.
 * ------------------------------------------------------------------ */

/** The full column set `Check` reports finding on a well-formed Legs sheet
 *  or CSV — matches `docs/DATA-FORMATS.md`'s Legs contract. */
const MOCK_TRADE_SOURCE_COLUMNS = [
  'Trade',
  'Side',
  'Qty',
  'OCC Symbol',
  'Opened',
  'Open Price',
  'Closed',
  'Close Price',
  'Status',
  'Fees',
  'Mark',
  'Expiry',
  '#',
  'Key',
]

/** The chosen sheet or CSV, if any. `null` falls back to the demo book. */
let mockSource: {
  kind: 'sheet' | 'csv'
  label: string
  tab: string | null
  path: string | null
  chosen_at: string
} | null = null

/** Recently forgotten trade sources, most-recent-first (cap 5, de-duplicated by label). */
const MOCK_RECENT_SOURCES: RecentSource[] = []

/** Test seam: forget any chosen trade source and its recent list, as a fresh boot would. */
export function resetMockTradeSource(): void {
  mockSource = null
  MOCK_RECENT_SOURCES.length = 0
}

function totalLegs(): number {
  return TRADE_SPECS.reduce((a, s) => a + s.legs.length, 0)
}

function tradeSourceResponse(): TradeSourceResponse {
  if (mockSource === null) {
    return {
      kind: 'demo',
      label: 'fixtures/demo/legs.csv',
      tab: null,
      path: 'fixtures/demo/legs.csv',
      open_trades: openTrades().length,
      closed_trades: closedTrades().length,
      legs: totalLegs(),
      rows_skipped: 0,
      chosen_at: null,
      fetched_at: nowIso(),
      stale: false,
      detail: null,
      auto_refresh_secs: null,
      recent: [...MOCK_RECENT_SOURCES],
    }
  }
  return {
    kind: mockSource.kind,
    label: mockSource.label,
    tab: mockSource.tab,
    path: mockSource.path,
    open_trades: openTrades().length,
    closed_trades: closedTrades().length,
    legs: totalLegs(),
    rows_skipped: 0,
    chosen_at: mockSource.chosen_at,
    fetched_at: nowIso(),
    stale: false,
    detail: null,
    auto_refresh_secs: null,
    recent: [...MOCK_RECENT_SOURCES],
  }
}

/** A bare spreadsheet id: no host, just a long opaque token. */
function isBareSheetId(value: string): boolean {
  return /^[A-Za-z0-9_-]{20,}$/.test(value)
}

/**
 * Validates a probe/commit request the way the real server would (see
 * decisions 4-5 of the trade-source plan), throwing the matching 422
 * `MockProblem`. Returns what a successful commit would record.
 */
function resolveTradeSource(
  source: TradeSourceRequest,
): { label: string; tab: string | null; missingColumns: boolean } {
  if (source.kind === 'manual') {
    throw new MockProblem(
      422,
      'Unprocessable Entity',
      'manual leg entry is not built yet',
      'invalid-trade-source',
    )
  }
  if (source.kind === 'sheet') {
    const { url, tab } = source
    if (!url.includes('docs.google.com') && !isBareSheetId(url)) {
      throw new MockProblem(
        422,
        'Unprocessable Entity',
        'that is not a Google Sheets link',
        'invalid-trade-source',
      )
    }
    if (url.includes('not-shared')) {
      throw new MockProblem(
        422,
        'Unprocessable Entity',
        'that spreadsheet is not shared — in Google Sheets choose Share → Anyone with the link → Viewer',
        'sheet-not-shared',
      )
    }
    if (tab !== undefined && tab !== '' && tab !== 'Legs') {
      throw new MockProblem(
        422,
        'Unprocessable Entity',
        `no tab called "${tab}" in that spreadsheet`,
        'sheet-tab-not-found',
      )
    }
    return { label: MOCK_SHEET_LABEL, tab: tab || 'Legs', missingColumns: false }
  }
  const { path } = source
  if (!path.endsWith('.csv')) {
    throw new MockProblem(422, 'Unprocessable Entity', 'that file is not a CSV', 'invalid-trade-source')
  }
  return { label: path, tab: null, missingColumns: path.includes('missing-columns') }
}

/** `POST /trades/source/probe` — validate-only preview: nothing is
 *  persisted, loaded or fetched. */
function probeTradeSource(source: TradeSourceRequest): TradeSourceProbe {
  const { label, tab, missingColumns } = resolveTradeSource(source)
  const legs = totalLegs()
  const missing = missingColumns ? ['Trade', 'Side'] : []
  return {
    label,
    tab,
    columns_found: MOCK_TRADE_SOURCE_COLUMNS.filter((c) => !missing.includes(c)),
    columns_missing: missing,
    columns_absent: [],
    columns_ignored: ['Days', 'Band'],
    rows: legs,
    legs,
    rows_skipped: 0,
    trades: { open: openTrades().length, closed: closedTrades().length },
    warnings: [],
    // No `note` key at all, matching the real wire: the DTO carries
    // `skip_serializing_if = "Option::is_none"`, so a probe with nothing to
    // say omits the field rather than sending `null` (`docs/IPC.md`).
  }
}

/** `PUT /trades/source` — commit to a sheet or a CSV. */
function putTradeSource(source: TradeSourceRequest): TradeSourceResponse {
  probeTradeSource(source) // validates; throws the matching 422 for anything bad
  if (source.kind === 'manual') {
    // Unreachable: `probeTradeSource` above always throws for `manual`.
    throw new MockProblem(
      422,
      'Unprocessable Entity',
      'manual leg entry is not built yet',
      'invalid-trade-source',
    )
  }
  const { label, tab } = resolveTradeSource(source)
  mockSource = {
    kind: source.kind,
    label,
    tab,
    path: source.kind === 'csv' ? source.path : null,
    chosen_at: nowIso(),
  }
  return tradeSourceResponse()
}

/**
 * `DELETE /trades/source` — Clear. Stops displaying the trades: the chosen
 * source is forgotten (pushed onto the front of `recent`) and the book falls
 * back to the demo book. Nothing on disk is touched — the mock never held
 * any bytes to begin with.
 */
function deleteTradeSource(): TradeSourceResponse {
  if (mockSource !== null) {
    const forgotten: RecentSource = {
      kind: mockSource.kind,
      label: mockSource.label,
      tab: mockSource.tab,
    }
    const deduped = [forgotten, ...MOCK_RECENT_SOURCES.filter((r) => r.label !== forgotten.label)]
    MOCK_RECENT_SOURCES.length = 0
    MOCK_RECENT_SOURCES.push(...deduped.slice(0, 5))
  }
  mockSource = null
  return tradeSourceResponse()
}


/* ------------------------------------------------------------------ *
 * The option chain and modeled legs
 *
 * The listing follows the server fixture's rules (`fixture.rs`), dated from
 * the browser's local today instead of the fixture clock: dailies out to 35
 * days, Friday weeklies out to 180, third-Friday SPX/AM monthlies out to two
 * years, and a quarterly on the last weekday of each quarter within a year.
 * Weekends are skipped; holidays are not, because the server's
 * `MarketCalendar` ships with an empty holiday set too.
 * ------------------------------------------------------------------ */

/** The server refuses more modeled legs (or contracts) than this, and a
 *  modeled |qty| above `MAX_MODEL_QTY`, with 422 `out-of-range`. */
const MAX_MODEL_LEGS = 64
const MAX_MODEL_QTY = 999

/** Day of week of a naive date, 0 = Sunday. */
function weekday(date: IsoDate): number {
  return new Date(`${date}T00:00:00Z`).getUTCDay()
}

function isoOf(year: number, month: number, day: number): IsoDate {
  return new Date(Date.UTC(year, month, day)).toISOString().slice(0, 10)
}

/** The third Friday of `month` (0-based). */
function thirdFriday(year: number, month: number): IsoDate {
  const first = new Date(Date.UTC(year, month, 1)).getUTCDay()
  return isoOf(year, month, 1 + ((5 - first + 7) % 7) + 14)
}

/** The third Friday of `date`'s own month. */
function thirdFridayOf(date: IsoDate): IsoDate {
  return thirdFriday(Number(date.slice(0, 4)), Number(date.slice(5, 7)) - 1)
}

/** The last Monday-to-Friday day of `month` (0-based). */
function lastWeekday(year: number, month: number): IsoDate {
  let date = isoOf(year, month + 1, 0)
  while (weekday(date) === 0 || weekday(date) === 6) date = addDays(date, -1)
  return date
}

/** Every listed expiration as of `today`, sorted by expiry then root. */
export function mockExpirations(today: IsoDate = MOCK_TODAY): ChainExpiry[] {
  const listed = new Map<string, ChainExpiry>()
  const add = (expiry: IsoDate, root: string, kind: ChainExpiry['kind']): void => {
    const key = `${expiry}|${root}`
    // First rule wins: a quarterly that falls on a listed weekly stays weekly.
    if (listed.has(key)) return
    listed.set(key, {
      expiry,
      dte: daysBetween(today, expiry),
      root,
      settlement: root === 'SPX' ? 'am' : 'pm',
      kind,
    })
  }

  for (let offset = 0; offset <= 180; offset += 1) {
    const date = addDays(today, offset)
    const dow = weekday(date)
    if (dow === 0 || dow === 6) continue
    if (offset <= 35) add(date, 'SPXW', dow === 5 ? 'weekly' : 'daily')
    else if (dow === 5) add(date, 'SPXW', 'weekly')
  }

  const start = new Date(`${today}T00:00:00Z`)
  for (let m = 0; m <= 25; m += 1) {
    const year = start.getUTCFullYear() + Math.floor((start.getUTCMonth() + m) / 12)
    const month = (start.getUTCMonth() + m) % 12
    const monthly = thirdFriday(year, month)
    const dte = daysBetween(today, monthly)
    if (dte >= 0 && dte <= 730) add(monthly, 'SPX', 'monthly')
    if (month % 3 === 2) {
      const quarter = lastWeekday(year, month)
      const qdte = daysBetween(today, quarter)
      if (qdte >= 0 && qdte <= 365) add(quarter, 'SPXW', 'quarterly')
    }
  }

  return [...listed.values()].sort((a, b) =>
    a.expiry === b.expiry ? a.root.localeCompare(b.root) : a.expiry.localeCompare(b.expiry),
  )
}

function grid(step: number, lo: number, hi: number): number[] {
  const out: number[] = []
  for (let k = Math.ceil(lo / step) * step; k <= hi; k += step) out.push(k)
  return out
}

/**
 * The strikes listed for an expiry `dte` days out: the 25-point grid over
 * [0.75·S, 1.15·S], densified to 5 points within ±2.5 % for 21 days and less,
 * or to 10 points within ±5 % out to 60. Built off the session's opening
 * spot, not the drifting tape, so a section's rows do not reshuffle while it
 * is on screen. Every leg of the mock book sits on the 25-point grid.
 */
export function mockStrikes(dte: number): number[] {
  const s = BASE_SPOT
  const strikes = new Set(grid(25, 0.75 * s, 1.15 * s))
  if (dte <= 21) for (const k of grid(5, 0.975 * s, 1.025 * s)) strikes.add(k)
  else if (dte <= 60) for (const k of grid(10, 0.95 * s, 1.05 * s)) strikes.add(k)
  return [...strikes].sort((a, b) => a - b)
}

/** Below a twentieth of a cent of model value there is nothing to quote. */
const UNPRICEABLE = 0.005

function chainQuote(
  listing: ChainExpiry,
  strike: number,
  right: Right,
  today: IsoDate,
  asOf: string,
): ChainQuote {
  const spec: LegSpec = { expiry: listing.expiry, strike, right, qty: 1 }
  const theo = legMark(spec, spot, today)
  const t = yearFraction(today, listing.expiry)
  const iv = t > 0 ? surfaceIv(strike, BASE_SPOT, t) : null
  const priced = theo >= UNPRICEABLE
  const sides = priced ? quoteSides(theo) : null
  return {
    contract: contract(listing.expiry, strike, right, listing.root, listing.settlement),
    bid: sides?.bid ?? null,
    ask: sides?.ask ?? null,
    // A contract worth nothing has no mark — the Model tab shows "—" and will
    // not model it, which is the path a real far-wing quote takes too.
    mark: priced ? round2(theo) : null,
    last: null,
    iv: priced ? iv : null,
    iv_hint: null,
    greeks:
      priced && iv !== null
        ? bsmGreeks({ spot, strike, t, vol: iv, rate: RATE, divYield: DIV, right })
        : null,
    // The server's fixture has no tape either.
    volume: null,
    open_interest: null,
    quote_as_of: asOf,
  }
}

function mockChainExpirations(): ChainExpirationsResponse {
  return {
    as_of: nowIso(),
    source: 'yfinance',
    underlying: 'SPX',
    stale: false,
    degraded: [DEGRADED[0] ?? ''],
    expirations: mockExpirations(),
  }
}

/**
 * An SPX chain as the XSP chain it stands for — the mock's twin of the Rust
 * `OptionChain::as_product` + `whole_strikes_only`: strikes, spot and forward
 * ÷ 10, every price ÷ 10, the vol as is, volume and open interest dropped
 * (they are SPX's), and greeks in the option's own terms (delta same, gamma ×
 * 10, theta ÷ 10, vega ÷ 10). XSP is always PM-settled and its root is `XSP`.
 * XSP lists whole-dollar strikes only, so a fractional row (SPX 7745 → 774.5)
 * is dropped unless `keep` names its XSP strike (one the book holds).
 */
export function xspView(
  chain: OptionChainResponse,
  keep: (strike: number) => boolean = () => false,
): OptionChainResponse {
  const tenth = (v: number | null): number | null => (v === null ? null : v / XSP_DIVISOR)
  const quote = (q: ChainQuote | null, strike: number): ChainQuote | null =>
    q === null
      ? null
      : {
          ...q,
          contract: contract(q.contract.expiry, strike, q.contract.right, 'XSP', 'pm', 'XSP'),
          bid: tenth(q.bid),
          ask: tenth(q.ask),
          mark: tenth(q.mark),
          last: tenth(q.last),
          greeks:
            q.greeks === null
              ? null
              : {
                  delta: q.greeks.delta,
                  gamma: q.greeks.gamma * XSP_DIVISOR,
                  theta: q.greeks.theta / XSP_DIVISOR,
                  vega: q.greeks.vega / XSP_DIVISOR,
                },
          volume: null,
          open_interest: null,
        }
  return {
    ...chain,
    underlying: 'XSP',
    spot: chain.spot / XSP_DIVISOR,
    forward: chain.forward / XSP_DIVISOR,
    root: 'XSP',
    settlement: 'pm',
    rows: chain.rows.flatMap((row) => {
      const strike = row.strike / XSP_DIVISOR
      if (!Number.isInteger(strike) && !keep(strike)) return []
      return [{ strike, call: quote(row.call, strike), put: quote(row.put, strike) }]
    }),
  }
}

export function mockOptionChain(req: OptionChainRequest): OptionChainResponse {
  const underlying = req?.underlying ?? 'SPX'
  if (underlying !== 'SPX' && underlying !== 'XSP') {
    throw badRequest(`a chain must be on SPX or XSP, not ${String(underlying)}`)
  }
  if (underlying === 'SPX') return mockSpxOptionChain(req)
  // As service.rs: `XSP` is an alias of the PM root, and an AM root is refused.
  const root = req.root === 'XSP' ? 'SPXW' : req.root
  if (root === 'SPX') {
    throw badRequest(
      `XSP options settle pm, and SPX ${String(req.expiry)} is an AM-settled SPX expiration; ask for SPXW`,
    )
  }
  const held = new Set(
    TRADE_SPECS.filter((s) => s.status === 'open')
      .flatMap((s) => s.legs)
      .filter((l) => l.underlying === 'XSP' && l.expiry === req.expiry)
      .map((l) => l.strike),
  )
  return xspView(mockSpxOptionChain({ ...req, root }), (strike) => held.has(strike))
}

function mockSpxOptionChain(req: OptionChainRequest): OptionChainResponse {
  if (typeof req?.expiry !== 'string' || typeof req?.root !== 'string') {
    throw new MockProblem(400, 'Bad Request', 'an option chain request needs an expiry and a root', 'bad-request')
  }
  const today = MOCK_TODAY
  const listing = mockExpirations(today).find((e) => e.expiry === req.expiry && e.root === req.root)
  if (!listing) {
    throw new MockProblem(
      404,
      'Not Found',
      `${req.root} ${req.expiry} is not a listed expiration`,
      'not-found',
    )
  }
  const asOf = nowIso()
  const t = yearFraction(today, listing.expiry)
  return {
    as_of: asOf,
    source: 'yfinance',
    stale: false,
    degraded: [DEGRADED[0] ?? ''],
    underlying: 'SPX',
    spot: round2(spot),
    expiry: listing.expiry,
    root: listing.root,
    settlement: listing.settlement,
    dte: listing.dte,
    forward: round2(spot * Math.exp((RATE - DIV) * t)),
    rows: mockStrikes(listing.dte).map((strike) => ({
      strike,
      call: chainQuote(listing, strike, 'call', today, asOf),
      put: chainQuote(listing, strike, 'put', today, asOf),
    })),
    warnings: [],
  }
}

function outOfRange(detail: string): MockProblem {
  return new MockProblem(422, 'Unprocessable Entity', detail, 'out-of-range')
}

function badRequest(detail: string): MockProblem {
  return new MockProblem(400, 'Bad Request', detail, 'bad-request')
}

function modelKey(c: ModelContract): string {
  return `${c.underlying}|${c.root}|${c.expiry}|${c.right}|${c.strike}`
}

/**
 * The model position a request's `model_legs` describe — or null when there
 * are none. Same contract summed, zero totals dropped, and every leg entered
 * at today's model mark (unrounded, like the book's value), so its T+0 P&L at
 * spot is exactly zero: the server's "entry = current mark" rule.
 */
function modelSpecFrom(legs: ModelLegDto[] | undefined): TradeSpec | null {
  if (!Array.isArray(legs) || legs.length === 0) return null
  if (legs.length > MAX_MODEL_LEGS) throw outOfRange(`at most ${MAX_MODEL_LEGS} model legs`)
  const merged = new Map<string, { contract: ModelContract; qty: number }>()
  for (const leg of legs) {
    const index = leg.contract.underlying
    if (index !== 'SPX' && index !== 'XSP') {
      throw badRequest(`a modeled contract must be on SPX or XSP, not ${String(index)}`)
    }
    if (!Number.isInteger(leg.qty) || leg.qty === 0 || Math.abs(leg.qty) > MAX_MODEL_QTY) {
      throw outOfRange(`a model leg's qty must be a non-zero integer of at most ${MAX_MODEL_QTY}`)
    }
    const key = modelKey(leg.contract)
    const previous = merged.get(key)
    merged.set(key, { contract: leg.contract, qty: (previous?.qty ?? 0) + leg.qty })
  }
  const today = MOCK_TODAY
  const specs: LegSpec[] = []
  for (const { contract: c, qty } of merged.values()) {
    if (qty === 0) continue
    const base: LegSpec = {
      expiry: c.expiry,
      strike: c.strike,
      right: c.right,
      qty,
      root: c.root,
      settlement: c.settlement,
      underlying: indexOf(c.underlying),
    }
    specs.push({ ...base, avgCost: legMark(base, spot, today) })
  }
  if (specs.length === 0) return null
  return {
    id: 'model',
    name: 'Model',
    status: 'open',
    opened_on: today,
    structure: 'model',
    spotAtOpen: spot,
    staleness: 0,
    legs: specs,
    realized: 0,
    fees: 0,
  }
}

function setModelContracts(req: SetModelContractsRequest): SetModelContractsResponse {
  const contracts = Array.isArray(req?.contracts) ? req.contracts : []
  if (contracts.length > MAX_MODEL_LEGS) throw outOfRange(`at most ${MAX_MODEL_LEGS} model contracts`)
  const distinct = new Set(contracts.map(modelKey)).size
  // The mock prices every contract it is asked about, so nothing is missing.
  return { contracts: distinct, quoted: distinct, missing: [], refreshed: true, detail: null }
}

/** `POST /portfolio`: the trades' stats plus the model position's, whose P&L
 *  is zero by construction and whose greeks are the model's own. */
function mockPortfolio(req: PortfolioRequest): PortfolioStats {
  const wanted = new Set(req?.ids ?? [])
  const trades = [...openTrades(), ...closedTrades()].filter((t) => wanted.has(t.id))
  const model = modelSpecFrom(req?.model_legs)
  if (model === null) return portfolioOf(trades)
  const stats = portfolioOf([...trades, buildTrade(model, MOCK_TODAY, nowIso())])
  // `count` is the number of trades; the model is not one.
  return { ...stats, count: trades.length }
}

/** `SPX 100 (Weeklys) 16 OCT 26 [AM] 7500 PUT` — one contract the way a TOS
 *  order line spells it (the `(Weeklys)` tag is the order's, the `[AM]` the
 *  expiry's). */
function tosContractText(c: LegSpec): string {
  const underlying = c.underlying ?? 'SPX'
  const root = c.root ?? underlying
  // TOS labels an XSP expiration "Weeklys" unless it is the month's third
  // Friday; XSP is always PM, so it never carries `[AM]`. A display
  // convention of the order string only — settlement never comes from the
  // calendar.
  const weeklys =
    underlying === 'XSP'
      ? c.expiry === thirdFridayOf(c.expiry)
        ? ''
        : ' (Weeklys)'
      : root === `${underlying}W`
        ? ' (Weeklys)'
        : ''
  const am = underlying === 'XSP' ? '' : (c.settlement ?? 'am') === 'am' ? ' [AM]' : ''
  const strike = String(Number(c.strike.toFixed(2)))
  return `${underlying} ${MULT}${weeklys} ${formatExpiry(c.expiry)}${am} ${strike} ${c.right.toUpperCase()}`
}

/**
 * `POST /model/orders` — **a deliberate simplification.** The server groups
 * the netted legs into the spreads TOS names (iron condors, butterflies,
 * verticals, …) and snaps single-leg prices to the SPX tick; that grammar
 * lives in Rust (`wingspan_core::tos_format`) and is not re-implemented here.
 * The mock writes one single-leg order per netted leg, limit at its own mark
 * to the cent, so every screen that lists orders has lines to show and copy.
 */
function mockModelOrders(req: ModelOrdersRequest): ModelOrdersResponse {
  const model = modelSpecFrom(req?.model_legs)
  if (model === null) return { orders: [], text: '' }
  const orders: TosOrderDto[] = model.legs.map((leg) => {
    const buy = leg.qty > 0
    const n = Math.abs(leg.qty)
    const price = round2(leg.avgCost ?? 0)
    return {
      strategy: 'single',
      side: buy ? 'buy' : 'sell',
      qty: n,
      line: `${buy ? 'BUY +' : 'SELL -'}${n} ${tosContractText(leg)} @${price.toFixed(2)} LMT`,
      net_price: price,
      legs: [
        {
          contract: contract(leg.expiry, leg.strike, leg.right, leg.root, leg.settlement, leg.underlying),
          qty: leg.qty,
        },
      ],
      unquoted: [],
    }
  })
  return { orders, text: orders.map((order) => `${order.line}\n`).join('') }
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

  /**
   * `slug` is optional, but every trade-source throw carries one: the error
   * slugs (`sheet-not-shared`, `sheet-tab-not-found`, `invalid-trade-source`,
   * …) are asserted by `errorSlug()`, which reads the last segment of
   * `problem.type` — so a slug given here lands there as
   * `https://wingspan.invalid/problems/<slug>`.
   */
  constructor(status: number, title: string, detail: string, slug?: string) {
    super(detail)
    this.name = 'MockProblem'
    this.problem = {
      title,
      status,
      detail,
      ...(slug ? { type: `https://wingspan.invalid/problems/${slug}` } : {}),
    }
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
    if (route === '/trades/source') return deleteTradeSource()
    throw new MockNotFound(`mock: no DELETE route for ${route}`)
  }

  if (method === 'PUT') {
    if (route === '/trades/source') return putTradeSource(body.source as TradeSourceRequest)
    if (route === '/model/contracts') {
      return setModelContracts(body as unknown as SetModelContractsRequest)
    }
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
      case '/trades/source/probe':
        return probeTradeSource(body.source as TradeSourceRequest)
      case '/chain':
        return mockOptionChain(body as unknown as OptionChainRequest)
      case '/portfolio':
        return mockPortfolio(body as unknown as PortfolioRequest)
      case '/model/orders':
        return mockModelOrders(body as unknown as ModelOrdersRequest)
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
    case '/trades/source':
      return tradeSourceResponse()
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
    case '/chain/expirations':
      return mockChainExpirations()
    default:
      throw new MockNotFound(`mock: no route for ${route}`)
  }
}

export const MOCK_SPOT = BASE_SPOT
