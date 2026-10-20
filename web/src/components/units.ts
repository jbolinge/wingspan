/** The units the server publishes for every number the table shows.
 *  Surfaced as column tooltips so a greek is never ambiguous. */

import type { ChainUnderlying } from '../api/types'

export const GREEK_UNITS = {
  delta: 'Delta — position P&L change in dollars per 1 index point move in SPX. Σ qty × 100 × leg delta.',
  gamma: 'Gamma — change in position delta per 1 index point move.',
  theta:
    'Theta — ONE-style: the position repriced one calendar day forward, in dollars. Negative means time costs you.',
  vega: 'Vega — position P&L change in dollars per 1.00 vol point (0.01 of decimal vol).',
  weightedVega:
    'Weighted vega — Σ leg vega × (REF_DTE / DTE_leg)^0.5, so near-dated vega counts for more than far-dated vega. REF_DTE is 30 days by default (configurable: another number of days, or the front expiry of the set).',
} as const

export const COLUMN_UNITS = {
  dit: 'Days in trade — calendar days from the trade file name date (your designation of the open date) to today, or to the close date.',
  dte: 'Days to expiry — calendar days to the front expiry among open legs.',
  cost: 'Cost basis — |net premium|. A debit shows as a positive cost; a credit trade shows the credit received.',
  value: 'Value — Σ mark × qty × multiplier over open legs.',
  pnlOpen: 'Open P&L — Σ (mark − average cost) × qty × multiplier. Excludes realised P&L and fees.',
  pnlTotal: 'Total P&L — open P&L plus realised P&L minus fees. This is what the T+0 curve equals at spot.',
  pnlPct: '% of cost basis. "—" when the basis is zero.',
  structure:
    'Netted structure across every matched order in the trade file. Quarantined orders are excluded.',
  staleness: 'Age of the oldest quote behind this row. Green < 1 min, amber < 15 min, red < 2 h.',
  iv: 'Implied volatility solved by Wingspan from the mark — never the provider hint — so the table and the graph always agree.',
} as const

/**
 * One contract's greeks, per share — the leg rows and the SPX chain. Not the
 * position texts above with "per contract" stuck on: those are dollars
 * summed over qty × 100, these are one option's own sensitivities in option
 * points (a position counts qty × 100 of each). These are the SPX chain's
 * texts; the leg rows add `LEG_SCOPE`.
 */
const CHAIN_GREEK_UNITS = {
  delta: 'Per contract, per share: the option’s price change, in option points, per 1-point move in SPX.',
  gamma: 'Per contract, per share: the change in the option’s delta per 1-point move in SPX.',
  theta:
    'Per contract, per share: the option repriced one calendar day forward, in option points. Negative means time costs you.',
  vega: 'Per contract, per share: the option’s price change, in option points, per 1.00 vol point (0.01 of decimal vol).',
} as const

const LEG_SCOPE =
  ' One contract’s own sensitivity, not the position’s: the trade row above counts qty × 100 of it, in dollars. An XSP leg is 0.1 × its SPXW proxy, per 1-point move in SPX.'

export const LEG_GREEK_UNITS = {
  delta: `${CHAIN_GREEK_UNITS.delta}${LEG_SCOPE}`,
  gamma: `${CHAIN_GREEK_UNITS.gamma}${LEG_SCOPE}`,
  theta: `${CHAIN_GREEK_UNITS.theta}${LEG_SCOPE}`,
  vega: `${CHAIN_GREEK_UNITS.vega}${LEG_SCOPE}`,
} as const

/** The option chain's columns (Model tab), SPX chain. Greeks are per
 *  contract, per share, in the same units as the positions table's leg rows. */
export const CHAIN_UNITS = {
  delta: CHAIN_GREEK_UNITS.delta,
  gamma: CHAIN_GREEK_UNITS.gamma,
  theta: CHAIN_GREEK_UNITS.theta,
  vega: CHAIN_GREEK_UNITS.vega,
  iv: COLUMN_UNITS.iv,
  bid: 'Bid — the provider’s quote, in option points.',
  ask: 'Ask — the provider’s quote, in option points.',
  mark: 'Mark — the price Wingspan values this contract at, in option points. A modeled leg is entered at this mark, so it adds nothing to T+0 at spot.',
  model:
    'Model — a quantity to add to the risk graph: +1 buys one, −1 sells one. Enter commits and moves down the column; Esc reverts; ↑/↓ step by one.',
  pos: 'Position — the quantity held across the trades the risk graph draws (the selection, or every open trade when nothing is selected). Read-only.',
  strike: 'Strike. The forward below it is the one this expiration’s IVs were solved against.',
} as const

/** What an XSP leg's numbers mean on the Positions leg rows and the ticket:
 *  SPX-point exposure is 0.1 × the SPXW proxy's greeks, which against the
 *  XSP option's own (chain) greeks is Δ × 0.1, Γ × 0.01, Θ and V unchanged. */
export const XSP_EXPOSURE_NOTE =
  'SPX-point exposure, as every position greek: 0.1 × the greeks of the SPXW proxy (the SPX option at 10× the strike). Against the XSP option’s own chain greeks that is Δ × 0.1 (× 10 for XSP points), Γ × 0.01 (× 100 for XSP points), Θ and V unchanged.'

/** The unit every price on the risk graph is in — its x-axis. */
export const SPX_POINTS = 'SPX pts'

/** Shown with the graph's price controls when the drawn position holds XSP
 *  legs: the axis is SPX points by design, so an XSP strike typed into a
 *  price field would be read as an SPX price ten times too low. */
export const XSP_AXIS_NOTE =
  'XSP legs are drawn in SPX points: the price axis, range, spot override, what-if price and breakevens are all SPX prices — XSP 650 is SPX 6500.'

const XSP_GREEK_NOTE =
  'Per XSP contract, in XSP points, per share — against the SPXW contract at 10× the strike: the same Δ, Γ × 10, Θ and V ÷ 10. Positions and the ticket count an XSP leg in SPX points instead, as 0.1 × the SPXW proxy’s greeks.'

/** The XSP chain's columns: greeks in the XSP option's own points (not the
 *  positions table's SPX-point units), prices the SPXW quote ÷ 10. */
export const XSP_CHAIN_UNITS = {
  ...CHAIN_UNITS,
  delta: `${XSP_GREEK_NOTE} Delta — option price change per 1-point move in XSP.`,
  gamma: `${XSP_GREEK_NOTE} Gamma — change in delta per 1-point move in XSP.`,
  theta: `${XSP_GREEK_NOTE} Theta — the option repriced one calendar day forward, in XSP option points.`,
  vega: `${XSP_GREEK_NOTE} Vega — option price change per 1.00 vol point.`,
  bid: 'Bid — the SPXW quote at 10× the strike, ÷ 10. No XSP quote is fetched.',
  ask: 'Ask — the SPXW quote at 10× the strike, ÷ 10. No XSP quote is fetched.',
  mark: 'Mark — the SPXW mark at 10× the strike, ÷ 10, in XSP option points. A modeled leg is entered at this mark, so it adds nothing to T+0 at spot.',
  strike:
    'Strike, in XSP points (the SPXW strike ÷ 10; whole dollars only, as XSP lists them). The forward below it is SPX’s ÷ 10.',
} as const

/** The column tooltips for a chain on `underlying`. */
export function chainUnits(underlying: ChainUnderlying | string): Readonly<Record<keyof typeof CHAIN_UNITS, string>> {
  return underlying === 'XSP' ? XSP_CHAIN_UNITS : CHAIN_UNITS
}
