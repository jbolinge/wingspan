import { describe, expect, it } from 'vitest'
import type { RiskLine } from '../api/types'
import {
  DEFAULT_ZOOM,
  MAX_POINTS,
  MAX_ZOOM,
  MAX_ZOOM_LOG2,
  MIN_ZOOM,
  MIN_ZOOM_LOG2,
  ZOOM_STEP_LOG2,
  clampZoom,
  coverageFor,
  formatZoom,
  priceRange,
  snapZoom,
  xWindow,
  yExtentInWindow,
  zoomAt,
  zoomFromSlider,
} from './viewport'

const prices = [6000, 6100, 6200, 6300, 6400]
/** The base range those prices span: 400 wide, midpoint 6200. */
const base = { min: 6000, max: 6400 }
const BASE_POINTS = 241

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
  it('holds the zoom between 25% and 500%', () => {
    expect(clampZoom(0.1)).toBe(MIN_ZOOM)
    expect(clampZoom(0.25)).toBe(0.25)
    expect(clampZoom(2.5)).toBe(2.5)
    expect(clampZoom(1000)).toBe(MAX_ZOOM)
  })

  /** 0.2.6's slider ran to 16×; a view persisted by it must land on the new
   *  top end, not be thrown away or fed to the slider as an out-of-range
   *  thumb position. */
  it('clamps a zoom persisted by the 1×–16× scale', () => {
    expect(clampZoom(16)).toBe(MAX_ZOOM)
    expect(clampZoom(8)).toBe(MAX_ZOOM)
  })

  /** A NaN or an Infinity can only come from a corrupted persisted blob;
   *  showing the whole computed range is the honest answer to it. */
  it('reads a non-finite zoom as 100%', () => {
    expect(clampZoom(Number.NaN)).toBe(DEFAULT_ZOOM)
    expect(clampZoom(Number.POSITIVE_INFINITY)).toBe(DEFAULT_ZOOM)
  })
})

describe('snapZoom', () => {
  /** The range input rounds the thumb to its step whatever the store holds;
   *  snapping on the way in keeps the thumb, the readout and the store from
   *  telling the trader three different zooms after a wheel notch. */
  it('lands on the slider grid, exactly', () => {
    const snapped = snapZoom(1.44)
    expect(Math.log2(snapped) / ZOOM_STEP_LOG2).toBeCloseTo(
      Math.round(Math.log2(snapped) / ZOOM_STEP_LOG2),
      9,
    )
    expect(snapped).toBeCloseTo(1.4641, 4)
  })

  it('leaves the round zooms the slider itself writes untouched', () => {
    expect(snapZoom(1)).toBe(1)
    expect(snapZoom(4)).toBe(4)
  })

  /** 500% is not on the log2 grid (log2 5 = 2.3219, the grid stops at 2.30):
   *  both limits are snap candidates in their own right, so a wheel can reach
   *  them exactly and the readout can say "25%" and "500%" and mean it. */
  it('reaches both limits exactly', () => {
    expect(snapZoom(MIN_ZOOM)).toBe(MIN_ZOOM)
    expect(snapZoom(MAX_ZOOM)).toBe(MAX_ZOOM)
    expect(snapZoom(4.99)).toBe(MAX_ZOOM)
    expect(formatZoom(snapZoom(MIN_ZOOM))).toBe('25%')
    expect(formatZoom(snapZoom(MAX_ZOOM))).toBe('500%')
  })

  it('still prefers the grid a step below the top', () => {
    expect(snapZoom(4.9)).toBeCloseTo(2 ** 2.3, 6)
  })

  it('clamps like clampZoom does', () => {
    expect(snapZoom(0)).toBe(MIN_ZOOM)
    expect(snapZoom(Number.NaN)).toBe(DEFAULT_ZOOM)
    expect(snapZoom(1000)).toBe(MAX_ZOOM)
  })
})

describe('zoomFromSlider', () => {
  it('maps both ends of the track onto the limits', () => {
    expect(zoomFromSlider(MIN_ZOOM_LOG2)).toBe(MIN_ZOOM)
    expect(zoomFromSlider(MAX_ZOOM_LOG2)).toBe(MAX_ZOOM)
  })

  /** A range input steps from its *min*, so the thumb stops on the last grid
   *  point (2.30) and can never land on a max that is off the grid (2.3219).
   *  That last step therefore has to mean 500%, or the slider's right end
   *  would read 492% and MAX_ZOOM would be unreachable by mouse. */
  it('reads the last reachable step as 500%', () => {
    expect(zoomFromSlider(MAX_ZOOM_LOG2 - ZOOM_STEP_LOG2 / 4)).toBe(MAX_ZOOM)
    expect(zoomFromSlider(2.3)).toBe(MAX_ZOOM)
    // The step below is still its own zoom.
    expect(zoomFromSlider(2.25)).toBeCloseTo(2 ** 2.25, 9)
  })

  it('is 100% in the middle of the track, and for nonsense', () => {
    expect(zoomFromSlider(0)).toBe(DEFAULT_ZOOM)
    expect(zoomFromSlider(Number.NaN)).toBe(DEFAULT_ZOOM)
  })
})

