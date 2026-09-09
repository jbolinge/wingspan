import { describe, expect, it } from 'vitest'
import type { RiskLine } from '../api/types'
import {
  MAX_ZOOM,
  MIN_ZOOM,
  ZOOM_STEP_LOG2,
  clampZoom,
  snapZoom,
  xWindow,
  yExtentInWindow,
  zoomAt,
} from './viewport'

const prices = [6000, 6100, 6200, 6300, 6400]
const spot = 6200

const lines: RiskLine[] = [
  { date: '2026-08-31', kind: 't0', key: 't0', label: 'T+0', pnl: [-400, -200, 0, 200, 400] },
  {
    date: '2026-10-16',
    kind: 'expiry',
    key: 'expiry',
    label: 'Exp 2026-10-16',
    pnl: [-800, -400, 0, 400, 800],
  },
]

describe('clampZoom', () => {
  it('holds the zoom between 1 and 16', () => {
    expect(clampZoom(0.25)).toBe(MIN_ZOOM)
    expect(clampZoom(2.5)).toBe(2.5)
    expect(clampZoom(1000)).toBe(MAX_ZOOM)
  })

  /** A NaN or an Infinity can only come from a corrupted persisted blob;
   *  showing the whole range is the honest answer to it. */
  it('reads a non-finite zoom as 1', () => {
    expect(clampZoom(Number.NaN)).toBe(MIN_ZOOM)
    expect(clampZoom(Number.POSITIVE_INFINITY)).toBe(MIN_ZOOM)
  })
})

describe('snapZoom', () => {
  /** The range input rounds the thumb to its step whatever the store holds;
   *  snapping on the way in keeps the thumb, the readout and the store from
   *  telling the trader three different zooms after a wheel notch. */
  it('lands on the slider grid, exactly', () => {
    const snapped = snapZoom(5.7333176582)
    expect(Math.log2(snapped) / ZOOM_STEP_LOG2).toBeCloseTo(
      Math.round(Math.log2(snapped) / ZOOM_STEP_LOG2),
      9,
    )
    expect(snapped).toBeCloseTo(5.6568542, 6)
  })

  it('leaves the round zooms the slider itself writes untouched', () => {
    expect(snapZoom(1)).toBe(1)
    expect(snapZoom(4)).toBe(4)
    expect(snapZoom(MAX_ZOOM)).toBe(MAX_ZOOM)
  })

  it('clamps like clampZoom does', () => {
    expect(snapZoom(0)).toBe(MIN_ZOOM)
    expect(snapZoom(Number.NaN)).toBe(MIN_ZOOM)
    expect(snapZoom(1000)).toBe(MAX_ZOOM)
  })
})

describe('xWindow', () => {
  it('is the whole computed range at zoom 1', () => {
    expect(xWindow(prices, 1, null, spot)).toEqual({ min: 6000, max: 6400 })
    expect(xWindow(prices, 0.5, 6300, spot)).toEqual({ min: 6000, max: 6400 })
  })

  it('halves the width per doubling of the zoom, centred on the centre', () => {
    expect(xWindow(prices, 2, 6200, spot)).toEqual({ min: 6100, max: 6300 })
    expect(xWindow(prices, 4, 6200, spot)).toEqual({ min: 6150, max: 6250 })
  })

  it('centres on spot when no centre has been chosen', () => {
    expect(xWindow(prices, 4, null, 6100)).toEqual({ min: 6050, max: 6150 })
  })

  /** The window keeps its width at the edges — shifted back in, never
   *  squashed — so the zoom readout stays true. */
  it('shifts a window that would hang off either end back inside', () => {
    expect(xWindow(prices, 2, 6000, spot)).toEqual({ min: 6000, max: 6200 })
    expect(xWindow(prices, 2, 6400, spot)).toEqual({ min: 6200, max: 6400 })
  })

  /** A centre left over from another selection, range mode or spot must never
   *  produce an empty, inverted or out-of-range window. */
  it('clamps a stale centre far outside the range', () => {
    expect(xWindow(prices, 4, 99_000, spot)).toEqual({ min: 6300, max: 6400 })
    expect(xWindow(prices, 4, -99_000, spot)).toEqual({ min: 6000, max: 6100 })
    expect(xWindow(prices, 4, Number.NaN, spot)).toEqual({ min: 6150, max: 6250 })
  })

  it('is null without at least two finite prices', () => {
    expect(xWindow([], 2, null, spot)).toBeNull()
    expect(xWindow([6200], 2, null, spot)).toBeNull()
    expect(xWindow([6200, Number.NaN], 2, null, spot)).toBeNull()
  })

  it('falls back to the range midpoint when spot is not finite either', () => {
    expect(xWindow(prices, 4, null, Number.NaN)).toEqual({ min: 6150, max: 6250 })
  })

  it('survives a degenerate range where every price is the same', () => {
    expect(xWindow([6200, 6200], 4, null, spot)).toEqual({ min: 6200, max: 6200 })
  })
})

