/**
 * The greek strip: six rows of contiguous bars (P&L, Δ, Γ, Θ, V, WV) drawn
 * under the risk graph's P&L plot, all T+0-only.
 *
 * Three ideas carry the whole module. First, the strip shares grid 0's price
 * axis pixel-for-pixel — the same `left`/`right` insets and the same x-axis
 * `min`/`max` as the P&L plot, plus `axisPointer.link` in the chart option —
 * so a bar always sits directly under the curve sample it describes and a
 * drag/zoom on either plot moves both together. Second, every row's y axis
 * is symmetric about zero (`symmetricExtent`), so the zero line is the row's
 * vertical midline and sign reads visually as up (positive) or down
 * (negative) rather than as a number you have to read to interpret.
 *
 * Third, a row is one *contiguous* histogram, not a picket fence: the bars
 * tile the price axis, so the eye reads the profile's slope — is the greek
 * rising or falling as spot moves — in a glance instead of comparing tip
 * heights across gaps. `stripColumns` cuts the axis at the midpoints between
 * neighbouring samples, and each row is a `custom` series that draws those
 * slices itself. A `bar` series cannot do this on a value axis: its band
 * width is the *minimum* gap between data points, and `sampleColumns` rounds
 * indices, so every bar would be sized to the narrowest column and the wider
 * ones would show gaps. Drawing the rects directly also lets `barRect` snap
 * the shared edges to whole device pixels — the only way two abutting rects
 * do not composite a pale antialiasing seam where they meet mid-pixel.
 */

import type {
  CustomSeriesOption,
  CustomSeriesRenderItemAPI,
  CustomSeriesRenderItemParams,
  CustomSeriesRenderItemReturn,
  EChartsOption,
  GridComponentOption,
  ScatterSeriesOption,
  TitleComponentOption,
  XAXisComponentOption,
  YAXisComponentOption,
} from 'echarts'
import type { RiskGraphResponse } from '../api/types'
import { fmtGreek, fmtPnl } from './format'
import type { ChartPalette } from './palette'
import type { PriceWindow } from './viewport'

export type StripRowKey = 'pnl' | 'delta' | 'gamma' | 'theta' | 'vega' | 'wvega'

export interface StripRow {
  key: StripRowKey
  /** Row title drawn in the left margin. */
  label: string
  /** Bar label and cursor-box text. */
  format: (value: number | null | undefined) => string
  /** Fill for one bar of this row. P&L colours by sign at full opacity;
   *  a greek row keeps its own hue and drops to 55 % opacity when negative. */
  barStyle: (palette: ChartPalette, value: number) => { color: string; opacity: number }
  /** Narrowest half-height this row's y axis may claim, in the row's own
   *  units — the `floor` handed to [`symmetricExtent`].
   *
   *  It exists to stop a row that is genuinely flat from being scaled up
   *  into pure noise, so it has to be small against what the row normally
   *  reads. The default 1 is right for the five dollar-scale rows; gamma is
   *  dollars of delta per index point, which for a book of this size runs
   *  around 0.1, so the same floor would pin every gamma bar to a tenth of
   *  the row and the row would render as a flat line. */
  extentFloor?: number
}

function greekBarStyle(hue: (palette: ChartPalette) => string) {
  return (palette: ChartPalette, value: number) => ({
    color: hue(palette),
    opacity: value < 0 ? 0.55 : 1,
  })
}

/** The six rows, top to bottom: P&L, Δ, Γ, Θ, V, WV. */
export const STRIP_ROWS: readonly StripRow[] = [
  {
    key: 'pnl',
    label: 'P&L',
    format: (v) => fmtPnl(v, 0),
    barStyle: (palette, v) => ({ color: v >= 0 ? palette.positive : palette.negative, opacity: 1 }),
  },
  {
    key: 'delta',
    label: 'Δ',
    format: (v) => fmtGreek('delta', v),
    barStyle: greekBarStyle((p) => p.delta),
  },
  {
    key: 'gamma',
    label: 'Γ',
    format: (v) => fmtGreek('gamma', v),
    barStyle: greekBarStyle((p) => p.gamma),
    // Two orders of magnitude below the dollar rows — see `extentFloor`.
    extentFloor: 0.01,
  },
  {
    key: 'theta',
    label: 'Θ',
    format: (v) => fmtGreek('theta', v),
    barStyle: greekBarStyle((p) => p.theta),
  },
  {
    key: 'vega',
    label: 'V',
    format: (v) => fmtGreek('vega', v),
    barStyle: greekBarStyle((p) => p.vega),
  },
  {
    key: 'wvega',
    label: 'WV',
    // Weighted vega is a vega, dollars per vol point of the reference expiry.
    format: (v) => fmtGreek('vega', v),
    barStyle: greekBarStyle((p) => p.wvega),
  },
]

