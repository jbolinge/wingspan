/** ECharts host for the risk graph.
 *
 *  Only the pieces the graph actually uses are registered, so the SPA does not
 *  ship the whole ECharts bundle. The option itself is built by the pure
 *  `buildRiskChartOption`; this component owns nothing but the canvas, the
 *  resize observer and the crosshair callback. */

import { LineChart, ScatterChart } from 'echarts/charts'
import {
  GridComponent,
  MarkAreaComponent,
  MarkLineComponent,
  MarkPointComponent,
  TooltipComponent,
} from 'echarts/components'
import * as echarts from 'echarts/core'
import type { ECharts } from 'echarts/core'
import { CanvasRenderer } from 'echarts/renderers'
import type { EChartsOption } from 'echarts'
import { useEffect, useRef } from 'react'

echarts.use([
  LineChart,
  ScatterChart,
  GridComponent,
  TooltipComponent,
  MarkLineComponent,
  MarkAreaComponent,
  MarkPointComponent,
  CanvasRenderer,
])

interface AxisPointerEvent {
  axesInfo?: { axisDim?: string; value?: number | string }[]
}

export function RiskChart({
  option,
  height,
  onCursorPrice,
  ariaLabel,
}: {
  option: EChartsOption
  height: number
  onCursorPrice?: (price: number | null) => void
  ariaLabel: string
}) {
  const hostRef = useRef<HTMLDivElement | null>(null)
  const chartRef = useRef<ECharts | null>(null)
  const cursorRef = useRef(onCursorPrice)

  useEffect(() => {
    cursorRef.current = onCursorPrice
  }, [onCursorPrice])

  useEffect(() => {
    const host = hostRef.current
    if (!host) return undefined
    const chart = echarts.init(host, undefined, { renderer: 'canvas' })
    chartRef.current = chart

    chart.on('updateAxisPointer', (params) => {
      const info = (params as AxisPointerEvent).axesInfo?.find((a) => a.axisDim === 'x')
      const value = info?.value
      cursorRef.current?.(typeof value === 'number' ? value : Number(value))
    })
    chart.on('globalout', () => cursorRef.current?.(null))

    const observer = new ResizeObserver(() => chart.resize())
    observer.observe(host)
    return () => {
      observer.disconnect()
      chart.dispose()
      chartRef.current = null
    }
  }, [])

  useEffect(() => {
    chartRef.current?.setOption(option, { notMerge: true, lazyUpdate: false })
  }, [option])

  useEffect(() => {
    chartRef.current?.resize()
  }, [height])

  return (
    <div
      ref={hostRef}
      role="img"
      aria-label={ariaLabel}
      data-testid="risk-chart"
      style={{ height, width: '100%' }}
    />
  )
}
