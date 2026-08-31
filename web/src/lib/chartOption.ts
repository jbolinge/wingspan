/**
 * Builds the ECharts option for the ONE-style risk graph.
 *
 * Kept as a pure function so the whole visual contract — line weights, dash
 * grading, the zero line, the spot marker, the sigma bands and the breakeven
 * markers — is unit-testable without a canvas.
 */

import type { EChartsOption } from 'echarts'
import type { RiskGraphResponse, RiskLine } from '../api/types'
import { formatDate } from './dates'
import { fmtMoney } from './format'
import { perTradeColor, tplusColor, tplusDash, type ChartPalette } from './palette'

export interface PerTradeSeries {
  tradeId: string
  name: string
  pnl: number[]
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

export function lineLabel(line: RiskLine, today: string): string {
  if (line.kind === 'expiry') return `EXP ${formatDate(line.date)}`
  if (line.kind === 't0' || line.date === today) return 'T+0'
  return `T+${formatDate(line.date)}`
}

export function buildRiskChartOption(input: RiskChartInput): EChartsOption {
  const { data, visibleLines, perTrade, palette, showSigmaBands, showBreakevens, yLock } = input
  const prices = data.prices
  const first = prices[0] ?? 0
  const last = prices[prices.length - 1] ?? 0

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
      name: lineLabel(line, data.as_of.slice(0, 10)),
      data: zip(prices, line.pnl),
      showSymbol: false,
      z: 4,
      lineStyle: { width: 1.6, color, type: tplusDash(index, tplus.length) },
      itemStyle: { color },
      emphasis: { disabled: true },
    })
  })

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
      name: lineLabel(line, data.as_of.slice(0, 10)),
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

  return {
    animation: false,
    backgroundColor: palette.background,
    grid: { left: 68, right: 20, top: 16, bottom: 30 },
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
    xAxis: {
      type: 'value',
      min: first,
      max: last,
      axisLine: { lineStyle: { color: palette.axis } },
      axisLabel: { color: palette.muted, fontSize: 10, formatter: (v: number) => v.toFixed(0) },
      splitLine: { show: true, lineStyle: { color: palette.grid } },
    },
    yAxis: {
      type: 'value',
      scale: true,
      ...(yLock ? { min: yLock.min, max: yLock.max } : {}),
      axisLine: { show: false },
      axisLabel: {
        color: palette.muted,
        fontSize: 10,
        formatter: (v: number) => fmtMoney(v, { dp: 0 }),
      },
      splitLine: { show: true, lineStyle: { color: palette.grid } },
    },
    series,
  }
}