/**
 * The six rows' values over the *whole* price grid.
 *
 * `null` — one flag for the whole strip — when there is nothing to draw:
 * `t0_greeks === null`, or no line whose `key` matches `t0_greeks.key`.
 * The strip deliberately ignores `hiddenLines`: it follows `t0_greeks`.
 */
export function stripValues(
  data: Pick<RiskGraphResponse, 'lines' | 't0_greeks'>,
): Record<StripRowKey, number[]> | null {
  const { lines, t0_greeks } = data
  if (!t0_greeks) return null
  const line = lines.find((l) => l.key === t0_greeks.key)
  if (!line) return null
  return {
    pnl: line.pnl,
    delta: t0_greeks.delta,
    gamma: t0_greeks.gamma,
    theta: t0_greeks.theta,
    vega: t0_greeks.vega,
    wvega: t0_greeks.weighted_vega,
  }
}

/** Default number of sampled bar columns across the visible window. */
export const DEFAULT_STRIP_COLUMNS = 20

/**
 * Indices into `prices`, evenly spaced across `window` (the whole grid when
 * `window` is null), at most `count` of them, strictly increasing and
 * de-duplicated. Empty when no grid point falls inside the window.
 */
export function sampleColumns(
  prices: readonly number[],
  window: PriceWindow | null,
  count: number = DEFAULT_STRIP_COLUMNS,
): number[] {
  if (prices.length === 0) return []
  let lo = 0
  let hi = prices.length - 1
  if (window) {
    lo = -1
    hi = -1
    for (let i = 0; i < prices.length; i += 1) {
      const price = prices[i]
      if (price === undefined || !Number.isFinite(price)) continue
      if (price >= window.min && price <= window.max) {
        if (lo < 0) lo = i
        hi = i
      }
    }
    if (lo < 0) return []
  }
  const span = hi - lo
  const n = Math.max(1, Math.min(count, span + 1))
  if (n === 1) return [lo]
  const out: number[] = []
  for (let k = 0; k < n; k += 1) {
    const idx = lo + Math.round((k * span) / (n - 1))
    const prev = out[out.length - 1]
    if (prev === undefined || idx > prev) out.push(idx)
  }
  return out
}

/**
 * Width of one sampled column in price units. Falls back to the grid step,
 * then to 1; never returns 0 or a non-finite number.
 */
export function columnPitch(
  prices: readonly number[],
  window: PriceWindow | null,
  count: number = DEFAULT_STRIP_COLUMNS,
): number {
  const indices = sampleColumns(prices, window, count)
  if (indices.length >= 2) {
    const firstIdx = indices[0]
    const lastIdx = indices[indices.length - 1]
    const first = firstIdx !== undefined ? prices[firstIdx] : undefined
    const last = lastIdx !== undefined ? prices[lastIdx] : undefined
    if (first !== undefined && last !== undefined && Number.isFinite(first) && Number.isFinite(last)) {
      const pitch = (last - first) / (indices.length - 1)
      if (Number.isFinite(pitch) && pitch > 0) return pitch
    }
  }
  const finite = prices.filter((p): p is number => Number.isFinite(p))
  if (finite.length >= 2) {
    const step = (finite[finite.length - 1]! - finite[0]!) / (finite.length - 1)
    if (Number.isFinite(step) && step > 0) return step
  }
  return 1
}

/**
 * Whether the grid carries a finite price beyond `index` in direction `dir`
 * (-1 left, +1 right) — i.e. the greeks genuinely continue past that sampled
 * column, which is the licence `stripColumns` needs before widening an end
 * column out to the window bound.
 */
function gridContinues(prices: readonly number[], index: number, dir: -1 | 1): boolean {
  for (let i = index + dir; i >= 0 && i < prices.length; i += dir) {
    const price = prices[i]
    if (price !== undefined && Number.isFinite(price)) return true
  }
  return false
}

