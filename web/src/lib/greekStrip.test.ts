import { describe, expect, it } from 'vitest'
import type { RiskGraphResponse, RiskGreeks, RiskLine } from '../api/types'
import {
  barRect,
  buildCursorOverlay,
  buildStrip,
  columnPitch,
  cursorSeriesId,
  DEFAULT_STRIP_COLUMNS,
  BAR_FRACTION,
  EXTENT_PAD,
  LABEL_FRACTION,
  labelSeriesId,
  sampleColumns,
  STRIP_FLOOR,
  STRIP_ROW_GAP,
  STRIP_ROW_HEIGHT,
  STRIP_ROWS,
  stripColumns,
  stripHeight,
  stripValues,
  symmetricExtent,
} from './greekStrip'
import { chartPalette } from './palette'

const dark = chartPalette('dark')
const light = chartPalette('light')

const prices = [6000, 6100, 6200, 6300, 6400]

const lines: RiskLine[] = [
  { date: '2026-08-31', kind: 't0', key: 't0', label: 'T+0', pnl: [-100, -20, 0, 40, 100] },
  { date: '2027-02-19', kind: 'expiry', key: 'expiry', label: 'Exp', pnl: [-200, -80, 0, 90, 200] },
]

const t0Greeks: RiskGreeks = {
  key: 't0',
  delta: [-30, -10, 5, 20, 40],
  gamma: [-2, -1, 1, 2, 3],
  theta: [-15, -8, -5, 2, 10],
  vega: [-8, -4, 3, 8, 12],
  weighted_vega: [-5, -2, 2, 6, 9],
  weighted_vega_reference: 'front_expiry (2026-10-16)',
}

const data: Pick<RiskGraphResponse, 'prices' | 'lines' | 't0_greeks'> = {
  prices,
  lines,
  t0_greeks: t0Greeks,
}

describe('sampleColumns', () => {
  it('spreads indices evenly across the whole grid with no window', () => {
    expect(sampleColumns(prices, null, 3)).toEqual([0, 2, 4])
  })

  it('returns every in-window index, de-duplicated, when fewer than count', () => {
    expect(sampleColumns(prices, null, 20)).toEqual([0, 1, 2, 3, 4])
  })

  it('is strictly increasing', () => {
    const out = sampleColumns(prices, null, 4)
    for (let i = 1; i < out.length; i += 1) {
      expect(out[i]!).toBeGreaterThan(out[i - 1]!)
    }
  })

  it('is empty when there is no grid', () => {
    expect(sampleColumns([], null)).toEqual([])
  })

  it('is empty when the window sits entirely off the data', () => {
    expect(sampleColumns(prices, { min: 9000, max: 9400 }, 5)).toEqual([])
  })

  it('collapses to a single index when the window is narrower than the grid step', () => {
    expect(sampleColumns(prices, { min: 6195, max: 6205 }, 5)).toEqual([2])
  })

  it('honours the default column count', () => {
    expect(sampleColumns(prices, null)).toHaveLength(Math.min(DEFAULT_STRIP_COLUMNS, prices.length))
  })
})

describe('columnPitch', () => {
  it('matches the grid step with no window', () => {
    expect(columnPitch(prices, null, 20)).toBe(100)
  })

  it('falls back to the grid step when the sample collapses to one column', () => {
    expect(columnPitch(prices, { min: 6195, max: 6205 }, 5)).toBe(100)
  })

  it('is 1 for a single-price grid', () => {
    expect(columnPitch([6200], null)).toBe(1)
  })

  it('is never zero or non-finite', () => {
    expect(columnPitch([], null)).toBe(1)
    expect(Number.isFinite(columnPitch(prices, { min: 9000, max: 9400 }))).toBe(true)
  })
})

