/**
 * Price-slice table and grid-index helpers.
 *
 * `buildSlices` is a pure function of the `/risk-graph` response so it stays
 * honest: the numbers in the table are the same array the chart drew, not a
 * second, subtly different model.
 */

import type { RiskGraphResponse, RiskLine } from '../api/types'
import { lineLabel } from './lines'

/** Index of the grid point nearest `price`, however far away that is.
 *
 *  A plain nearest-distance scan, so it holds on the non-uniform grid the
 *  server sends — uniform samples plus spot and every in-range strike
 *  inserted in order — where an index computed from a fixed step would land
 *  beside a dense cluster of strikes instead of on the right node. An exact
 *  node (spot, a strike) is found exactly. */
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
 * pointing. Half a grid step of slack past each end, because ECharts' axis
 * pointer reports the snapped sample it drew, not the raw pixel price — the
 * *local* step at that end, since the grid is not uniformly spaced (inserted
 * strikes and spot) and a mean step would be the wrong slack at either end.
 */
export function indexAt(prices: readonly number[], price: number): number | null {
  if (prices.length === 0 || !Number.isFinite(price)) return null
  const first = prices[0]
  const last = prices[prices.length - 1]
  if (first === undefined || last === undefined) return null
  if (!Number.isFinite(first) || !Number.isFinite(last)) return null
  const lo = Math.min(first, last)
  const hi = Math.max(first, last)
  const second = prices[1]
  const penultimate = prices[prices.length - 2]
  const halfStep = (a: number | undefined, b: number | undefined): number =>
    a !== undefined && b !== undefined && Number.isFinite(a) && Number.isFinite(b) ? Math.abs(b - a) / 2 : 0
  const loSlack = prices.length > 1 ? halfStep(first, second) : 0
  const hiSlack = prices.length > 1 ? halfStep(penultimate, last) : 0
  if (price < lo - loSlack || price > hi + hiSlack) return null
  return nearestPriceIndex(prices, price)
}

export interface SliceRow {
  price: number
  /** Label such as `spot`, `-5%`, `BE EXP Dec 18 2026` (a breakeven names
   *  the line it is a breakeven of). */
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

/**
 * Rows for the price-slice table: fixed offsets from spot plus breakevens.
 *
 * Breakevens are only the given (visible) lines' own, each labelled with the
 * line it belongs to: a hidden line's crossings are not on screen, and an
 * unlabelled `BE` row for a T+x crossing reads as an expiration breakeven.
 */
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
    for (const line of lines) {
      for (const be of data.breakevens[line.key] ?? []) {
        if (be >= first && be <= last) points.push({ price: be, label: `BE ${lineLabel(line)}` })
      }
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