export interface StripColumn {
  /** Index into `prices` of the grid sample this column describes. */
  index: number
  /** That sample's price — the value the column reads, and its midpoint up to
   *  the ±½ grid step of jitter `sampleColumns`' rounding leaves behind. */
  price: number
  /** The column's edges in price units. `to` of column k is `from` of column
   *  k+1 — the same number, produced once — which is what makes the row paint
   *  gap-free and seam-free. */
  from: number
  to: number
}

/**
 * The sampled columns of a strip row, as a partition of the price axis.
 *
 * Each edge is the midpoint between two neighbouring samples: the
 * nearest-sample partition is the only one that is gap-free, overlap-free and
 * still keeps every sample inside the column that reads it. The two end
 * columns reach half their own pitch past the first/last sample, and are then
 * pulled out to the window bound whenever the grid genuinely continues past
 * it, so the row paints edge to edge; the series clips them to the grid rect.
 *
 * That last step is not cosmetic padding. `sampleColumns` keeps only grid
 * points *inside* the window, so the first sample can sit up to a whole grid
 * step above `window.min` while the first column only overhangs by half the
 * *sampled* pitch — at the coarsest point count and a deep zoom the overhang
 * loses, and the row stops ~20 px short of the plot edge next to bars barely
 * twice that wide. It reads as a missing bar at exactly the edge the eye
 * checks first. The widening is conditional because the same reach would be a
 * lie the other way round: when the window runs past the fetched grid (zoom
 * 25 %, or panned off the tile) the first sample *is* where the data starts,
 * and stretching its column to `window.min` would paint hundreds of pixels of
 * greeks over a region that has none. `gridContinues` is the difference.
 *
 * The widths are deliberately *not* evened out. `sampleColumns` rounds
 * indices, so neighbouring columns routinely differ by one grid step — e.g.
 * `[6000…6500]` at `count: 4` samples `6000, 6200, 6300, 6500`, giving widths
 * `200, 150, 150, 200`. Uneven columns that tile the axis are honest; equal
 * columns that leave gaps between them are not.
 *
 * Samples whose price is not finite are dropped (`sampleColumns` only checks
 * finiteness when it has a window to compare against), so the result can be
 * shorter than the index list.
 */
export function stripColumns(
  prices: readonly number[],
  window: PriceWindow | null,
  count: number = DEFAULT_STRIP_COLUMNS,
): StripColumn[] {
  const samples: { index: number; price: number }[] = []
  for (const index of sampleColumns(prices, window, count)) {
    const price = prices[index]
    if (price === undefined || !Number.isFinite(price)) continue
    samples.push({ index, price })
  }
  const n = samples.length
  if (n === 0) return []
  const first = samples[0]!
  const last = samples[n - 1]!
  // Built once into one array, so `edges[k + 1]` handed out as column k's `to`
  // and as column k+1's `from` is literally the same float — contiguity by
  // construction rather than by rounding to within a tolerance.
  const edges: number[] = new Array<number>(n + 1)
  if (n === 1) {
    // Nothing to take a midpoint against, so fall back to the row's pitch —
    // which is never 0 or non-finite, so the column is always drawable.
    const half = columnPitch(prices, window, count) / 2
    edges[0] = first.price - half
    edges[1] = first.price + half
  } else {
    for (let k = 1; k < n; k += 1) edges[k] = (samples[k - 1]!.price + samples[k]!.price) / 2
    edges[0] = first.price - (samples[1]!.price - first.price) / 2
    edges[n] = last.price + (last.price - samples[n - 2]!.price) / 2
  }
  if (window) {
    // Only where there is data to the side of the end sample — see the note
    // above. `clip: true` on the row's series trims whatever this overshoots,
    // so reaching too far into the window costs nothing.
    if (gridContinues(prices, first.index, -1)) edges[0] = Math.min(edges[0]!, window.min)
    if (gridContinues(prices, last.index, 1)) edges[n] = Math.max(edges[n]!, window.max)
  }
  return samples.map((sample, k) => ({
    index: sample.index,
    price: sample.price,
    from: edges[k]!,
    to: edges[k + 1]!,
  }))
}

export const STRIP_ROW_HEIGHT = 30 // px, one strip row
export const STRIP_ROW_GAP = 2 // px between rows
export const STRIP_AXIS_SPACE = 26 // px under grid 0 for its price labels
export const STRIP_FLOOR = 6 // px under the bottom row

