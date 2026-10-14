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

/** The option chain's columns (Model tab), SPX chain. Greeks are per
 *  contract, in the same units as the positions table's leg rows. */
export const CHAIN_UNITS = {
  delta: `Per contract. ${GREEK_UNITS.delta}`,
  gamma: `Per contract. ${GREEK_UNITS.gamma}`,
  theta: `Per contract. ${GREEK_UNITS.theta}`,
  vega: `Per contract. ${GREEK_UNITS.vega}`,
  iv: COLUMN_UNITS.iv,
  bid: 'Bid — the provider’s quote, in option points.',
  ask: 'Ask — the provider’s quote, in option points.',
  mark: 'Mark — the price Wingspan values this contract at, in option points. A modeled leg is entered at this mark, so it adds nothing to T+0 at spot.',
  model:
    'Model — a quantity to add to the risk graph: +1 buys one, −1 sells one. Enter commits and moves down the column; Esc reverts; ↑/↓ step by one.',
  pos: 'Position — the quantity held across the trades the risk graph draws (the selection, or every open trade when nothing is selected). Read-only.',
  strike: 'Strike. The forward below it is the one this expiration’s IVs were solved against.',
} as const

/** What an XSP leg's numbers mean on the Positions leg rows and the ticket. */
export const XSP_EXPOSURE_NOTE =
  'SPX-point exposure, as every position greek: an XSP leg counts one tenth of the XSP option’s own Δ and Γ (×10 for XSP points); Θ and V are the same in either unit.'

const XSP_GREEK_NOTE =
  'Per XSP contract, in XSP points — the SPXW contract at 10× the strike: the same Δ, Γ × 10, Θ and V ÷ 10. Positions and the ticket count an XSP leg in SPX points, one tenth of SPX.'

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