describe('formatZoom', () => {
  it('prints whole percent, ONE-style', () => {
    expect(formatZoom(1)).toBe('100%')
    expect(formatZoom(2.5)).toBe('250%')
    expect(formatZoom(0.3715)).toBe('37%')
    expect(formatZoom(MIN_ZOOM)).toBe('25%')
    expect(formatZoom(MAX_ZOOM)).toBe('500%')
  })
})

describe('priceRange', () => {
  it('is the extent of the finite prices', () => {
    expect(priceRange(prices)).toEqual(base)
    expect(priceRange([6200, Number.NaN])).toBeNull()
    expect(priceRange([])).toBeNull()
  })
})

describe('xWindow', () => {
  it('is exactly the base range at 100% with no centre', () => {
    expect(xWindow(prices, 1, null)).toEqual(base)
  })

  it('widens to 4× the base range at 25% and narrows to a fifth at 500%', () => {
    expect(xWindow(prices, MIN_ZOOM, null)).toEqual({ min: 5400, max: 7000 })
    expect(xWindow(prices, MAX_ZOOM, null)).toEqual({ min: 6160, max: 6240 })
  })

  it('halves the width per doubling of the zoom, centred on the centre', () => {
    expect(xWindow(prices, 2, 6200)).toEqual({ min: 6100, max: 6300 })
    expect(xWindow(prices, 4, 6200)).toEqual({ min: 6150, max: 6250 })
  })

  /** The centre is the base range's midpoint, not spot: that is what makes
   *  100% with no centre the base range itself, byte for byte. */
  it('centres on the base midpoint when no centre has been chosen', () => {
    expect(xWindow([6000, 6500], 4, null)).toEqual({ min: 6187.5, max: 6312.5 })
    expect(xWindow(prices, 4, Number.NaN)).toEqual({ min: 6150, max: 6250 })
  })

  /** A view persisted by 0.2.6 can hold a zoom of 16; it draws at 500%. */
  it('clamps a persisted 16× to 500%', () => {
    expect(xWindow(prices, 16, 6200)).toEqual(xWindow(prices, MAX_ZOOM, 6200))
  })

  /** Prices below zero are not a thing, and the server rejects a range that
   *  reaches them — so the window slides right, keeping its width, rather
   *  than being squashed against the axis. */
  it('floors at zero by shifting, never by shrinking', () => {
    const window = xWindow([10, 410], MIN_ZOOM, null)
    expect(window).toEqual({ min: 0, max: 1600 })
    expect(xWindow(prices, 2, -99_000)).toEqual({ min: 0, max: 200 })
  })

  /** Panning past the computed range is the whole feature: nothing pulls the
   *  window back towards the prices the server happened to answer with. */
  it('has no upper clamp at all', () => {
    expect(xWindow(prices, 4, 99_000)).toEqual({ min: 98_950, max: 99_050 })
    expect(xWindow(prices, 1, 12_000)).toEqual({ min: 11_800, max: 12_200 })
  })

  it('is null without at least two finite prices', () => {
    expect(xWindow([], 2, null)).toBeNull()
    expect(xWindow([6200], 2, null)).toBeNull()
    expect(xWindow([6200, Number.NaN], 2, null)).toBeNull()
  })

  it('survives a degenerate range where every price is the same', () => {
    expect(xWindow([6200, 6200], 4, null)).toEqual({ min: 6200, max: 6200 })
  })
})