/**
 * How a row's half-height is shared between the bar and its value label.
 *
 * ONE prints the number in every column and draws a short bar under it;
 * the two never fight for the same pixels. The same split here: the bar
 * grows from the row's zero line and may fill at most `BAR_FRACTION` of the
 * half-row, and the label is centred `LABEL_FRACTION` of the half-row out —
 * in the band the bar can never reach — on the same side of zero as the
 * value. With a 30 px row (15 px half): bars up to ~8 px, a 9 px label whose
 * glyph box spans roughly 7–16 px. Anchoring labels to the bar *tip* instead
 * (ECharts' `'top'`/`'bottom'`) let a tall bar push its label into the
 * neighbouring row, and moving it inside the bar made it wider than the bar.
 *
 * `BAR_FRACTION` governs the bar's *height* only. Its width is its column's,
 * and the columns tile the price axis edge to edge (`stripColumns`).
 */
export const BAR_FRACTION = 0.55
export const LABEL_FRACTION = 0.76

/** The axis half-extent is the sampled max scaled by this, which is what
 *  holds the tallest bar at `BAR_FRACTION` of the half-row. */
export const EXTENT_PAD = 1 / BAR_FRACTION

/**
 * A row's y bounds: symmetric about zero, padded `EXTENT_PAD` beyond the
 * largest sampled magnitude, never narrower than ±`floor`. Non-finite
 * values are skipped; nothing finite → ±`floor`.
 */
export function symmetricExtent(
  values: readonly number[],
  indices: readonly number[],
  floor = 1,
): { min: number; max: number } {
  let maxAbs = 0
  for (const i of indices) {
    const v = values[i]
    if (v === undefined || !Number.isFinite(v)) continue
    const abs = Math.abs(v)
    if (abs > maxAbs) maxAbs = abs
  }
  const bound = Math.max(floor, maxAbs * EXTENT_PAD)
  return { min: -bound, max: bound }
}

/** Total pixel height the strip occupies — grid 0's `bottom`.
 *  6 rows: 6*30 + 5*2 + 26 + 6 = 222. */
export function stripHeight(rowCount: number = STRIP_ROWS.length): number {
  return rowCount * STRIP_ROW_HEIGHT + (rowCount - 1) * STRIP_ROW_GAP + STRIP_AXIS_SPACE + STRIP_FLOOR
}

/** Pixel `bottom` of strip row `i` (0-based from the top of the strip). */
function rowBottom(i: number): number {
  return STRIP_FLOOR + (STRIP_ROWS.length - 1 - i) * (STRIP_ROW_HEIGHT + STRIP_ROW_GAP)
}

/** Stable series id for a row's cursor box: `cursor-pnl`, `cursor-delta`,
 *  `cursor-gamma`, `cursor-theta`, `cursor-vega`, `cursor-wvega`. */
export function cursorSeriesId(key: StripRowKey): string {
  return `cursor-${key}`
}

const NOTE_TEXT = 'add T+0 to the dates to see greeks'

export interface StripBuildInput {
  data: Pick<RiskGraphResponse, 'prices' | 'lines' | 't0_greeks'>
  /** Visible price window, or null for the whole grid. */
  window: PriceWindow | null
  palette: ChartPalette
  /** Same pixel insets as grid 0. */
  left: number
  right: number
  /** Pinned x-axis bounds — identical to grid 0's `min`/`max`. */
  xMin: number
  xMax: number
  columns?: number
}

export interface StripOption {
  /** Grids 1..6, positioned by `bottom`, `height: STRIP_ROW_HEIGHT`. */
  grids: GridComponentOption[]
  /** x axes 1..6: `type:'value'`, same `min`/`max` as axis 0, `show:false`. */
  xAxes: XAXisComponentOption[]
  /** y axes 1..6: symmetric about zero, every part hidden. */
  yAxes: YAXisComponentOption[]
  /** Per row: one `custom` series drawing the row's contiguous bars (empty
   *  `data` when `stripValues` is null), then its `strip-label-*` scatter
   *  series carrying both the value labels and the row's zero-line
   *  `markLine`; after all rows, the six `cursor-*` scatter placeholders with
   *  empty data. */
  series: (CustomSeriesOption | ScatterSeriesOption)[]
  /** Row titles in the left margin, plus the muted note when there is no
   *  T+0 greek data. */
  titles: TitleComponentOption[]
  /** `stripHeight()` — what grid 0 must use as its `bottom`. */
  height: number
}