describe('stripColumns', () => {
  it('reaches half a pitch past the end samples when there is no window', () => {
    const cols = stripColumns(prices, null, 20)
    expect(cols).toHaveLength(prices.length)
    expect(cols[0]!.from).toBeLessThan(prices[0]!)
    expect(cols[cols.length - 1]!.to).toBeGreaterThan(prices[prices.length - 1]!)
  })

  /** Half the *sampled* pitch is not always enough to reach the window bound:
   *  `sampleColumns` only keeps in-window grid points, so the first sample can
   *  sit a whole grid step above `window.min`. A bound that falls strictly
   *  between two grid points is the case that used to leave bare plot at the
   *  very edge of the row. */
  it('covers a window whose bounds fall between grid points', () => {
    const window = { min: 6030, max: 6370 }
    const cols = stripColumns(prices, window, 20)
    expect(cols.map((c) => c.price)).toEqual([6100, 6200, 6300])
    expect(cols[0]!.from).toBeLessThanOrEqual(window.min)
    expect(cols[cols.length - 1]!.to).toBeGreaterThanOrEqual(window.max)
    // Still one contiguous partition after the end columns are widened.
    for (let k = 0; k + 1 < cols.length; k += 1) expect(cols[k]!.to).toBe(cols[k + 1]!.from)
  })

  it('covers the window at a coarse sample count and a narrow window', () => {
    const grid = Array.from({ length: 121 }, (_, i) => 4800 + i * 20)
    const window = { min: 6003.7, max: 6497.3 }
    const cols = stripColumns(grid, window, 20)
    expect(cols[0]!.from).toBeLessThanOrEqual(window.min)
    expect(cols[cols.length - 1]!.to).toBeGreaterThanOrEqual(window.max)
  })

  /** The other half of the rule: a column may only be stretched to the window
   *  bound where the grid actually continues past it. Zoomed out (or panned
   *  off the fetched tile) the end sample *is* where the greeks stop, and
   *  reaching to the bound would paint a bar over a region that has none. */
  it('does not reach past the data when the window is wider than the grid', () => {
    const window = { min: 5000, max: 7400 }
    const cols = stripColumns(prices, window, 20)
    expect(cols).toHaveLength(prices.length)
    expect(cols[0]!.from).toBe(5950)
    expect(cols[cols.length - 1]!.to).toBe(6450)
  })

  it('widens only the end that has data beyond it', () => {
    // Window open to the left of the grid, cutting it on the right.
    const cols = stripColumns(prices, { min: 5000, max: 6230 }, 20)
    expect(cols.map((c) => c.price)).toEqual([6000, 6100, 6200])
    expect(cols[0]!.from).toBe(5950) // grid starts here: no widening
    expect(cols[cols.length - 1]!.to).toBe(6250) // grid continues: out to 6230+
  })

  /** The whole point of the helper: the row paints as one histogram, so a
   *  column's right edge must be the *same float* as its neighbour's left
   *  edge — not merely close to it, which is what leaves a sub-pixel gap or
   *  a double-painted overlap once it reaches the canvas. */
  it('hands neighbouring columns the identical shared edge', () => {
    for (const count of [3, 4, 7, 20]) {
      const cols = stripColumns(prices, null, count)
      for (let k = 0; k + 1 < cols.length; k += 1) {
        expect(cols[k]!.to).toBe(cols[k + 1]!.from)
      }
    }
  })

  it('keeps every sample strictly inside the column that reads it', () => {
    for (const col of stripColumns(prices, null, 4)) {
      expect(col.from).toBeLessThan(col.price)
      expect(col.to).toBeGreaterThan(col.price)
      expect(col.price).toBe(prices[col.index])
    }
  })

  /** `sampleColumns` rounds indices, so the sampled pitch varies by up to one
   *  grid step: [6000…6500] at count 4 samples 6000, 6200, 6300, 6500. The
   *  columns must tile that unevenness rather than smooth it away. */
  it('tiles an uneven pitch at the midpoints between samples', () => {
    const grid = [6000, 6100, 6200, 6300, 6400, 6500]
    const cols = stripColumns(grid, null, 4)
    expect(cols.map((c) => c.price)).toEqual([6000, 6200, 6300, 6500])
    expect(cols.map((c) => c.from)).toEqual([5900, 6100, 6250, 6400])
    expect(cols.map((c) => c.to)).toEqual([6100, 6250, 6400, 6600])
    expect(cols.map((c) => c.to - c.from)).toEqual([200, 150, 150, 200])
  })

  it('gives every column the same width, centred on its price, at a uniform pitch', () => {
    const cols = stripColumns(prices, null, 20)
    for (const col of cols) {
      expect(col.to - col.from).toBeCloseTo(100, 9)
      expect((col.from + col.to) / 2).toBeCloseTo(col.price, 9)
    }
  })

  it('falls back to the column pitch for a lone column', () => {
    const cols = stripColumns(prices, { min: 6195, max: 6205 }, 5)
    const half = columnPitch(prices, { min: 6195, max: 6205 }, 5) / 2
    expect(cols).toEqual([{ index: 2, price: 6200, from: 6200 - half, to: 6200 + half }])
  })

  it('is empty for an empty grid and for a window off the data', () => {
    expect(stripColumns([], null)).toEqual([])
    expect(stripColumns(prices, { min: 9000, max: 9400 }, 5)).toEqual([])
  })

  it('drops a non-finite price rather than emitting a non-finite edge', () => {
    const holed = [6000, Number.NaN, 6200, 6300]
    const cols = stripColumns(holed, null, 20)
    expect(cols.map((c) => c.price)).toEqual([6000, 6200, 6300])
    for (const col of cols) {
      expect(Number.isFinite(col.from)).toBe(true)
      expect(Number.isFinite(col.to)).toBe(true)
    }
  })
})