describe('zoomAt', () => {
  const view = { min: 6100, max: 6300 }

  it('keeps the anchor price at the same fraction of the window', () => {
    const next = zoomAt(2, 2, 6150, view)
    expect(next.xZoom).toBe(4)
    // 6150 sits a quarter in; after halving the width it must still be a
    // quarter in — window 6125…6175, centre 6150.
    const after = xWindow(prices, next.xZoom, next.xCenter, spot)
    expect((6150 - after!.min) / (after!.max - after!.min)).toBeCloseTo(0.25, 10)
  })

  it('zooms out about the anchor as well', () => {
    const next = zoomAt(4, 0.5, 6150, { min: 6150, max: 6250 })
    expect(next.xZoom).toBe(2)
    expect(next.xCenter).toBeCloseTo(6250, 10)
  })

  it('clamps at both ends of the zoom range', () => {
    expect(zoomAt(MAX_ZOOM, 4, 6200, view).xZoom).toBe(MAX_ZOOM)
    expect(zoomAt(1.2, 0.01, 6200, view).xZoom).toBe(MIN_ZOOM)
  })

  it('ignores a nonsense factor or anchor and falls back to the centre', () => {
    expect(zoomAt(2, Number.NaN, 6200, view)).toEqual({ xZoom: 2, xCenter: 6200 })
    expect(zoomAt(2, 2, Number.NaN, view).xCenter).toBe(6200)
  })

  it('survives a zero-width window', () => {
    expect(zoomAt(2, 2, 6200, { min: 6200, max: 6200 })).toEqual({ xZoom: 4, xCenter: 6200 })
  })
})

describe('yExtentInWindow', () => {
  /** The segments the frame clips still have to be measured, or the curve
   *  leaves the top of the chart the moment it crosses the edge. */
  it('includes one neighbouring sample on each side of the window', () => {
    const extent = yExtentInWindow(lines, prices, { min: 6190, max: 6210 })
    // The samples at 6100 and 6300 are the neighbours: -400 … 400, padded.
    expect(extent!.min).toBeLessThan(-400)
    expect(extent!.min).toBeGreaterThan(-800)
    expect(extent!.max).toBeGreaterThan(400)
    expect(extent!.max).toBeLessThan(800)
  })

  it('measures only the visible curves, so a flat region opens up', () => {
    const narrow = yExtentInWindow(lines, prices, { min: 6190, max: 6210 })
    const whole = yExtentInWindow(lines, prices, { min: 6000, max: 6400 })
    expect(narrow!.max - narrow!.min).toBeLessThan(whole!.max - whole!.min)
  })

  it('pads by the same 4% rule as the whole-range extent', () => {
    const extent = yExtentInWindow(lines, prices, { min: 6000, max: 6400 })
    expect(extent).toEqual({ min: -800 - 64, max: 800 + 64 })
  })

  it('ignores non-finite P&L samples', () => {
    const ragged: RiskLine[] = [
      { ...lines[0]!, pnl: [Number.NaN, -200, 0, 200, Number.NaN] },
    ]
    expect(yExtentInWindow(ragged, prices, { min: 6000, max: 6400 })).toEqual({
      min: -200 - 16,
      max: 200 + 16,
    })
  })

  it('falls back to the bracketing pair when the window fits between samples', () => {
    const extent = yExtentInWindow(lines, prices, { min: 6210, max: 6290 })
    expect(extent!.min).toBeLessThan(0)
    expect(extent!.max).toBeGreaterThan(400)
  })

  it('is null when there is nothing finite to measure', () => {
    expect(yExtentInWindow([], prices, { min: 6000, max: 6400 })).toBeNull()
    expect(yExtentInWindow(lines, [], { min: 6000, max: 6400 })).toBeNull()
  })
})