/**
 * The pixel rect of one strip bar.
 *
 * The two *horizontal* edges are snapped to whole device pixels. Neighbouring
 * columns are handed the identical edge number (`stripColumns` produces it
 * once), so they snap to the identical device pixel: no half-covered pixel
 * column — canvas antialiasing composites one to roughly 76 % of the fill, a
 * pale seam the eye reads as a gap — and no overlap, which would double-paint
 * the 55 % fill of a negative greek into a dark seam instead.
 *
 * Nothing is rounded *vertically*: the tallest bar is only ~8 px
 * (`BAR_FRACTION` of a 15 px half-row), so a 1 px quantisation would be a
 * 12 % error in the one thing a bar's height means — and vertical neighbours
 * never share an edge, so there is no seam to fix there. A value of exactly 0
 * gives `height: 0` and paints nothing, as the old bar series did.
 */
export function barRect(
  leftPx: number,
  rightPx: number,
  zeroPx: number,
  valuePx: number,
  dpr = 1,
): { x: number; y: number; width: number; height: number } {
  const scale = Number.isFinite(dpr) && dpr > 0 ? dpr : 1
  const snap = (v: number): number => Math.round(v * scale) / scale
  const left = snap(leftPx)
  const right = snap(rightPx)
  return {
    x: Math.min(left, right),
    // The clamp only fires for a column narrower than one device pixel, which
    // cannot happen at ~20 columns across a plot hundreds of pixels wide; it
    // is also the one case where two columns can overlap by a pixel, which is
    // still better than a bar that vanishes.
    width: Math.max(1 / scale, Math.abs(right - left)),
    y: Math.min(zeroPx, valuePx),
    height: Math.abs(valuePx - zeroPx),
  }
}

/** One drawn bar: what it reads, the slice of the axis it fills, and how it
 *  is painted. The row's `renderItem` closes over the array and looks itself
 *  up by `dataIndex`. */
interface StripBar {
  price: number
  value: number
  from: number
  to: number
  color: string
  opacity: number
}

function rowBars(
  row: StripRow,
  rowValues: readonly number[] | undefined,
  columns: readonly StripColumn[],
  palette: ChartPalette,
): StripBar[] {
  if (!rowValues) return []
  const out: StripBar[] = []
  for (const column of columns) {
    const raw = rowValues[column.index]
    // A greek the engine could not produce arrives as JSON `null`. Drawing it
    // as a zero-height bar labelled "$0" would claim the position is flat
    // there, which is a different statement from "not known" — so the column
    // is simply left blank, the way `symmetricExtent` already skips it.
    if (!Number.isFinite(raw)) continue
    const value = raw as number
    const style = row.barStyle(palette, value)
    out.push({
      price: column.price,
      value,
      from: column.from,
      to: column.to,
      color: style.color,
      opacity: style.opacity,
    })
  }
  return out
}

/**
 * One row's bars: a `custom` series that draws its own rects.
 *
 * The geometry and the colour both come from the `bars` closure rather than
 * from extra data dimensions or a per-item `itemStyle`: `bars[dataIndex]` is
 * trivially unit-testable and does not depend on custom-series dimension
 * inference, and it leaves the `data` payload as the plain
 * `{ value: [price, value] }` everything else reads. The end columns reach
 * past the window bound whenever there are greeks out there (`stripColumns`),
 * so `clip: true` trims them to the grid rect and the histogram runs edge to
 * edge instead of stopping short.
 */
function barSeries(
  row: StripRow,
  index: number,
  rowValues: readonly number[] | undefined,
  columns: readonly StripColumn[],
  palette: ChartPalette,
): CustomSeriesOption {
  const bars = rowBars(row, rowValues, columns, palette)
  return {
    type: 'custom',
    name: `strip-${row.key}`,
    xAxisIndex: index + 1,
    yAxisIndex: index + 1,
    silent: true,
    z: 2,
    clip: true,
    data: bars.map((bar) => ({ value: [bar.price, bar.value] })),
    renderItem: (
      params: CustomSeriesRenderItemParams,
      api: CustomSeriesRenderItemAPI,
    ): CustomSeriesRenderItemReturn => {
      // `dataIndex` is the raw index into the array we passed while nothing
      // filters the series (no dataZoom here); the guard keeps a future
      // filter from throwing rather than papering over one.
      const bar = bars[params.dataIndex]
      if (!bar) return undefined
      const [leftPx, zeroPx] = api.coord([bar.from, 0])
      const [rightPx, valuePx] = api.coord([bar.to, bar.value])
      return {
        type: 'rect',
        shape: barRect(leftPx!, rightPx!, zeroPx!, valuePx!, api.getDevicePixelRatio()),
        style: { fill: bar.color, opacity: bar.opacity },
        // The strip axes still dispatch `highlight` at whatever series sit on
        // them (`axisPointer.triggerEmphasis`); the bars are a read-only
        // backdrop and must not restyle themselves under the cursor.
        emphasisDisabled: true,
      }
    },
  }
}