describe('barRect', () => {
  /** Two neighbours are handed the same edge number, so after snapping they
   *  land on the same device pixel: the rects abut exactly — no half-covered
   *  pixel column (antialiased to a pale seam) and no overlap (which would
   *  double-paint a 55 %-opacity negative bar into a dark seam). */
  it('makes two columns sharing an edge touch exactly, at any dpr', () => {
    for (const dpr of [1, 2, 1.5]) {
      const a = barRect(100.3, 131.87, 40, 20, dpr)
      const b = barRect(131.87, 163.4, 40, 30, dpr)
      expect(a.x + a.width).toBe(b.x)
    }
  })

  it('snaps the horizontal edges to whole device pixels', () => {
    for (const dpr of [1, 2]) {
      const rect = barRect(100.4, 131.87, 40, 20, dpr)
      expect(rect.x * dpr).toBe(Math.round(rect.x * dpr))
      expect((rect.x + rect.width) * dpr).toBe(Math.round((rect.x + rect.width) * dpr))
    }
  })

  it('grows a positive bar up from zero and a negative one down from it', () => {
    // Pixel y grows downward, so "above zero" is the smaller number.
    const up = barRect(10, 20, 40, 25)
    expect(up.y + up.height).toBe(40)
    const down = barRect(10, 20, 40, 55)
    expect(down.y).toBe(40)
    expect(down.height).toBe(15)
  })

  it('paints nothing for a value sitting exactly on zero', () => {
    expect(barRect(10, 20, 40, 40).height).toBe(0)
  })

  it('never lets a column collapse below one device pixel', () => {
    expect(barRect(100.1, 100.2, 40, 20, 1).width).toBe(1)
    expect(barRect(100.1, 100.2, 40, 20, 2).width).toBe(0.5)
  })

  it('leaves the height unrounded — a bar is only a few pixels tall', () => {
    // 1 px of vertical quantisation would be >10% of the tallest bar, and
    // vertical neighbours share no edge, so there is no seam to buy with it.
    expect(barRect(10, 20, 40, 32.4, 2).height).toBeCloseTo(7.6, 9)
  })
})