describe('zoomAt', () => {
  const view = { min: 6100, max: 6300 }

  it('keeps the anchor price at the same fraction of the window', () => {
    const next = zoomAt(2, 2, 6150, view)
    expect(next.xZoom).toBe(4)
    // 6150 sits a quarter in; after halving the width it must still be a
    // quarter in — window 6125…6225, centre 6175.
    const after = xWindow(prices, next.xZoom, next.xCenter)
    expect((6150 - after!.min) / (after!.max - after!.min)).toBeCloseTo(0.25, 10)
  })

  it('zooms out about the anchor as well', () => {
    const next = zoomAt(4, 0.5, 6150, { min: 6150, max: 6250 })
    expect(next.xZoom).toBe(2)
    expect(next.xCenter).toBeCloseTo(6250, 10)
  })

  it('clamps at both ends of the zoom range', () => {
    expect(zoomAt(MAX_ZOOM, 4, 6200, view).xZoom).toBe(MAX_ZOOM)
    expect(zoomAt(0.3, 0.01, 6200, view).xZoom).toBe(MIN_ZOOM)
  })

  it('ignores a nonsense factor or anchor and falls back to the centre', () => {
    expect(zoomAt(2, Number.NaN, 6200, view)).toEqual({ xZoom: 2, xCenter: 6200 })
    expect(zoomAt(2, 2, Number.NaN, view).xCenter).toBe(6200)
  })

  it('survives a zero-width window', () => {
    expect(zoomAt(2, 2, 6200, { min: 6200, max: 6200 })).toEqual({ xZoom: 4, xCenter: 6200 })
  })
})

describe('coverageFor', () => {
  it('is deterministic and contains the window with a margin', () => {
    const view = { min: 6110, max: 6310 }
    const coverage = coverageFor(view, base, BASE_POINTS)!
    expect(coverageFor(view, base, BASE_POINTS)).toEqual(coverage)
    expect(coverage.min).toBeLessThan(view.min)
    expect(coverage.max).toBeGreaterThan(view.max)
  })

  /** A drag fires per animation frame: the request must stay put until the
   *  window has actually left what was fetched, or every frame is a new
   *  query key and a fresh pricing grid. */
  it('is unchanged by a pan that stays inside it, and moves when one leaves', () => {
    const here = coverageFor({ min: 6110, max: 6310 }, base, BASE_POINTS)
    expect(coverageFor({ min: 6150, max: 6350 }, base, BASE_POINTS)).toEqual(here)
    expect(coverageFor({ min: 6900, max: 7100 }, base, BASE_POINTS)).not.toEqual(here)
  })

  /** Tiles are anchored at price 0 and sized by the zoom, so the coverage of
   *  a 4× window is one tile of 100 either side, not a fitted box. */
  it('halves the tile when the zoom doubles', () => {
    expect(coverageFor({ min: 6100, max: 6300 }, base, BASE_POINTS)).toMatchObject({
      min: 6000,
      max: 6400,
    })
    expect(coverageFor({ min: 6150, max: 6250 }, base, BASE_POINTS)).toMatchObject({
      min: 6100,
      max: 6300,
    })
  })

  /** `/risk-graph` rejects a range starting at or below zero. */
  it('floors the low end at 1 near zero', () => {
    const coverage = coverageFor({ min: 0, max: 80 }, base, BASE_POINTS)!
    expect(coverage.min).toBe(1)
    expect(coverage.max).toBeGreaterThan(80)
  })

  it('scales the point count with the coverage, at the base density', () => {
    const coverage = coverageFor({ min: 6100, max: 6300 }, base, BASE_POINTS)!
    // 6000…6400 is exactly the base width, so it gets the base point count.
    expect(coverage.points).toBe(BASE_POINTS)
    const wide = coverageFor({ min: 5400, max: 7000 }, base, BASE_POINTS)!
    expect(wide.points).toBeGreaterThan(BASE_POINTS)
  })

  it('caps the point count where the server does', () => {
    const zoomedOut = coverageFor({ min: 5400, max: 7000 }, base, 961)!
    expect(zoomedOut.points).toBe(MAX_POINTS)
  })

  it('is null for a degenerate window or base range', () => {
    expect(coverageFor({ min: 6200, max: 6200 }, base, BASE_POINTS)).toBeNull()
    expect(coverageFor({ min: 6100, max: 6300 }, { min: 6200, max: 6200 }, BASE_POINTS)).toBeNull()
    expect(coverageFor({ min: Number.NaN, max: 6300 }, base, BASE_POINTS)).toBeNull()
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
    const ragged: RiskLine[] = [{ ...lines[0]!, pnl: [Number.NaN, -200, 0, 200, Number.NaN] }]
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

  /** Panned clean off the fetched data there is nothing on screen to scale
   *  to: null, which the chart reads as "leave the axis to ECharts" rather
   *  than pinning it to the extent of one edge sample. */
  it('is null when the window sits entirely outside the data', () => {
    expect(yExtentInWindow(lines, prices, { min: 9000, max: 9500 })).toBeNull()
    expect(yExtentInWindow(lines, prices, { min: 100, max: 200 })).toBeNull()
  })

  it('is null when there is nothing finite to measure', () => {
    expect(yExtentInWindow([], prices, { min: 6000, max: 6400 })).toBeNull()
    expect(yExtentInWindow(lines, [], { min: 6000, max: 6400 })).toBeNull()
  })
})
