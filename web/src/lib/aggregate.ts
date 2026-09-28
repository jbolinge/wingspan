/** Selection, sorting and client-side aggregation over the trade list.
 *
 *  The server owns the authoritative `/portfolio` numbers. Every row's
 *  `weighted_vega` is measured against the whole book's reference, so with a
 *  fixed reference (the `30d` default) the sum of any rows is exact; only in
 *  `front_expiry` mode does the server re-resolve the reference for the
 *  selected set, which a sum cannot reproduce. These helpers give the table
 *  its instant, offline-safe view and a placeholder while that request is in
 *  flight. */

import type { Greeks, IsoDate, PortfolioStats, Trade } from '../api/types'

export const SORT_KEYS = [
  'name',
  'opened_on',
  'dit',
  'dte',
  'structure',
  'cost',
  'value',
  'pnl_open',
  'pnl_total',
  'pnl_pct',
  'delta',
  'gamma',
  'theta',
  'vega',
  'weighted_vega',
  'staleness_secs',
  'warnings',
] as const

export type SortKey = (typeof SORT_KEYS)[number]
export type SortDir = 'asc' | 'desc'

const ZERO: Greeks = { delta: 0, gamma: 0, theta: 0, vega: 0 }

/** Every distinct expiry with open legs, ascending — the per-expiry vol rows. */
export function expiriesOf(trades: Trade[]): IsoDate[] {
  const set = new Set<IsoDate>()
  for (const trade of trades) {
    for (const leg of trade.legs) {
      if (leg.qty !== 0) set.add(leg.contract.expiry)
    }
  }
  return [...set].sort()
}

/** The label a placeholder carries when it was given no reference. */
const NO_REFERENCE = '—'

/** Whether a server reference label (`front_expiry (YYYY-MM-DD)` or `<n>d`)
 *  names the front-expiry mode rather than a fixed horizon. */
export function isFrontExpiryReference(label: string): boolean {
  return label.startsWith('front_expiry')
}

/** The label the Selected placeholder carries: the book's reference, which
 *  every summed row is measured against — except for an empty selection in
 *  `front_expiry` mode, which has no front expiry of its own. */
export function selectedPlaceholderReference(
  bookReference: string | null,
  selectedCount: number,
): string | null {
  if (bookReference === null) return null
  if (selectedCount === 0 && isFrontExpiryReference(bookReference)) return null
  return bookReference
}

/** What the Selected card's weighted vega is measured against, in words;
 *  empty while no reference is known yet. `provisional` marks the summed-rows
 *  placeholder shown until `/portfolio` answers for the selection. */
export function selectedReferenceNote(label: string, provisional = false): string {
  if (isFrontExpiryReference(label)) {
    return provisional
      ? "The sum of the rows, measured against the whole book's front expiry until the selection's own reference loads."
      : 'The reference is the front expiry of the selected set, so it moves as you change the selection.'
  }
  if (label === NO_REFERENCE) return ''
  return 'A fixed horizon: every row, the selection and the whole book are measured against it, so the W.V column adds up to these totals.'
}

/** Sum `trades` into portfolio stats.
 *
 *  `reference` is the book's resolved weighted-vega label
 *  (`portfolio_all.weighted_vega_reference`), which every row's
 *  `weighted_vega` is measured against — so the sum is exactly the W.V against
 *  that label, and with the default fixed reference it equals the server's
 *  `/portfolio` number. Without one the label is `—`, never a guess. */
export function aggregateTrades(trades: Trade[], reference: string | null = null): PortfolioStats {
  const greeks = trades.reduce<Greeks>(
    (acc, t) => ({
      delta: acc.delta + t.greeks.delta,
      gamma: acc.gamma + t.greeks.gamma,
      theta: acc.theta + t.greeks.theta,
      vega: acc.vega + t.greeks.vega,
    }),
    ZERO,
  )
  return {
    count: trades.length,
    pnl_open: trades.reduce((a, t) => a + t.pnl_open, 0),
    pnl_total: trades.reduce((a, t) => a + t.pnl_total, 0),
    greeks,
    weighted_vega: trades.reduce((a, t) => a + t.weighted_vega, 0),
    weighted_vega_reference: reference ?? NO_REFERENCE,
  }
}

export function tradesById(trades: Trade[], ids: readonly string[]): Trade[] {
  const wanted = new Set(ids)
  return trades.filter((t) => wanted.has(t.id))
}

/* ---------- selection set algebra (pure, order-stable) ---------- */

export function selectAllIds(trades: Trade[]): string[] {
  return trades.map((t) => t.id)
}

export function invertIds(trades: Trade[], ids: readonly string[]): string[] {
  const current = new Set(ids)
  return trades.filter((t) => !current.has(t.id)).map((t) => t.id)
}

/** Drop ids that no longer exist (a trade file was renamed or closed). */
export function pruneIds(trades: Trade[], ids: readonly string[]): string[] {
  const live = new Set(trades.map((t) => t.id))
  return ids.filter((id) => live.has(id))
}

/* ---------- sorting ---------- */

function sortValue(trade: Trade, key: SortKey): number | string {
  switch (key) {
    case 'name':
      return trade.name.toLowerCase()
    case 'opened_on':
      return trade.opened_on
    case 'structure':
      return trade.structure.toLowerCase()
    case 'dte':
      return trade.dte ?? Number.POSITIVE_INFINITY
    case 'pnl_pct':
      return trade.pnl_pct ?? Number.NEGATIVE_INFINITY
    case 'delta':
    case 'gamma':
    case 'theta':
    case 'vega':
      return trade.greeks[key]
    case 'staleness_secs':
      return trade.staleness_secs ?? Number.POSITIVE_INFINITY
    case 'warnings':
      return trade.warnings.length + trade.quarantined.length
    default:
      return trade[key]
  }
}

/** Stable sort; ties fall back to the trade name so the order never jitters. */
export function sortTrades(trades: Trade[], key: SortKey, dir: SortDir): Trade[] {
  const factor = dir === 'asc' ? 1 : -1
  return [...trades].sort((a, b) => {
    const av = sortValue(a, key)
    const bv = sortValue(b, key)
    let cmp: number
    if (typeof av === 'string' || typeof bv === 'string') {
      cmp = String(av).localeCompare(String(bv))
    } else {
      cmp = av === bv ? 0 : av < bv ? -1 : 1
    }
    // The tie-break stays ascending in both directions so flipping the sort
    // never reshuffles rows that compare equal.
    return cmp === 0 ? a.name.localeCompare(b.name) : cmp * factor
  })
}

/**
 * Total warning weight for the ⚠ badge. The server emits one
 * `duplicate_execution_claim` warning per quarantined order, so quarantined
 * orders only add weight when no such warning represents them (e.g. mock data).
 */
export function warningCount(trade: Trade): number {
  const claimed = trade.warnings.filter(
    (w) => w.code === 'duplicate_execution_claim',
  ).length
  return trade.warnings.length + Math.max(0, trade.quarantined.length - claimed)
}

export function worstSeverity(trade: Trade): 'error' | 'warn' | 'info' | null {
  if (trade.quarantined.length > 0) return 'error'
  let worst: 'error' | 'warn' | 'info' | null = null
  for (const w of trade.warnings) {
    if (w.severity === 'error') return 'error'
    if (w.severity === 'warn') worst = 'warn'
    else if (worst === null) worst = 'info'
  }
  return worst
}