describe('symmetricExtent', () => {
  const indices = [0, 1, 2, 3, 4]

  it('pads the max absolute value by EXTENT_PAD', () => {
    const extent = symmetricExtent([-5, 0, 8, 3, -2], indices)
    expect(extent.max).toBeCloseTo(8 * EXTENT_PAD, 6)
    expect(extent.min).toBeCloseTo(-8 * EXTENT_PAD, 6)
  })

  it('holds the tallest bar at BAR_FRACTION of the half-row, below the label band', () => {
    // The label band starts at LABEL_FRACTION of the half-row; a bar that
    // reached into it would sit under its own (or a neighbour's) number.
    const half = STRIP_ROW_HEIGHT / 2
    for (const maxAbs of [8, 30, 250]) {
      const extent = symmetricExtent([maxAbs], [0], 0.001)
      const tipPx = half * (maxAbs / extent.max)
      expect(tipPx / half).toBeCloseTo(BAR_FRACTION, 6)
      expect(BAR_FRACTION).toBeLessThan(LABEL_FRACTION)
    }
  })

  it('floors at ±1 by default', () => {
    expect(symmetricExtent([0.1, -0.2], [0, 1])).toEqual({ min: -1, max: 1 })
  })

  it('skips non-finite values', () => {
    const extent = symmetricExtent([Number.NaN, 5, Number.POSITIVE_INFINITY, -3], [0, 1, 2, 3])
    expect(extent.max).toBeCloseTo(5 * EXTENT_PAD, 6)
  })

  it('returns the floor when nothing finite is in range', () => {
    expect(symmetricExtent([Number.NaN, Number.NaN], [0, 1], 2)).toEqual({ min: -2, max: 2 })
  })

  it('never returns min === max', () => {
    const extent = symmetricExtent([0, 0, 0], [0, 1, 2])
    expect(extent.min).not.toBe(extent.max)
  })
})

describe('stripValues', () => {
  it('maps the matched T+0 lines pnl and every greek array', () => {
    const values = stripValues(data)
    expect(values).toEqual({
      pnl: lines[0]!.pnl,
      delta: t0Greeks.delta,
      gamma: t0Greeks.gamma,
      theta: t0Greeks.theta,
      vega: t0Greeks.vega,
      wvega: t0Greeks.weighted_vega,
    })
  })

  it('is null when t0_greeks is null', () => {
    expect(stripValues({ lines, t0_greeks: null })).toBeNull()
  })

  it('is null when no line matches the greeks key', () => {
    const noMatch = { lines, t0_greeks: { ...t0Greeks, key: 't+12' } }
    expect(stripValues(noMatch)).toBeNull()
  })

  it('ignores which lines are hidden — it is handed the full line list either way', () => {
    // stripValues has no concept of "hidden"; the caller decides what to pass.
    // Passing every line vs. just the t0 line must give the same result as
    // long as the t0 line is present.
    const onlyT0 = stripValues({ lines: [lines[0]!], t0_greeks: t0Greeks })
    const both = stripValues({ lines, t0_greeks: t0Greeks })
    expect(onlyT0).toEqual(both)
  })
})

describe('STRIP_ROWS', () => {
  it('has the six pinned keys in order', () => {
    expect(STRIP_ROWS.map((r) => r.key)).toEqual(['pnl', 'delta', 'gamma', 'theta', 'vega', 'wvega'])
  })

  it('labels each row', () => {
    expect(STRIP_ROWS.map((r) => r.label)).toEqual(['P&L', 'Δ', 'Γ', 'Θ', 'V', 'WV'])
  })

  it('formats P&L with fmtPnl(v, 0)', () => {
    const pnl = STRIP_ROWS.find((r) => r.key === 'pnl')!
    expect(pnl.format(1234.5)).toBe('+$1,235')
    expect(pnl.format(-1234.5)).toBe('-$1,235')
  })

  it('formats vega and weighted vega alike, via fmtGreek', () => {
    const vega = STRIP_ROWS.find((r) => r.key === 'vega')!
    const wvega = STRIP_ROWS.find((r) => r.key === 'wvega')!
    expect(vega.format(12.4)).toBe(wvega.format(12.4))
  })

  it('colours P&L bars by sign, at full opacity', () => {
    const pnl = STRIP_ROWS.find((r) => r.key === 'pnl')!
    expect(pnl.barStyle(dark, 5)).toEqual({ color: dark.positive, opacity: 1 })
    expect(pnl.barStyle(dark, -5)).toEqual({ color: dark.negative, opacity: 1 })
  })

  it('keeps a greek rows own hue and drops to 55% opacity when negative, in both themes', () => {
    const delta = STRIP_ROWS.find((r) => r.key === 'delta')!
    expect(delta.barStyle(dark, 5)).toEqual({ color: dark.delta, opacity: 1 })
    expect(delta.barStyle(dark, -5)).toEqual({ color: dark.delta, opacity: 0.55 })
    expect(delta.barStyle(light, -5)).toEqual({ color: light.delta, opacity: 0.55 })
  })

  /** Gamma is dollars of delta per index point — around 0.1 for a book this
   *  size, two orders of magnitude under the five dollar-scale rows. On the
   *  default ±1 floor every gamma bar would be a tenth of the row and the
   *  row would read as a flat line, so only that row carries a floor. */
  it('gives gamma, and only gamma, a floor small enough to draw against', () => {
    const floors = new Map(STRIP_ROWS.map((r) => [r.key, r.extentFloor]))
    expect(floors.get('gamma')).toBe(0.01)
    for (const key of ['pnl', 'delta', 'theta', 'vega', 'wvega'] as const) {
      expect(floors.get(key)).toBeUndefined()
    }
  })
})