/** Stable series id for a row's value labels: `strip-label-pnl`, … */
export function labelSeriesId(key: StripRowKey): string {
  return `strip-label-${key}`
}

/**
 * The row's carrier series: its value labels *and* its zero line.
 *
 * The labels are an invisible scatter point per sampled column, parked
 * `LABEL_FRACTION` of the way to the row's edge on the value's own side of
 * zero, carrying the value as a third tuple element for the formatter. They
 * are their own series because a `renderItem` rect has no label plumbing at
 * all, and because the row's layout wants the number in a fixed band rather
 * than pinned to the bar's rectangle — see `BAR_FRACTION`.
 *
 * The zero-line `markLine` rides here rather than on the row's `custom`
 * series simply because this scatter is a proven host (silent, on the right
 * grid pair, present for every row). It is a visual no-op: `MarkLineModel`
 * carries its own `z` (5), not its host series', so the line draws above the
 * z-2 bars either way.
 */
function labelSeries(
  row: StripRow,
  index: number,
  prices: readonly number[],
  rowValues: readonly number[] | undefined,
  indices: readonly number[],
  palette: ChartPalette,
  bound: number,
): ScatterSeriesOption {
  const data: [number, number, number][] = []
  if (rowValues) {
    for (const idx of indices) {
      const price = prices[idx]
      const raw = rowValues[idx]
      if (price === undefined || !Number.isFinite(raw)) continue
      const value = raw as number
      data.push([price, (value < 0 ? -1 : 1) * bound * LABEL_FRACTION, value])
    }
  }
  return {
    id: labelSeriesId(row.key),
    type: 'scatter',
    xAxisIndex: index + 1,
    yAxisIndex: index + 1,
    silent: true,
    symbolSize: 0,
    z: 3,
    data,
    itemStyle: { opacity: 1 },
    label: {
      show: true,
      position: 'inside',
      fontSize: 9,
      color: palette.muted,
      formatter: (params: { value: unknown }) => {
        const v = Array.isArray(params.value) ? Number(params.value[2]) : undefined
        return row.format(v !== undefined && Number.isFinite(v) ? v : undefined)
      },
    },
    // ~20 columns share the plot's width, so at a narrow window the pitch
    // drops under a formatted dollar label's width: left to itself the row
    // renders as a smear of half-overlapping numbers. `hideOverlap` is opt-in
    // per series, so it thins these and never the `cursor-*` boxes, which
    // stay the exact, always-legible read at the price the trader points at.
    labelLayout: { hideOverlap: true },
    markLine: {
      silent: true,
      symbol: 'none',
      label: { show: false },
      data: [{ yAxis: 0 }],
      lineStyle: { color: palette.zero, width: 1, opacity: 0.5 },
    },
    emphasis: { disabled: true },
  }
}

function emptyCursorSeries(row: StripRow, index: number, palette: ChartPalette): ScatterSeriesOption {
  const hue = row.barStyle(palette, 0).color
  return {
    id: cursorSeriesId(row.key),
    type: 'scatter',
    xAxisIndex: index + 1,
    yAxisIndex: index + 1,
    silent: true,
    symbolSize: 0,
    // Scatter clips by default: a point outside the grid rect is dropped
    // along with its label. The cursor sits at the *exact* grid index, which
    // can fall outside the row's sampled-and-padded extent (a spike between
    // sampled columns) — without this the boxed value silently vanishes for
    // that row while its neighbours keep theirs.
    clip: false,
    z: 20,
    data: [],
    // Scatter series default to itemStyle.opacity 0.8, which multiplies into
    // the box's background/border and its label — defeating the alpha
    // `palette.cursorBox` already encodes and letting the bars/markLine show
    // through the readout that is supposed to sit on top of them.
    itemStyle: { opacity: 1 },
    label: {
      show: true,
      position: 'inside',
      fontSize: 10,
      color: palette.text,
      backgroundColor: palette.cursorBox,
      borderColor: hue,
      borderWidth: 1,
      borderRadius: 2,
      padding: [2, 3],
    },
    markArea: {
      silent: true,
      itemStyle: { color: palette.cursorBand },
      emphasis: { disabled: true },
      data: [],
    },
  }
}

