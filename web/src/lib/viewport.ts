/**
 * The visible slice of the price axis: zoom, pan, the price range the server
 * is asked to cover for it, and the y-extent that follows them.
 *
 * The window is pure client-side arithmetic over the *base range* — the
 * `[prices[0], prices[last]]` the unzoomed request answered with. It is no
 * longer held inside that range: the trader may pan anywhere above zero and
 * zoom out to four times it, ONE-style. What keeps that honest is
 * `coverageFor`, which turns whatever they are looking at into the absolute
 * price range a *second* `/risk-graph` request should cover — tiled, so a
 * small pan or a nudge of the zoom re-uses the answer already on screen
 * instead of firing a fresh pricing grid per frame. The header's max profit /
 * max loss / PoP stay the base response's numbers throughout.
 *
 * Kept as pure functions so the rules — a centre left over from a different
 * selection, a window that would cross zero, the tile a coverage request
 * snaps to — are testable without a chart.
 */

import type { RiskLine } from '../api/types'
import { yExtent } from './readout'

/** Zoom as a fraction of the base range, ONE-style: 0.25 = 25%, 5 = 500%. */
export const MIN_ZOOM = 0.25
export const MAX_ZOOM = 5
/** 100%: the window is exactly the range the range mode computed. */
export const DEFAULT_ZOOM = 1
/** Slider steps per doubling: the zoom grid the readout and the thumb share. */
export const ZOOM_STEPS_PER_OCTAVE = 20
/** The zoom slider's step, on its log2 scale. */
export const ZOOM_STEP_LOG2 = 1 / ZOOM_STEPS_PER_OCTAVE
/** The slider's ends, in the log2 units the range input actually holds. */
export const MIN_ZOOM_LOG2 = Math.log2(MIN_ZOOM)
export const MAX_ZOOM_LOG2 = Math.log2(MAX_ZOOM)
/** The most grid points `/risk-graph` accepts (docs/IPC.md). */
export const MAX_POINTS = 2001

export interface PriceWindow {
  min: number
  max: number
}

/** An absolute `price_range` plus the `points` to sample it at. */
export interface Coverage {
  min: number
  max: number
  points: number
}

/** Anything with a P&L column: a `RiskLine` or a per-trade overlay curve. */
type PnlCurve = Pick<RiskLine, 'pnl'>

/**
 * `z` forced into [MIN_ZOOM, MAX_ZOOM]; a non-finite zoom reads as 100%.
 *
 * 0.2.6's scale ran 1×–16×, so a zoom persisted by it can be well past the
 * new top end: it lands on MAX_ZOOM rather than being thrown away.
 */
export function clampZoom(z: number): number {
  if (!Number.isFinite(z)) return DEFAULT_ZOOM
  return Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, z))
}

/**
 * `z` snapped to the zoom slider's own grid, or to either limit.
 *
 * A wheel notch lands on an arbitrary factor; left as is, the range input
 * silently rounds the thumb to its `step` while the readout beside it prints
 * the unrounded number, so the two disagree and the next nudge jumps from
 * somewhere the user cannot see. Snapping on the way into the store keeps the
 * store, the thumb and the readout the same number. The two limits are
 * candidates in their own right because 500% does not sit on the grid, and a
 * wheel must still be able to reach it exactly.
 */
export function snapZoom(z: number): number {
  const zoom = clampZoom(z)
  const log = Math.log2(zoom)
  const grid = clampZoom(2 ** (Math.round(log * ZOOM_STEPS_PER_OCTAVE) / ZOOM_STEPS_PER_OCTAVE))
  let best = grid
  for (const candidate of [MIN_ZOOM, MAX_ZOOM]) {
    if (Math.abs(Math.log2(candidate) - log) < Math.abs(Math.log2(best) - log)) best = candidate
  }
  return best
}

/**
 * The zoom a slider position means.
 *
 * The grid is even in log2, but 500% is not on it (log2 5 = 2.3219, where the
 * grid stops at 2.30) and a range input steps from its *min*: its thumb can
 * never land on a max that is off the step grid. So the last step is read as
 * MAX_ZOOM exactly — the right end of the slider says "500%" and means it.
 */
export function zoomFromSlider(log2Zoom: number): number {
  if (!Number.isFinite(log2Zoom)) return DEFAULT_ZOOM
  if (log2Zoom > MAX_ZOOM_LOG2 - ZOOM_STEP_LOG2 / 2) return MAX_ZOOM
  return snapZoom(2 ** log2Zoom)
}

