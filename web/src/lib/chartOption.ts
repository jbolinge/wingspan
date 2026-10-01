/**
 * Builds the ECharts option for the ONE-style risk graph.
 *
 * Kept as a pure function so the whole visual contract — line weights, dash
 * grading, the zero line, the spot marker, the sigma bands and the breakeven
 * markers — is unit-testable without a canvas.
 */

import type { EChartsOption } from 'echarts'
import type { PerTradeLine, RiskGraphResponse, RiskLine } from '../api/types'
import { daysBetween, formatDate, todayIso } from './dates'
import { fmtMoney } from './format'
import { buildStrip } from './greekStrip'
import { perTradeColor, tplusColor, tplusDash, type ChartPalette } from './palette'
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
 * drawn dashed and muted under the model's own curves so the trader sees what
 * the adjustment changes. Carries its own price grid — an automatic range is
 * computed from the strikes in play, so it can differ from the model's.
 */
export interface CompareLines {
  prices: number[]
  /** null when the trader has hidden T+0 (or the response has none). */
  t0: number[] | null
  /** The first expiration line; null when hidden or absent. */
  expiry: number[] | null
}

/** Legend names of the compare series — also what GraphControls lists. */
export const COMPARE_T0_LABEL = 'T+0 (position)'
export const COMPARE_EXPIRY_LABEL = 'Exp (position)'

/** The dash both compare lines are drawn with: longer gaps than any T+x
 *  ladder dash, so the two families never read as one. */
export const COMPARE_DASH = [3, 5]

/** Picks the compare curves out of a position-only response, honouring the
 *  trader's line toggles by key (`t0`, `expiry`). */
export function compareLinesOf(
  data: RiskGraphResponse | undefined,
  hiddenLines: readonly string[],
): CompareLines | null {
  if (!data) return null
  const t0 = data.lines.find((line) => line.kind === 't0')
  const expiry = data.lines.find((line) => line.kind === 'expiry')
  return {
    prices: data.prices,
    t0: t0 && !hiddenLines.includes(t0.key) ? t0.pnl : null,
    expiry: expiry && !hiddenLines.includes(expiry.key) ? expiry.pnl : null,
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
  /** Position-only lines while a model is drawn and compare is on. */
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

export function lineLabel(line: RiskLine, today: string): string {
  if (line.kind === 'expiry') return `EXP ${formatDate(line.date)}`
  if (line.kind === 't0' || line.date === today) return 'T+0'
  return `T+${daysBetween(today, line.date)}`
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
  const compareCurves = compare
    ? [compare.t0, compare.expiry].filter((pnl): pnl is number[] => pnl !== null).map((pnl) => ({ pnl }))
    : []
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
      // Local calendar day, not the server's UTC as_of sliced to a date — see
      // GraphControls.
      name: lineLabel(line, todayIso()),
      data: zip(prices, line.pnl),
      showSymbol: false,
      z: 4,
      lineStyle: { width: 1.6, color, type: tplusDash(index, tplus.length) },
      itemStyle: { color },
      emphasis: { disabled: true },
    })
  })

  // 3b. The position without the model: dashed, in the hue of the line it
  //     shadows but faded, under the model's T+0 and expiration lines.
  if (compare) {
    const shadows: [string, number[] | null, string, number][] = [
      [COMPARE_T0_LABEL, compare.t0, palette.t0, 1.6],
      [COMPARE_EXPIRY_LABEL, compare.expiry, palette.expiry, 2],
    ]
    for (const [name, pnl, color, width] of shadows) {
      if (pnl === null) continue
      series.push({
        type: 'line',
        name,
        data: zip(compare.prices, pnl),
        showSymbol: false,
        z: 5,
        lineStyle: { width, color, type: COMPARE_DASH, opacity: 0.55 },
        itemStyle: { color },
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

  // 5. Expiration: the thick line the eye should land on first.
  for (const line of ordered.filter((l) => l.kind === 'expiry')) {
    series.push({
      type: 'line',
      name: lineLabel(line, todayIso()),
      data: zip(prices, line.pnl),
      showSymbol: false,
      z: 8,
      lineStyle: { width: 3, color: palette.expiry },
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

  // 7. Breakevens on the expiration line.
  if (showBreakevens) {
    const points = Object.values(data.breakevens)
      .flat()
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