/**
 * Must not throw on empty data, an empty sample set, or a null
 * `stripValues` — the strip degrades to empty bars and a muted note rather
 * than taking down the whole chart.
 */
export function buildStrip(input: StripBuildInput): StripOption {
  const { data, window, palette, left, right, xMin, xMax, columns } = input
  const values = stripValues(data)
  const cols = stripColumns(data.prices, window, columns)
  const indices = cols.map((c) => c.index)
  const height = stripHeight()

  const grids: GridComponentOption[] = []
  const xAxes: XAXisComponentOption[] = []
  const yAxes: YAXisComponentOption[] = []
  const series: (CustomSeriesOption | ScatterSeriesOption)[] = []
  const titles: TitleComponentOption[] = []

  STRIP_ROWS.forEach((row, i) => {
    const bottom = rowBottom(i)
    grids.push({ left, right, bottom, height: STRIP_ROW_HEIGHT })
    xAxes.push({
      type: 'value',
      gridIndex: i + 1,
      min: xMin,
      max: xMax,
      show: false,
      // ECharts widens a numeric axis' value→pixel mapping by half a band
      // width (its `containShape`) whenever the axis carries a series whose
      // shapes could overflow the plot — bar, candlestick, boxplot. The row's
      // `custom` series asks for no such widening, so this is now a pinned
      // guarantee rather than an active correction: grid 0's x axis is never
      // widened, and the moment a strip axis were, the same price would map
      // to a different pixel here than under the curve above.
      containShape: false,
      // The crosshair must report the price under the cursor, not the bar
      // nearest it. A tooltip-driven axis pointer on a value axis snaps to
      // its own series' nearest sample, and this axis' series is the ~20
      // sampled bars — so the moment the mouse crossed into the strip the
      // cursor would jump in twenty steps, and `axisPointer.link` would push
      // that quantised price straight back to the P&L plot, the inset and
      // the cursor boxes. Grid 0 keeps its snap (its samples *are* the
      // curves'); these hand over the raw price and let `indexAt` choose the
      // grid sample.
      //
      // `label: { show: false }` because `axisPointer.link` puts all seven x
      // axes into one linked group, and a `cross`-type tooltip axis pointer
      // shows a price label on every axis in that group by default — without
      // this every hover draws six duplicate price chips stacked in the
      // strip on top of grid 0's own.
      //
      // `type: 'none'` because each grid draws its *own* pointer line across
      // its *own* rect: what looked like one crosshair running down the chart
      // was grid 0's line plus six strip segments. The pointer graphics take
      // z 50 from the axis-pointer model, well above the z-20 `cursor-*`
      // boxes and above the 0.92-alpha box background, so those six segments
      // struck straight through the numbers the boxes exist to make legible.
      // `type: 'none'` suppresses only the line element; the axis stays fully
      // registered. `show: false` would *not* do — `saveTooltipAxisInfo`
      // returns early on it, dropping the axis out of `axesInfo` and out of
      // the link group, so hovering a strip row would trigger no axis at all
      // and the cursor column would go dead. Grid 0's crosshair is untouched,
      // and the cursor column is still marked in the strip by the `cursor-*`
      // `markArea` band, which is what that band is for.
      axisPointer: { type: 'none', snap: false, label: { show: false } },
    })
    const rowValues = values ? values[row.key] : undefined
    const extent = symmetricExtent(rowValues ?? [], indices, row.extentFloor)
    yAxes.push({
      type: 'value',
      gridIndex: i + 1,
      min: extent.min,
      max: extent.max,
      show: false,
      axisLine: { show: false },
      axisLabel: { show: false },
      axisTick: { show: false },
      splitLine: { show: false },
      // The same linked cross-tooltip pointer also draws a y-value chip for
      // every y axis it can reach; the strip's y axes carry no meaningful
      // "value" for a crosshair (each row has its own unrelated scale), so
      // suppress the pointer entirely rather than just its label.
      axisPointer: { show: false },
    })
    series.push(barSeries(row, i, rowValues, cols, palette))
    series.push(labelSeries(row, i, data.prices, rowValues, indices, palette, extent.max))
    titles.push({
      text: row.label,
      left: 8,
      bottom: bottom + 8,
      textStyle: { fontSize: 10, color: palette.muted },
    })
  })

  STRIP_ROWS.forEach((row, i) => series.push(emptyCursorSeries(row, i, palette)))

  if (!values) {
    titles.push({
      text: NOTE_TEXT,
      left: 'center',
      bottom: Math.round(height / 2),
      textStyle: { fontSize: 11, color: palette.muted },
    })
  }

  return { grids, xAxes, yAxes, series, titles, height }
}

