/**
 * The visible slice of the price axis: zoom, pan and the y-extent that follows
 * them.
 *
 * All of it is client-side arithmetic over the `prices`/`pnl` arrays the
 * server already answered with — zooming and panning never re-query
 * `/risk-graph`, so the header's max profit / max loss / PoP stay the server's
 * numbers over the *computed* range while the eye works on a slice of it.
 *
 * Kept as pure functions so the clamping rules — a centre left over from a
 * different selection, a window that would hang off either end — are testable
 * without a chart.
 */

import type { RiskLine } from '../api/types'
import { yExtent } from './readout'

export const MIN_ZOOM = 1
export const MAX_ZOOM = 16

export interface PriceWindow {
  min: number
  max: number
}

/** Anything with a P&L column: a `RiskLine` or a per-trade overlay curve. */
type PnlCurve = Pick<RiskLine, 'pnl'>

/** `z` forced into [MIN_ZOOM, MAX_ZOOM]; a non-finite zoom reads as 1. */
export function clampZoom(z: number): number {
  if (!Number.isFinite(z)) return MIN_ZOOM
  return Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, z))
}

/** Extent of the finite prices, or null when there is nothing to plot. */
function fullRange(prices: readonly number[]): PriceWindow | null {
  let min = Number.POSITIVE_INFINITY
  let max = Number.NEGATIVE_INFINITY
  let count = 0
  for (const price of prices) {
    if (!Number.isFinite(price)) continue
    count += 1
    if (price < min) min = price
    if (price > max) max = price
  }
  if (count < 2) return null
  return { min, max }
}

/**
 * The window `xZoom`/`xCenter` select, always inside `[prices[0], prices[last]]`.
 *
 * The width is the full range divided by the zoom; a centre that would push
 * the window off an end is *shifted* back in rather than shrunk, so the zoom
 * readout stays true and a stale centre — one left over from another
 * selection, range mode or spot — can never produce an empty, inverted or
 * out-of-range window. `xCenter === null` centres on spot.
 */
export function xWindow(
  prices: readonly number[],
  xZoom: number,
  xCenter: number | null,
  spot: number,
): PriceWindow | null {
  const full = fullRange(prices)
  if (!full) return null
  const zoom = clampZoom(xZoom)
  const fullWidth = full.max - full.min
  if (zoom <= MIN_ZOOM || fullWidth <= 0) return full

  const half = fullWidth / zoom / 2
  const fallback = Number.isFinite(spot) ? spot : (full.min + full.max) / 2
  const wanted = xCenter !== null && Number.isFinite(xCenter) ? xCenter : fallback
  const centre = Math.min(full.max - half, Math.max(full.min + half, wanted))
  return { min: centre - half, max: centre + half }
}

/**
 * Zoom by `factor` about the price `anchor` — the price under the wheel keeps
 * the same fraction of the window, so the chart grows away from the cursor
 * rather than from the middle. Clamped at MIN_ZOOM/MAX_ZOOM, where the
 * effective factor shrinks to whatever is left.
 */
export function zoomAt(
  xZoom: number,
  factor: number,
  anchor: number,
  currentWindow: PriceWindow,
): { xZoom: number; xCenter: number } {
  const from = clampZoom(xZoom)
  const to = clampZoom(Number.isFinite(factor) && factor > 0 ? from * factor : from)
  const width = currentWindow.max - currentWindow.min
  const centre = (currentWindow.min + currentWindow.max) / 2
  if (!(width > 0)) return { xZoom: to, xCenter: centre }
  const nextWidth = (width * from) / to
  const price = Number.isFinite(anchor) ? anchor : centre
  const fraction = Math.min(1, Math.max(0, (price - currentWindow.min) / width))
  return { xZoom: to, xCenter: price + nextWidth * (0.5 - fraction) }
}

/**
 * Sample indices the window covers, plus one neighbour on each side so the
 * segments clipped by the frame are measured too. Falls back to the pair
 * bracketing a window narrower than the grid step.
 */
function indexRange(
  prices: readonly number[],
  view: PriceWindow,
): { from: number; to: number } | null {
  let from = -1
  let to = -1
  let left = -1
  let right = -1
  for (let i = 0; i < prices.length; i += 1) {
    const price = prices[i]
    if (price === undefined || !Number.isFinite(price)) continue
    if (price >= view.min && price <= view.max) {
      if (from < 0) from = i
      to = i
    } else if (price < view.min) {
      left = i
    } else if (right < 0) {
      right = i
    }
  }
  if (from < 0) {
    if (left < 0 && right < 0) return null
    const lo = left < 0 ? right : left
    const hi = right < 0 ? left : right
    return { from: Math.min(lo, hi), to: Math.max(lo, hi) }
  }
  return { from: Math.max(0, from - 1), to: Math.min(prices.length - 1, to + 1) }
}

/**
 * y-extent of the curves over the visible window — the same 4% padding as the
 * whole-range `yExtent`, which this delegates to. Null when nothing finite
 * falls inside.
 */
export function yExtentInWindow(
  lines: readonly PnlCurve[],
  prices: readonly number[],
  view: PriceWindow,
): { min: number; max: number } | null {
  const range = indexRange(prices, view)
  if (!range) return null
  return yExtent(lines, range)
}