/** The zoom readout: "100%", "250%", "37%" — whole percent, like ONE. */
export function formatZoom(z: number): string {
  return `${Math.round(clampZoom(z) * 100)}%`
}

/** Extent of the finite prices, or null when there is nothing to plot. */
export function priceRange(prices: readonly number[]): PriceWindow | null {
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
 * The window `xZoom`/`xCenter` select over the base range `prices` spans.
 *
 * Width is the base width divided by the zoom — so 25% is four times the base
 * range and 500% a fifth of it — centred on `xCenter`, or on the *midpoint of
 * the base range* when no centre has been chosen. That fallback is what makes
 * 100% with no centre exactly the base range again, byte for byte.
 *
 * The only clamp left is zero: a window that would reach below it is shifted
 * right, never shrunk, so the zoom readout stays true and the request derived
 * from it never asks the server for a price the model cannot take. There is
 * deliberately no upper bound — panning above the computed range is the point.
 */
export function xWindow(
  prices: readonly number[],
  xZoom: number,
  xCenter: number | null,
): PriceWindow | null {
  const base = priceRange(prices)
  if (!base) return null
  const zoom = clampZoom(xZoom)
  const baseWidth = base.max - base.min
  const centred = xCenter === null || !Number.isFinite(xCenter)
  // Exactly the base range, with none of the arithmetic below: the default
  // view has to be identical to the one that existed before zooming did.
  if ((zoom === DEFAULT_ZOOM && centred) || baseWidth <= 0) return base

  const half = baseWidth / zoom / 2
  const centre =
    xCenter !== null && Number.isFinite(xCenter) ? xCenter : (base.min + base.max) / 2
  const min = Math.max(0, centre - half)
  return { min, max: min + half * 2 }
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
 * The absolute price range a second `/risk-graph` request should cover for
 * `view`, sampled at the base grid's own density.
 *
 * Tiled, not fitted: the unit is the base width halved (or doubled) to the
 * nearest power of two of the window's own width, tiles are anchored at price
 * 0, and the window is padded by half a tile before being rounded outwards.
 * So a drag — which fires per animation frame — keeps asking for the *same*
 * range until it has travelled half a tile past what is already loaded, and a
 * wheel keeps asking for it until the zoom has moved a whole octave. Without
 * that, every frame of a pan would be a new query key and a new pricing grid.
 *
 * `min` floors at 1 because the server rejects a range starting at or below
 * zero, and `points` is capped at the same 2001 it caps them at.
 */
export function coverageFor(
  view: PriceWindow,
  base: PriceWindow,
  basePoints: number,
): Coverage | null {
  const baseWidth = base.max - base.min
  const viewWidth = view.max - view.min
  if (!(baseWidth > 0) || !(viewWidth > 0)) return null
  if (!Number.isFinite(view.min) || !Number.isFinite(view.max)) return null

  const unit = baseWidth * 2 ** Math.round(Math.log2(viewWidth / baseWidth))
  if (!(unit > 0) || !Number.isFinite(unit)) return null
  const min = Math.max(1, Math.floor((view.min - unit / 2) / unit) * unit)
  const wanted = Math.ceil((view.max + unit / 2) / unit) * unit
  const max = wanted > min ? wanted : min + Math.max(unit, 1)

  const density = Number.isFinite(basePoints) ? Math.max(0, basePoints) : 0
  const points = Math.min(MAX_POINTS, Math.max(2, Math.round((density * (max - min)) / baseWidth)))
  return { min, max, points }
}

/**
 * Sample indices the window covers, plus one neighbour on each side so the
 * segments clipped by the frame are measured too. Falls back to the pair
 * bracketing a window narrower than the grid step, and is null when the
 * window sits entirely off one end of the data — panned past what has been
 * fetched, there is nothing on screen to scale the y axis to.
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
    if (left < 0 || right < 0) return null
    return { from: Math.min(left, right), to: Math.max(left, right) }
  }
  return { from: Math.max(0, from - 1), to: Math.min(prices.length - 1, to + 1) }
}

/**
 * y-extent of the curves over the visible window — the same 4% padding as the
 * whole-range `yExtent`, which this delegates to. Null when nothing finite
 * falls inside, which the chart reads as "leave the y axis to ECharts".
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