export interface CursorOverlayInput {
  data: Pick<RiskGraphResponse, 'prices' | 'lines' | 't0_greeks'>
  /** The grid sample under the cursor (or spot). `null` clears the boxes. */
  index: number | null
  window: PriceWindow | null
  palette: ChartPalette
  columns?: number
}

/**
 * A *merged* partial option (`setOption(overlay, { notMerge: false })`).
 *
 * Always returns all six `cursor-*` series — with `data: []` and an empty
 * `markArea.data` when there is nothing to show — so applying it clears the
 * previous cursor instead of leaving a stale box behind.
 */
export function buildCursorOverlay(input: CursorOverlayInput): EChartsOption {
  const { data, index, window, palette, columns } = input
  const values = stripValues(data)
  const price = index !== null ? data.prices[index] : undefined
  // The parked index can fall outside the *visible* window (e.g. after
  // panning/zooming away from spot while the underlying `prices` grid still
  // spans the wider fetched/tiled range) — `clip: false` below deliberately
  // defeats x/y clipping so an in-window point isn't dropped at the row's
  // sampled-and-padded y-extent, but that also means an off-window point
  // would be drawn at its extrapolated pixel position, off to the side of
  // the plot area (or beyond it) instead of being hidden. Gate on the
  // window explicitly instead of relying on clipping.
  const inWindow = price !== undefined && (window === null || (price >= window.min && price <= window.max))
  const hasPoint =
    index !== null && values !== null && price !== undefined && Number.isFinite(price) && inWindow
  const pitch = hasPoint ? columnPitch(data.prices, window, columns) : 1

  const series: ScatterSeriesOption[] = STRIP_ROWS.map((row, i) => {
    const rowValues = hasPoint ? values![row.key] : undefined
    const raw = hasPoint && index !== null ? rowValues?.[index] : undefined
    const value = Number.isFinite(raw) ? (raw as number) : 0
    const style = row.barStyle(palette, value)
    const point = hasPoint ? (price as number) : 0
    return {
      id: cursorSeriesId(row.key),
      type: 'scatter',
      xAxisIndex: i + 1,
      yAxisIndex: i + 1,
      silent: true,
      symbolSize: 0,
      // See `emptyCursorSeries` — the cursor lands at the exact grid index,
      // which can fall outside the row's sampled extent.
      clip: false,
      z: 20,
      // On the row's zero line, not at the value: the six boxes then stack
      // into one straight column under the crosshair, the way ONE prints the
      // cursor's readout, instead of jumping up and down with the bars.
      data: hasPoint ? [[point, 0]] : [],
      // See `emptyCursorSeries` — scatter's default itemStyle.opacity (0.8)
      // would otherwise fade the box and its border below the palette's own
      // alpha and let the bars/markLine show through it.
      itemStyle: { opacity: 1 },
      label: {
        show: true,
        position: 'inside',
        fontSize: 10,
        color: palette.text,
        backgroundColor: palette.cursorBox,
        borderColor: style.color,
        borderWidth: 1,
        borderRadius: 2,
        padding: [2, 3],
        formatter: () => row.format(hasPoint ? raw : undefined),
      },
      markArea: {
        silent: true,
        itemStyle: { color: palette.cursorBand },
        emphasis: { disabled: true },
        data: hasPoint
          ? [[{ xAxis: point - pitch / 2 }, { xAxis: point + pitch / 2 }]]
          : [],
      },
    }
  })

  return { series }
}
