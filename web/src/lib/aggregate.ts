/** Selection, sorting and client-side aggregation over the trade list.
 *
 *  The server owns the authoritative `/portfolio` numbers (its weighted-vega
 *  reference is recomputed for the selected set, which a naive sum cannot
 *  reproduce). These helpers give the table its instant, offline-safe view and
 *  a placeholder while that request is in flight. */

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

/** The earliest expiry among open (non-zero qty) legs across `trades`. */
export function frontExpiry(trades: Trade[]): IsoDate | null {
  let best: IsoDate | null = null
  for (const trade of trades) {
    for (const leg of trade.legs) {
      if (leg.qty === 0) continue
      if (best === null || leg.contract.expiry < best) best = leg.contract.expiry
    }
  }
  return best
}

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

export function aggregateTrades(trades: Trade[]): PortfolioStats {
  const greeks = trades.reduce<Greeks>(
    (acc, t) => ({
      delta: acc.delta + t.greeks.delta,
      gamma: acc.gamma + t.greeks.gamma,
      theta: acc.theta + t.greeks.theta,
      vega: acc.vega + t.greeks.vega,
    }),
    ZERO,
  )
  const front = frontExpiry(trades)
  return {
    count: trades.length,
    pnl_open: trades.reduce((a, t) => a + t.pnl_open, 0),
    pnl_total: trades.reduce((a, t) => a + t.pnl_total, 0),
    greeks,
    weighted_vega: trades.reduce((a, t) => a + t.weighted_vega, 0),
    weighted_vega_reference: front ? `front_expiry (${front})` : 'front_expiry (—)',
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

