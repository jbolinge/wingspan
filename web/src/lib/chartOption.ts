/**
 * Builds the ECharts option for the ONE-style risk graph.
 *
 * Kept as a pure function so the whole visual contract — line weights, dash
 * grading, the zero line, the spot marker, the sigma bands and the breakeven
 * markers — is unit-testable without a canvas.
 */

import type { EChartsOption } from 'echarts'
import type { PerTradeLine, RiskGraphResponse, RiskLine } from '../api/types'
import { fmtMoney } from './format'
import { buildStrip } from './greekStrip'
import { isLineHidden, lineLabel } from './lines'
import { expiryRanks, expiryStyle, perTradeColor, tplusColor, tplusDash, type ChartPalette } from './palette'
import { yExtentInWindow, type PriceWindow } from './viewport'

/** Grid 0's pixel insets — pinned here (not exported from `greekStrip`, to
 *  avoid a circular import) and passed to `buildStrip` so the strip's grids
 *  line up with the P&L plot's left margin and right edge exactly. */
const GRID_LEFT = 68
const GRID_RIGHT = 20

export interface PerTradeSeries {
  tradeId: string
  name: string
  pnl: number[]
}

/**
 * The position without the model: the same request with `model_legs: []`,
 * drawn as one dashed, warm-grey ghost family under the combined curves so
 * the trader sees exactly what the adjustment changes — every line they have
 * left visible, not just T+0 and the expiration. Carries its own price grid:
 * an automatic range is computed from the strikes in play, so it can differ
 * from the model's.
 */
export interface CompareLines {
  prices: number[]
  /** The position-only lines the trader has left switched on, by key. */
  lines: RiskLine[]
}

/** What every ghost line's legend name ends in. */
export const COMPARE_SUFFIX = ' (position)'

/** Legend name of a ghost line: its own label plus the family suffix. */
export function compareLabel(line: RiskLine): string {
  return `${lineLabel(line)}${COMPARE_SUFFIX}`
}

/** The dash the ghost T+0 and expiration are drawn with: longer gaps than any
 *  T+x ladder dash, so the two families never read as one. */
export const COMPARE_DASH = [3, 5]
/** …and the ghost ladder's: finer still, so it sits quietly behind. */
export const COMPARE_TPLUS_DASH = [2, 4]

/** Picks the ghost lines out of a position-only response, honouring the
 *  trader's line toggles (`t0`, `t+12`, an expiration by its date — see
 *  `hideKey`). */
export function compareLinesOf(
  data: RiskGraphResponse | undefined,
  hiddenLines: readonly string[],
): CompareLines | null {
  if (!data) return null
  return {
    prices: data.prices,
    lines: data.lines.filter((line) => !isLineHidden(line, hiddenLines)),
  }
}

export interface RiskChartInput {
  data: RiskGraphResponse
  /** Lines the trader has left switched on, any order. */
  visibleLines: RiskLine[]
  perTrade: PerTradeSeries[]
  palette: ChartPalette
  showSigmaBands: boolean
  showBreakevens: boolean
  /** Frozen y-extent, or null to auto-scale. */
  yLock: { min: number; max: number } | null
  /** Visible slice of the price axis, or null for the whole computed range. */
  xWindow: PriceWindow | null
  /** The ghost family while a model is drawn over a position and the
   *  position toggle is on. */
  compare?: CompareLines | null
}

function zip(prices: number[], pnl: number[]): [number, number][] {
  const out: [number, number][] = []
  for (let i = 0; i < prices.length; i += 1) {
    const x = prices[i]
    const y = pnl[i]
    if (x === undefined || y === undefined) continue
    out.push([x, y])
  }
  return out
}

/**
 * The per-trade curves the overlay should draw.
 *
 * `/risk-graph` answers with one curve per (trade, date) pair, so the raw
 * array holds `trades × dates` entries. The overlay is a decomposition of the
 * *current* aggregate — its series are labelled “(T+0)” — so it takes exactly
 * one curve per trade: the T+0 one, or the first line's when the trader has
 * edited the date list down to something with no T+0 in it.
 */
export function perTradeOverlayCurves(data: RiskGraphResponse | undefined): PerTradeLine[] {
  if (!data) return []
  const key = (data.lines.find((line) => line.kind === 't0') ?? data.lines[0])?.key
  if (key === undefined) return []
  const seen = new Set<string>()
  return data.per_trade.filter((curve) => {
    if (curve.key !== key || seen.has(curve.trade_id)) return false
    seen.add(curve.trade_id)
    return true
  })
}

// The server's own label, not one recomputed from the browser clock — see
// `lib/lines`. Re-exported so every caller keeps one import.
export { lineLabel }

/**
 * The breakevens the chart marks: the canonical expiration line's — the bare
 * `expiry` key, which is always the live front expiry's expiration line, the one
 * the header's max profit/loss, PoP and BE readout describe — and only while
 * that line is switched on. The other lines' crossings are in `breakevens`
 * too, but drawn as diamonds on the zero line they read as expiration
 * breakevens they are not, and a hidden line's would mark a curve that is
 * not on screen.
 */
