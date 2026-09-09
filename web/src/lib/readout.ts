/**
 * Crosshair readout and price-slice table.
 *
 * Both are pure functions of the `/risk-graph` response so they stay honest:
 * the numbers under the cursor are the same array the chart drew, and the
 * per-line delta/theta are finite differences of those curves rather than a
 * second, subtly different model.
 */

import type { IsoDate, RiskGraphResponse, RiskLine, RiskLineKind } from '../api/types'
import { daysBetween } from './dates'

export interface ReadoutRow {
  /** `key` of the line this row reads, unique where the date is not. */
  key: string
  date: IsoDate
  kind: RiskLineKind
  pnl: number
  /** dP&L / dPrice at the cursor, in dollars per index point. */
  delta: number | null
  /** dP&L / dDay against the next line out, in dollars per calendar day. */
  theta: number | null
}

export interface Readout {
  price: number
  index: number
  rows: ReadoutRow[]
}

function slopeAt(prices: number[], pnl: number[], index: number): number | null {
  const lo = Math.max(0, index - 1)
  const hi = Math.min(prices.length - 1, index + 1)
  if (hi === lo) return null
  const p0 = prices[lo]
  const p1 = prices[hi]
  const v0 = pnl[lo]
  const v1 = pnl[hi]
  if (p0 === undefined || p1 === undefined || v0 === undefined || v1 === undefined) return null
  if (p1 === p0) return null
  return (v1 - v0) / (p1 - p0)
}

/**
 * Per-line P&L (and its local delta/theta) at price index `index`.
 *
 * Reads exactly the `lines` it is handed — callers showing a filtered set must
 * pass the filtered lines, so the rows (and the theta between them) match the
 * curves actually plotted.
 */
export function readoutAt(
  data: Pick<RiskGraphResponse, 'prices' | 'lines'>,
  index: number,
): Readout | null {
  const { prices, lines } = data
  if (prices.length === 0) return null
  const i = Math.max(0, Math.min(prices.length - 1, index))
  const price = prices[i]
  if (price === undefined) return null
  const ordered = [...lines].sort((a, b) => a.date.localeCompare(b.date))
  const rows = ordered.map((line, position) => {
    const pnl = line.pnl[i] ?? Number.NaN
    const next = ordered[position + 1]
    let theta: number | null = null
    if (next) {
      const days = daysBetween(line.date, next.date)
      const nextPnl = next.pnl[i]
      if (days > 0 && nextPnl !== undefined && Number.isFinite(pnl)) {
        theta = (nextPnl - pnl) / days
      }
    }
    return {
      key: line.key,
      date: line.date,
      kind: line.kind,
      pnl,
      delta: slopeAt(prices, line.pnl, i),
      theta,
    }
  })
  return { price, index: i, rows }
}

/** Index of the grid point nearest `price`, however far away that is. */
export function nearestPriceIndex(prices: readonly number[], price: number): number {
  if (prices.length === 0) return 0
  let best = 0
  let bestDistance = Number.POSITIVE_INFINITY
  for (let i = 0; i < prices.length; i += 1) {
    const candidate = prices[i]
    if (candidate === undefined) continue
    const distance = Math.abs(candidate - price)
    if (distance < bestDistance) {
      bestDistance = distance
      best = i
    }
  }
  return best
}

/**
 * Index of the grid point at `price`, or null when `price` is off the grid.
 *
 * `nearestPriceIndex` has no distance limit, which was harmless while the
 * window was clamped inside the fetched data: the cursor was always over a
 * sample. It no longer is — the window can be panned anywhere above zero, and
 * the coverage tile behind it need not contain spot — and clamping to an edge
 * sample there reads out the P&L of a price nowhere near where the trader is
 * pointing. Half a grid step of slack, because ECharts' axis pointer reports
 * the snapped sample it drew, not the raw pixel price.
 */
export function indexAt(prices: readonly number[], price: number): number | null {
  if (prices.length === 0 || !Number.isFinite(price)) return null
  const first = prices[0]
  const last = prices[prices.length - 1]
  if (first === undefined || last === undefined) return null
  if (!Number.isFinite(first) || !Number.isFinite(last)) return null
  const lo = Math.min(first, last)
  const hi = Math.max(first, last)
  const slack = prices.length > 1 ? (hi - lo) / (prices.length - 1) / 2 : 0
  if (price < lo - slack || price > hi + slack) return null
  return nearestPriceIndex(prices, price)
}

export interface SliceRow {
  price: number
  /** Label such as `spot`, `-5%`, `BE`. */
  label: string
  /** P&L per line, in the order the lines were given. */
  values: number[]
}

export interface SliceOptions {
  /** Percentage offsets from spot, e.g. `[-0.1, -0.05, 0, 0.05, 0.1]`. */
  offsets?: number[]
  includeBreakevens?: boolean
}

const DEFAULT_OFFSETS = [-0.1, -0.05, -0.025, 0, 0.025, 0.05, 0.1]

/** Rows for the price-slice table: fixed offsets from spot plus breakevens. */
export function buildSlices(
  data: Pick<RiskGraphResponse, 'prices' | 'lines' | 'spot' | 'breakevens'>,
  lines: RiskLine[],
  options: SliceOptions = {},
): SliceRow[] {
  const { offsets = DEFAULT_OFFSETS, includeBreakevens = true } = options
  const { prices, spot } = data
  if (prices.length === 0) return []
  const first = prices[0] ?? spot
  const last = prices[prices.length - 1] ?? spot
  const points: { price: number; label: string }[] = offsets
    .map((offset) => ({
      price: spot * (1 + offset),
      label: offset === 0 ? 'spot' : `${offset > 0 ? '+' : ''}${(offset * 100).toFixed(1)}%`,
    }))
    .filter((point) => point.price >= first && point.price <= last)

  if (includeBreakevens) {
    for (const be of Object.values(data.breakevens).flat()) {
      if (be >= first && be <= last) points.push({ price: be, label: 'BE' })
    }
  }

  const seen = new Set<number>()
  return points
    .sort((a, b) => a.price - b.price)
    .flatMap((point) => {
      const index = nearestPriceIndex(prices, point.price)
      if (seen.has(index)) return []
      seen.add(index)
      return [
        {
          price: prices[index] ?? point.price,
          label: point.label,
          values: lines.map((line) => line.pnl[index] ?? Number.NaN),
        },
      ]
    })
}

/**
 * y-extent of the visible lines, padded by 4% so nothing touches the frame.
 *
 * `range` narrows the scan to a slice of the price grid (inclusive), which is
 * how the zoomed viewport rescales the y axis to what is actually on screen —
 * the padding rule lives here once, for both callers.
 */
export function yExtent(
  lines: readonly Pick<RiskLine, 'pnl'>[],
  range?: { from: number; to: number },
): { min: number; max: number } | null {
  let min = Number.POSITIVE_INFINITY
  let max = Number.NEGATIVE_INFINITY
  for (const line of lines) {
    const from = Math.max(0, range?.from ?? 0)
    const to = Math.min(line.pnl.length - 1, range?.to ?? line.pnl.length - 1)
    for (let i = from; i <= to; i += 1) {
      const value = line.pnl[i]
      if (value === undefined || !Number.isFinite(value)) continue
      if (value < min) min = value
      if (value > max) max = value
    }
  }
  if (!Number.isFinite(min) || !Number.isFinite(max)) return null
  const pad = Math.max(1, (max - min) * 0.04)
  return { min: min - pad, max: max + pad }
}