describe('stripHeight', () => {
  it('is 222 for the pinned 6-row strip', () => {
    expect(stripHeight()).toBe(222)
    expect(stripHeight()).toBe(
      STRIP_ROWS.length * STRIP_ROW_HEIGHT + (STRIP_ROWS.length - 1) * STRIP_ROW_GAP + 26 + STRIP_FLOOR,
    )
  })
})

describe('buildCursorOverlay', () => {
  it('returns all six cursor-* series ids', () => {
    const overlay = buildCursorOverlay({ data, index: 2, window: null, palette: dark })
    const ids = (overlay.series as { id: string }[]).map((s) => s.id)
    expect(ids).toEqual(STRIP_ROWS.map((r) => cursorSeriesId(r.key)))
  })

  it('reads the value at the exact index, not the nearest sampled bar', () => {
    // With only 2 columns sampled, index 3 (price 6300) is not one of the
    // sampled bars — the overlay must still read it directly. The point
    // itself sits on the row's zero line; the value goes into the label.
    const overlay = buildCursorOverlay({ data, index: 3, window: null, palette: dark, columns: 2 })
    const pnlSeries = (
      overlay.series as { id: string; data: [number, number][]; label: { formatter: () => string } }[]
    ).find((s) => s.id === 'cursor-pnl')!
    expect(pnlSeries.data).toEqual([[6300, 0]])
    expect(pnlSeries.label.formatter()).toBe(STRIP_ROWS[0]!.format(40))
  })

  it('centers a one-column-pitch-wide markArea band on the cursor price', () => {
    const overlay = buildCursorOverlay({ data, index: 1, window: null, palette: dark })
    const pitch = columnPitch(prices, null)
    const deltaSeries = (
      overlay.series as {
        id: string
        markArea: { data: [{ xAxis: number }, { xAxis: number }][] }
      }[]
    ).find((s) => s.id === 'cursor-delta')!
    const [[from, to]] = deltaSeries.markArea.data
    expect(to.xAxis - from.xAxis).toBeCloseTo(pitch, 6)
    expect((from.xAxis + to.xAxis) / 2).toBeCloseTo(prices[1]!, 6)
  })

  it('formats the label from the value at the cursor index', () => {
    const overlay = buildCursorOverlay({ data, index: 4, window: null, palette: dark })
    const wvega = (
      overlay.series as { id: string; label: { formatter: () => string } }[]
    ).find((s) => s.id === 'cursor-wvega')!
    expect(wvega.label.formatter()).toBe(STRIP_ROWS.find((r) => r.key === 'wvega')!.format(9))
  })

  it('clears every series (empty data, empty markArea) when index is null', () => {
    const overlay = buildCursorOverlay({ data, index: null, window: null, palette: dark })
    for (const series of overlay.series as {
      data: unknown[]
      markArea: { data: unknown[] }
    }[]) {
      expect(series.data).toEqual([])
      expect(series.markArea.data).toEqual([])
    }
  })

  it('clears every series when there is nothing to draw at all', () => {
    const overlay = buildCursorOverlay({
      data: { ...data, t0_greeks: null },
      index: 2,
      window: null,
      palette: dark,
    })
    for (const series of overlay.series as { data: unknown[] }[]) {
      expect(series.data).toEqual([])
    }
  })

  it('disables clipping so a cursor value outside the rows sampled extent still draws', () => {
    // Scatter clips by default, dropping the point AND its label together.
    // The cursor sits at the exact grid index, which the row's sampled,
    // padded extent does not always cover (a spike between sampled columns).
    const overlay = buildCursorOverlay({ data, index: 2, window: null, palette: dark })
    for (const series of overlay.series as { clip?: boolean }[]) {
      expect(series.clip).toBe(false)
    }
  })
})