export function canonicalBreakevens(
  data: Pick<RiskGraphResponse, 'breakevens'>,
  visibleLines: readonly Pick<RiskLine, 'key'>[],
): number[] {
  if (!visibleLines.some((line) => line.key === 'expiry')) return []
  return data.breakevens.expiry ?? []
}

function unionExtent(
  a: { min: number; max: number } | null,
  b: { min: number; max: number } | null,
): { min: number; max: number } | null {
  if (a === null) return b
  if (b === null) return a
  return { min: Math.min(a.min, b.min), max: Math.max(a.max, b.max) }
}

export function buildRiskChartOption(input: RiskChartInput): EChartsOption {
  const { data, visibleLines, perTrade, palette, showSigmaBands, showBreakevens, yLock } = input
  const view = input.xWindow
  const compare = input.compare ?? null
  const compareCurves = compare ? compare.lines : []
  const prices = data.prices
  const first = prices[0] ?? 0
  const last = prices[prices.length - 1] ?? 0

  // Zoomed or panned, the y axis follows the curves actually on screen so a
  // flat region opens up vertically; 'lock y-axis' still wins, and with no
  // window the option stays exactly what it was before zooming existed. A
  // window panned clean off the fetched data measures nothing (null), which
  // leaves the axis to ECharts rather than pinning it to a stale extent.
  const windowBounds =
    yLock === null && view
      ? unionExtent(
          yExtentInWindow([...visibleLines, ...perTrade], prices, view),
          compare ? yExtentInWindow(compareCurves, compare.prices, view) : null,
        )
      : null
  const yBounds = yLock ?? windowBounds

  const ordered = [...visibleLines].sort((a, b) => a.date.localeCompare(b.date))
  const tplus = ordered.filter((l) => l.kind === 'tplus')

  const series: NonNullable<EChartsOption['series']> = []

  // 1. Sigma bands sit underneath everything, drawn by a silent carrier series.
  if (showSigmaBands) {
    series.push({
      type: 'line',
      name: 'sigma',
      silent: true,
      data: [],
      z: 1,
      markArea: {
        silent: true,
        itemStyle: { color: palette.band2 },
        emphasis: { disabled: true },
        data: [
          [
            { xAxis: data.sigma_bands.two[0], itemStyle: { color: palette.band2 } },
            { xAxis: data.sigma_bands.two[1] },
          ],
          [
            { xAxis: data.sigma_bands.one[0], itemStyle: { color: palette.band1 } },
            { xAxis: data.sigma_bands.one[1] },
          ],
        ],
      },
    })
  }

  // 2. Per-trade overlay: thin, behind the aggregate curves.
  perTrade.forEach((trade, index) => {
    series.push({
      type: 'line',
      name: `${trade.name} (T+0)`,
      data: zip(prices, trade.pnl),
      showSymbol: false,
      z: 2,
      lineStyle: { width: 1, color: perTradeColor(palette, index), opacity: 0.75 },
      itemStyle: { color: perTradeColor(palette, index) },
      emphasis: { disabled: true },
    })
  })

  // 3. T+x ladder: dashed, graded from the near date to the far one.
  tplus.forEach((line, index) => {
    const color = tplusColor(palette, index, tplus.length)
    series.push({
      type: 'line',
      name: lineLabel(line),
      data: zip(prices, line.pnl),
      showSymbol: false,
      z: 4,
      lineStyle: { width: 1.6, color, type: tplusDash(index, tplus.length) },
      itemStyle: { color },
      emphasis: { disabled: true },
    })
  })

  // 3b. The ghost family — the position without the model — in its one hue,
  //     dashed, weighted like the lines it shadows: the ladder underneath the
  //     live ladder, T+0 and the expiration above it but under their own live
  //     counterparts, so a line and its ghost always sit together.
  if (compare) {
    for (const line of [...compare.lines].sort((a, b) => a.date.localeCompare(b.date))) {
      const ghost =
        line.kind === 'tplus'
          ? { width: 1.2, type: COMPARE_TPLUS_DASH, opacity: 0.6, z: 3 }
          : line.kind === 't0'
            ? { width: 1.8, type: COMPARE_DASH, opacity: 0.9, z: 5 }
            : { width: 2.4, type: COMPARE_DASH, opacity: 0.9, z: 5 }
      series.push({
        type: 'line',
        name: compareLabel(line),
        data: zip(compare.prices, line.pnl),
        showSymbol: false,
        z: ghost.z,
        lineStyle: { width: ghost.width, color: palette.position, type: ghost.type, opacity: ghost.opacity },
        itemStyle: { color: palette.position },
        emphasis: { disabled: true },
      })
    }
  }

  // 4. T+0: solid, brighter.
  for (const line of ordered.filter((l) => l.kind === 't0')) {
    series.push({
      type: 'line',
      name: 'T+0',
      data: zip(prices, line.pnl),
      showSymbol: false,
      z: 6,
      lineStyle: { width: 2.2, color: palette.t0 },
      itemStyle: { color: palette.t0 },
      emphasis: { disabled: true },
    })
  }

  // 5. Expiration: the thick line the eye should land on first — the
  //    canonical (live-front) expiration line; the others, by date, are
  //    thinner and dashed, each its own
  //    pattern, ranked over every line in the answer so hiding one never
  //    restyles another (`expiryStyle`).
  const expiryRank = expiryRanks(data.lines)
  for (const line of ordered.filter((l) => l.kind === 'expiry')) {
    const style = expiryStyle(expiryRank.get(line.key) ?? 0)
    series.push({
      type: 'line',
      name: lineLabel(line),
      data: zip(prices, line.pnl),
      showSymbol: false,
      z: 8,
      lineStyle: { width: style.width, color: palette.expiry, ...(style.dash ? { type: style.dash } : {}) },
      itemStyle: { color: palette.expiry },
      emphasis: { disabled: true },
    })
  }

  // 6. Zero line and spot marker ride on a silent carrier series so they are
  //    independent of which curves are switched on.
  series.push({
    type: 'line',
    name: 'markers',
    silent: true,
    data: [],
    z: 9,
    markLine: {
      silent: true,
      symbol: 'none',
      label: { show: true, color: palette.muted, fontSize: 10, formatter: '{b}' },
      data: [
        {
          name: '0',
          yAxis: 0,
          lineStyle: { color: palette.zero, width: 1, type: 'solid', opacity: 0.6 },
          label: { show: false },
        },
        {
          name: `spot ${data.spot.toFixed(0)}`,
          xAxis: data.spot,
          lineStyle: { color: palette.spot, width: 1.2, type: [6, 4] },
          label: { show: true, position: 'insideEndTop', color: palette.spot, fontSize: 10 },
        },
      ],
    },
  })

  // 7. Breakevens on the (canonical) expiration line.
  if (showBreakevens) {
    const points = canonicalBreakevens(data, visibleLines)
      .filter((price) => price >= first && price <= last)
      .map((price) => [price, 0] as [number, number])
    if (points.length > 0) {
      series.push({
        type: 'scatter',
        name: 'breakeven',
        data: points,
        symbol: 'diamond',
        symbolSize: 8,
        z: 12,
        itemStyle: { color: palette.breakeven },
        label: {
          show: true,
          position: 'top',
          fontSize: 10,
          color: palette.breakeven,
          formatter: (params: { value: unknown }) =>
            Array.isArray(params.value) ? String(Math.round(Number(params.value[0]))) : '',
        },
        emphasis: { disabled: true },
      })
    }
  }

  const xMin = view ? view.min : first
  const xMax = view ? view.max : last
  const strip = buildStrip({
    data,
    window: view,
    palette,
    left: GRID_LEFT,
    right: GRID_RIGHT,
    xMin,
    xMax,
  })

  return {
    animation: false,
    backgroundColor: palette.background,
    title: strip.titles,
    // Links every grid's crosshair so a hover anywhere — the P&L plot or a
    // strip row — moves the cursor column across all of them at once.
    axisPointer: { link: [{ xAxisIndex: 'all' }] },
    grid: [{ left: GRID_LEFT, right: GRID_RIGHT, top: 16, bottom: strip.height }, ...strip.grids],
    // showContent:false keeps the crosshair (and its updateAxisPointer events)
    // while the readout is rendered by React next to the chart.
    tooltip: {
      trigger: 'axis',
      showContent: false,
      axisPointer: {
        type: 'cross',
        snap: true,
        lineStyle: { color: palette.muted, width: 1, type: 'dashed' },
        crossStyle: { color: palette.muted, width: 1, type: 'dashed' },
        label: { backgroundColor: palette.axis, fontSize: 10 },
      },
    },
    xAxis: [
      {
        type: 'value',
        min: xMin,
        max: xMax,
        axisLine: { lineStyle: { color: palette.axis } },
        axisLabel: { color: palette.muted, fontSize: 10, formatter: (v: number) => v.toFixed(0) },
        splitLine: { show: true, lineStyle: { color: palette.grid } },
      },
      ...strip.xAxes,
    ],
    yAxis: [
      {
        type: 'value',
        scale: true,
        ...(yBounds ? { min: yBounds.min, max: yBounds.max } : {}),
        axisLine: { show: false },
        axisLabel: {
          color: palette.muted,
          fontSize: 10,
          formatter: (v: number) => fmtMoney(v, { dp: 0 }),
          // A window-derived bound is wherever the padded curves happened to
          // end, so ECharts' boundary tick lands off the round interval and
          // collides with the label next to it. The frozen (locked) extent is
          // a number the trader chose, so that one keeps its labels.
          ...(windowBounds ? { showMinLabel: false, showMaxLabel: false } : {}),
        },
        splitLine: { show: true, lineStyle: { color: palette.grid } },
      },
      ...strip.yAxes,
    ],
    series: [...series, ...strip.series],
  }
}
