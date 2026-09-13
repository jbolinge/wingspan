/**
 * The greek strip: six rows of small bars (P&L, Δ, Γ, Θ, V, WV) drawn under
 * the risk graph's P&L plot, all T+0-only.
 *
 * Two ideas carry the whole module. First, the strip shares grid 0's price
 * axis pixel-for-pixel — the same `left`/`right` insets and the same x-axis
 * `min`/`max` as the P&L plot, plus `axisPointer.link` in the chart option —
 * so a bar always sits directly under the curve sample it describes and a
 * drag/zoom on either plot moves both together. Second, every row's y axis
 * is symmetric about zero (`symmetricExtent`), so the zero line is the row's
 * vertical midline and sign reads visually as up (positive) or down
 * (negative) rather than as a number you have to read to interpret.
 */

import type {
  BarSeriesOption,
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
  /** Per row: one `bar` series (empty `data` when `stripValues` is null)
   *  carrying a zero-line `markLine`, then its `strip-label-*` scatter
   *  series holding the value labels; after all rows, the six `cursor-*`
   *  scatter placeholders with empty data. */
  series: (BarSeriesOption | ScatterSeriesOption)[]
  /** Row titles in the left margin, plus the muted note when there is no
   *  T+0 greek data. */
  titles: TitleComponentOption[]
  /** `stripHeight()` — what grid 0 must use as its `bottom`. */
  height: number
}

function barData(
  row: StripRow,
  prices: readonly number[],
  rowValues: readonly number[] | undefined,
  indices: readonly number[],
  palette: ChartPalette,
): NonNullable<BarSeriesOption['data']> {
  if (!rowValues) return []
  const out: NonNullable<BarSeriesOption['data']> = []
  for (const idx of indices) {
    const price = prices[idx]
    if (price === undefined) continue
    const raw = rowValues[idx]
    // A greek the engine could not produce arrives as JSON `null`. Drawing it
    // as a zero-height bar labelled "$0" would claim the position is flat
    // there, which is a different statement from "not known" — so the column
    // is simply left blank, the way `symmetricExtent` already skips it.
    if (!Number.isFinite(raw)) continue
    const value = raw as number
    const style = row.barStyle(palette, value)
    out.push({
      value: [price, value],
      itemStyle: { color: style.color, opacity: style.opacity },
    })
  }
  return out
}

/** Stable series id for a row's value labels: `strip-label-pnl`, … */
export function labelSeriesId(key: StripRowKey): string {
  return `strip-label-${key}`
}

/**
 * The value labels of one row: an invisible scatter point per sampled
 * column, parked `LABEL_FRACTION` of the way to the row's edge on the
 * value's own side of zero, carrying the value as a third tuple element for
 * the formatter. A separate series because a bar label can only be placed
 * relative to the bar's own rectangle — see `BAR_FRACTION`.
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
  const indices = sampleColumns(data.prices, window, columns)
  const height = stripHeight()

  const grids: GridComponentOption[] = []
  const xAxes: XAXisComponentOption[] = []
  const yAxes: YAXisComponentOption[] = []
  const series: (BarSeriesOption | ScatterSeriesOption)[] = []
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
      // A numeric axis carrying a bar series widens its value→pixel mapping
      // by half a bar's band width by default (ECharts' `containShape`) so
      // the bar shape never overflows the plot. Grid 0's x axis carries only
      // line series and is never widened, so without this a strip bar would
      // land up to half a column pitch off the curve sample it describes —
      // worst at the window's edges, exact only at the midpoint.
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
      axisPointer: { snap: false, label: { show: false } },
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
    series.push({
      type: 'bar',
      name: `strip-${row.key}`,
      xAxisIndex: i + 1,
      yAxisIndex: i + 1,
      barWidth: '60%',
      silent: true,
      z: 2,
      data: barData(row, data.prices, rowValues, indices, palette),
      // The value labels are their own series (`labelSeries`): a bar label
      // can only sit relative to the bar's rectangle, and the row layout
      // wants it in a fixed band instead.
      label: { show: false },
      markLine: {
        silent: true,
        symbol: 'none',
        label: { show: false },
        data: [{ yAxis: 0 }],
        lineStyle: { color: palette.zero, width: 1, opacity: 0.5 },
      },
      emphasis: { disabled: true },
    })
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