/** The shape `buildStrip` gives a row's `custom` series, as the test reads
 *  it: the option types describe `renderItem` far more loosely than the one
 *  return we actually produce. */
interface FakeRenderApi {
  coord: (point: number[]) => number[]
  getDevicePixelRatio: () => number
}
interface FakeRect {
  type: string
  shape: { x: number; y: number; width: number; height: number }
  style: { fill: string; opacity: number }
  emphasisDisabled?: boolean
}
interface RowSeries {
  type: string
  name: string
  z?: number
  clip?: boolean
  silent?: boolean
  markLine?: unknown
  data: { value: [number, number] }[]
  renderItem: (params: { dataIndex: number }, api: FakeRenderApi) => FakeRect | undefined
}

const stripInput = { window: null, palette: dark, left: 68, right: 20, xMin: 6000, xMax: 6400 }

function rowSeries(strip: ReturnType<typeof buildStrip>, key: string): RowSeries {
  return strip.series.find((s) => (s as { name?: string }).name === `strip-${key}`) as unknown as RowSeries
}

/** A stand-in cartesian grid: price maps 1:1 to x, the row's zero line sits
 *  at pixel y `ZERO_PX`, and value grows upward (i.e. toward smaller y), as a
 *  real grid does. */
const ZERO_PX = 100
const renderApi: FakeRenderApi = {
  coord: ([x, y]) => [x!, ZERO_PX - y!],
  getDevicePixelRatio: () => 1,
}

describe('buildStrip', () => {
  it('draws each row as a clipped custom series that renders its own rects', () => {
    // A `bar` series sizes every bar to the *minimum* gap between data points
    // on a value axis, so the wider sampled columns would show gaps; the row
    // draws its own rects instead. `clip: true` trims the end columns — which
    // `stripColumns` deliberately runs past the window bound wherever there
    // are greeks out there — to the grid rect, so the histogram runs edge to
    // edge rather than stopping short.
    const strip = buildStrip({ data, ...stripInput })
    for (const row of STRIP_ROWS) {
      const series = rowSeries(strip, row.key)
      expect(series.type).toBe('custom')
      expect(series.clip).toBe(true)
      expect(series.silent).toBe(true)
      expect(series.z).toBe(2)
      expect(typeof series.renderItem).toBe('function')
    }
  })

  it('renders each column edge to edge from barRect, in the rows own style', () => {
    const strip = buildStrip({ data, ...stripInput })
    const delta = rowSeries(strip, 'delta')
    // Column 1 of the uniform 100-wide grid: price 6100, delta -10, so the
    // slice runs 6050…6150 and the bar hangs 10 units below zero.
    expect(delta.data[1]).toEqual({ value: [6100, -10] })
    const rect = delta.renderItem({ dataIndex: 1 }, renderApi)!
    expect(rect.type).toBe('rect')
    expect(rect.shape).toEqual(barRect(6050, 6150, ZERO_PX, ZERO_PX + 10, 1))
    expect(rect.style).toEqual({ fill: dark.delta, opacity: 0.55 })
    // The strip axes still dispatch `highlight`; the bars must ignore it.
    expect(rect.emphasisDisabled).toBe(true)
  })

  it('draws neighbouring columns with no gap and no overlap between them', () => {
    const strip = buildStrip({ data, ...stripInput })
    const theta = rowSeries(strip, 'theta')
    const rects = theta.data.map((_, i) => theta.renderItem({ dataIndex: i }, renderApi)!)
    for (let i = 1; i < rects.length; i += 1) {
      expect(rects[i - 1]!.shape.x + rects[i - 1]!.shape.width).toBe(rects[i]!.shape.x)
    }
  })

  it('renders nothing rather than throwing for a data index it has no bar for', () => {
    const strip = buildStrip({ data, ...stripInput })
    expect(rowSeries(strip, 'pnl').renderItem({ dataIndex: 99 }, renderApi)).toBeUndefined()
  })

  /** The renderItem closure looks its geometry up by `dataIndex`, so a blank
   *  greek has to drop the same column out of both the payload and the bar
   *  list or every bar after the hole would be drawn in the wrong slice. */
  it('keeps the drawn columns and the payload in step across a blank greek', () => {
    const holed: RiskGreeks = { ...t0Greeks, vega: [-8, Number.NaN, 3, 8, 12] }
    const strip = buildStrip({ data: { prices, lines, t0_greeks: holed }, ...stripInput })
    const vega = rowSeries(strip, 'vega')
    expect(vega.data.map((d) => d.value)).toEqual([
      [6000, -8],
      [6200, 3],
      [6300, 8],
      [6400, 12],
    ])
    // Index 1 is now price 6200, whose slice is still 6150…6250 — the blank
    // column leaves a hole in the row rather than shifting its neighbours.
    const rect = vega.renderItem({ dataIndex: 1 }, renderApi)!
    expect(rect.shape).toEqual(barRect(6150, 6250, ZERO_PX, ZERO_PX - 3, 1))
  })

  /** `MarkLineModel` carries its own z (5), so hosting the zero line on the
   *  label scatter instead of the row's custom series is a visual no-op — and
   *  a scatter is a proven markLine host. */
  it('hangs each rows zero line off its label series, not its bars', () => {
    const strip = buildStrip({ data, ...stripInput })
    for (const row of STRIP_ROWS) {
      const labels = strip.series.find((s) => (s as { id?: string }).id === labelSeriesId(row.key)) as {
        markLine?: { data?: { yAxis: number }[] }
      }
      expect(labels.markLine?.data).toEqual([{ yAxis: 0 }])
      expect(rowSeries(strip, row.key).markLine).toBeUndefined()
    }
  })

  it('disables clipping on every cursor-* placeholder series', () => {
    const strip = buildStrip({ data, window: null, palette: dark, left: 68, right: 20, xMin: 6000, xMax: 6400 })
    const cursorSeries = strip.series.filter((s) => (s as { id?: string }).id?.startsWith('cursor-'))
    expect(cursorSeries).toHaveLength(STRIP_ROWS.length)
    for (const series of cursorSeries as { clip?: boolean }[]) {
      expect(series.clip).toBe(false)
    }
  })

  /** ~20 columns over ~600 px is a ~30 px pitch, and a formatted dollar
   *  label is wider than that — without `hideOverlap` every row renders as a
   *  smear of half-overlapping numbers. It is opt-in per series, so it must
   *  be on the bars and off the `cursor-*` boxes, which are the exact read
   *  and must never be thinned away. */
  it('thins the value labels on overlap and never the cursor boxes', () => {
    const strip = buildStrip({ data, window: null, palette: dark, left: 68, right: 20, xMin: 6000, xMax: 6400 })
    const labels = strip.series.filter((s) => (s as { id?: string }).id?.startsWith('strip-label-'))
    expect(labels).toHaveLength(STRIP_ROWS.length)
    for (const series of labels as { labelLayout?: { hideOverlap?: boolean } }[]) {
      expect(series.labelLayout?.hideOverlap).toBe(true)
    }
    // The rows themselves carry no label plumbing at all: a `renderItem` rect
    // has none, and the numbers are the `strip-label-*` scatter's job.
    for (const series of strip.series.filter((s) => s.type === 'custom') as { label?: unknown }[]) {
      expect(series.label).toBeUndefined()
    }
    const cursorSeries = strip.series.filter((s) => (s as { id?: string }).id?.startsWith('cursor-'))
    for (const series of cursorSeries as { labelLayout?: unknown }[]) {
      expect(series.labelLayout).toBeUndefined()
    }
  })

  it('parks every value label in the row edge band on the value own side of zero', () => {
    // delta: [-30, -10, 5, 20, 40] — each label sits LABEL_FRACTION of the
    // half-row out, above zero for a positive value and below for a negative
    // one, and carries the value itself for the formatter.
    const strip = buildStrip({ data, window: null, palette: dark, left: 68, right: 20, xMin: 6000, xMax: 6400 })
    const labels = strip.series.find((s) => (s as { id?: string }).id === labelSeriesId('delta'))!
    const bound = 40 * EXTENT_PAD
    expect(labels.data).toEqual([
      [6000, -bound * LABEL_FRACTION, -30],
      [6100, -bound * LABEL_FRACTION, -10],
      [6200, bound * LABEL_FRACTION, 5],
      [6300, bound * LABEL_FRACTION, 20],
      [6400, bound * LABEL_FRACTION, 40],
    ])
    const formatter = (labels.label as { formatter: (p: { value: unknown }) => string }).formatter
    expect(formatter({ value: [6000, -1, -30] })).toBe(STRIP_ROWS[1]!.format(-30))
  })

  /** A real book's gamma runs around 0.1 — under the ±1 floor the other five
   *  rows use, which would flatten every gamma bar to a tenth of its row.
   *  The gamma axis has to follow the gamma data at that scale; the delta
   *  row, at the same magnitude, is correctly still held open by the floor
   *  (dollar delta that small really is flat). */
  it('scales the gamma row to sub-unit gamma instead of pinning it to the ±1 floor', () => {
    const small: RiskGreeks = { ...t0Greeks, gamma: [-0.05, -0.02, 0.04, 0.1, 0.13] }
    const strip = buildStrip({
      data: { prices, lines, t0_greeks: small },
      window: null,
      palette: dark,
      left: 68,
      right: 20,
      xMin: 6000,
      xMax: 6400,
    })
    const gammaRow = STRIP_ROWS.findIndex((r) => r.key === 'gamma')
    const axis = strip.yAxes[gammaRow] as { min: number; max: number }
    expect(axis.max).toBeCloseTo(0.13 * EXTENT_PAD, 6)
    expect(axis.min).toBeCloseTo(-0.13 * EXTENT_PAD, 6)
  })

  it('still holds a flat gamma row open at its own floor', () => {
    const flat: RiskGreeks = { ...t0Greeks, gamma: [0, 0, 0, 0, 0] }
    const strip = buildStrip({
      data: { prices, lines, t0_greeks: flat },
      window: null,
      palette: dark,
      left: 68,
      right: 20,
      xMin: 6000,
      xMax: 6400,
    })
    const gammaRow = STRIP_ROWS.findIndex((r) => r.key === 'gamma')
    expect(strip.yAxes[gammaRow]).toMatchObject({ min: -0.01, max: 0.01 })
  })

  /** The other five rows keep the pinned ±1 default. */
  it('keeps the ±1 floor on the dollar-scale rows', () => {
    const tiny: RiskGreeks = { ...t0Greeks, delta: [0, 0, 0, 0, 0] }
    const strip = buildStrip({
      data: { prices, lines, t0_greeks: tiny },
      window: null,
      palette: dark,
      left: 68,
      right: 20,
      xMin: 6000,
      xMax: 6400,
    })
    const deltaRow = STRIP_ROWS.findIndex((r) => r.key === 'delta')
    expect(strip.yAxes[deltaRow]).toMatchObject({ min: -1, max: 1 })
  })
})
