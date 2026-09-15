/** ECharts host for the risk graph.
 *
 *  Only the pieces the graph actually uses are registered, so the SPA does not
 *  ship the whole ECharts bundle. The option itself is built by the pure
 *  `buildRiskChartOption`; this component owns nothing but the canvas, the
 *  resize observer, the crosshair callback and the drag/wheel gestures that
 *  move the visible price window (whose arithmetic lives in `lib/viewport`).
 *
 *  The main option is applied `notMerge: true` — a full rebuild of every
 *  series — on every pan/zoom frame; doing that for a crosshair move too
 *  would tear the whole chart down and redraw it per mouse move. So the
 *  cursor highlight is a second, separately-supplied `overlay` option merged
 *  on top with `notMerge: false` instead. */

import { CustomChart, LineChart, ScatterChart } from 'echarts/charts'
import {
  AxisPointerComponent,
  GridComponent,
  MarkAreaComponent,
  MarkLineComponent,
  MarkPointComponent,
  TitleComponent,
  TooltipComponent,
} from 'echarts/components'
import * as echarts from 'echarts/core'
import type { ECharts } from 'echarts/core'
import { CanvasRenderer } from 'echarts/renderers'
import type { EChartsOption } from 'echarts'
import { useEffect, useRef, useState } from 'react'

echarts.use([
  CustomChart,
  LineChart,
  ScatterChart,
  GridComponent,
  TooltipComponent,
  AxisPointerComponent,
  TitleComponent,
  MarkLineComponent,
  MarkAreaComponent,
  MarkPointComponent,
  CanvasRenderer,
])

interface AxisPointerEvent {
  axesInfo?: { axisDim?: string; axisIndex?: number; value?: number | string }[]
}

/** Below this the gesture is still a click, not a drag. */
const DRAG_THRESHOLD_PX = 3
/** Baseline for the price-per-pixel measurement taken at drag start. */
const PROBE_PX = 100
/** One 100px wheel notch ≈ 1.16×. */
const WHEEL_GAIN = 0.0015

/** Price under a pixel on the x axis, or null outside a drawn chart. */
function priceAtPixel(chart: ECharts, x: number): number | null {
  const value = chart.convertFromPixel({ xAxisIndex: 0 }, x)
  return typeof value === 'number' && Number.isFinite(value) ? value : null
}

export function RiskChart({
  option,
  overlay,
  height,
  onCursorPrice,
  onPan,
  onZoom,
  onResetView,
  pannable = false,
  canZoomIn = true,
  canZoomOut = true,
  ariaLabel,
}: {
  option: EChartsOption
  /** A partial option merged on top of `option` after every rebuild —
   *  the greek strip's cursor boxes. Null/undefined applies nothing. */
  overlay?: EChartsOption | null
  /** A fixed pixel height, or `'fill'` to take the whole parent (which must
   *  then have a definite height of its own); the resize observer keeps the
   *  canvas in step either way. */
  height: number | 'fill'
  onCursorPrice?: (price: number | null) => void
  /** Drag: how far the window should move along the price axis, in price
   *  units, since the previous frame (positive = window moves right). */
  onPan?: (deltaPrice: number) => void
  /** Wheel: multiply the zoom by `factor`, keeping `anchorPrice` put. */
  onZoom?: (factor: number, anchorPrice: number) => void
  /** Double click: back to the whole computed range. */
  onResetView?: () => void
  /** There is a window to drag — i.e. there is data. Panning is allowed at
   *  every zoom now (the request follows the window), so this only gates the
   *  drag itself, the grab cursor and the touch-action claim. */
  pannable?: boolean
  /** Whether a wheel notch could still change the zoom in that direction.
   *  When it cannot, the wheel is left alone so it scrolls the panel
   *  underneath instead of being swallowed for nothing. */
  canZoomIn?: boolean
  canZoomOut?: boolean
  ariaLabel: string
}) {
  const hostRef = useRef<HTMLDivElement | null>(null)
  const chartRef = useRef<ECharts | null>(null)
  // The main option object last applied via `notMerge: true`. A crosshair
  // move produces a new `overlay` (and often a referentially-stable
  // `option`) far more often than the option itself changes — rebuilding
  // every series on every mouse tick is exactly what the module comment
  // above says this design avoids, so the rebuild is skipped whenever
  // `option` is the same object already applied.
  const appliedOptionRef = useRef<EChartsOption | null>(null)
  const cursorRef = useRef(onCursorPrice)
  const panRef = useRef(onPan)
  const zoomRef = useRef(onZoom)
  const resetRef = useRef(onResetView)
  const pannableRef = useRef(pannable)
  const zoomInRef = useRef(canZoomIn)
  const zoomOutRef = useRef(canZoomOut)
  const [dragging, setDragging] = useState(false)

  // Callbacks (and the one flag the listeners read) live in refs so a new
  // closure from the panel never tears the chart down and rebuilds it.
  useEffect(() => {
    cursorRef.current = onCursorPrice
    panRef.current = onPan
    zoomRef.current = onZoom
    resetRef.current = onResetView
    pannableRef.current = pannable
    zoomInRef.current = canZoomIn
    zoomOutRef.current = canZoomOut
  }, [onCursorPrice, onPan, onZoom, onResetView, pannable, canZoomIn, canZoomOut])

  useEffect(() => {
    const host = hostRef.current
    if (!host) return undefined
    const chart = echarts.init(host, undefined, { renderer: 'canvas' })
    chartRef.current = chart

    chart.on('updateAxisPointer', (params) => {
      // Seven x axes are linked now — the P&L plot's plus the six strip
      // rows' — and the payload lists whichever ones ECharts updated, in its
      // own order. They carry the same linked value, but grid 0's is the one
      // whose samples are the curves the readouts price, so ask for it by
      // index rather than taking whatever landed first.
      const xs = (params as AxisPointerEvent).axesInfo?.filter((a) => a.axisDim === 'x')
      const info = xs?.find((a) => a.axisIndex === 0) ?? xs?.[0]
      const value = info?.value
      cursorRef.current?.(typeof value === 'number' ? value : Number(value))
    })
    chart.on('globalout', () => cursorRef.current?.(null))

    const drag = {
      active: false,
      moved: false,
      pointerId: -1,
      startX: 0,
      lastX: 0,
      pricePerPixel: 0,
    }
    let pendingDelta = 0
    let pendingFactor = 1
    let pendingAnchor = 0
    let frame = 0

    // One store update per animation frame: a drag fires pointermove — and a
    // trackpad fires wheel — far faster than React can re-render the option,
    // and every update is a full notMerge rebuild of every series.
    const flush = (): void => {
      frame = 0
      const delta = pendingDelta
      const factor = pendingFactor
      const anchor = pendingAnchor
      pendingDelta = 0
      pendingFactor = 1
      if (delta !== 0) panRef.current?.(delta)
      if (factor !== 1) zoomRef.current?.(factor, anchor)
    }

    const schedule = (): void => {
      if (frame === 0) frame = requestAnimationFrame(flush)
    }

    const endDrag = (): void => {
      if (!drag.active) return
      drag.active = false
      if (frame !== 0) {
        cancelAnimationFrame(frame)
        flush()
      }
      if (drag.moved) setDragging(false)
      drag.moved = false
    }

    const onPointerDown = (event: PointerEvent): void => {
      // Nothing drawn to drag, and pointer capture would retarget the moves
      // zrender needs for the crosshair — so the readout would go dead for
      // the rest of the gesture in exchange for nothing.
      if (event.button !== 0 || !event.isPrimary || !pannableRef.current || !panRef.current) return
      const rect = host.getBoundingClientRect()
      const x = event.clientX - rect.left
      // The pixel→price scale is measured once, here: converting again on
      // every move would read the axis the drag itself is moving, so the
      // grabbed price would slide out from under the cursor.
      const from = priceAtPixel(chart, x)
      const to = priceAtPixel(chart, x + PROBE_PX)
      if (from === null || to === null) return
      drag.active = true
      drag.moved = false
      drag.pointerId = event.pointerId
      drag.startX = event.clientX
      drag.lastX = event.clientX
      drag.pricePerPixel = (to - from) / PROBE_PX
      host.setPointerCapture?.(event.pointerId)
    }

    const onPointerMove = (event: PointerEvent): void => {
      if (!drag.active || event.pointerId !== drag.pointerId) return
      if (!drag.moved) {
        if (Math.abs(event.clientX - drag.startX) < DRAG_THRESHOLD_PX) return
        drag.moved = true
        setDragging(true)
      }
      const dx = event.clientX - drag.lastX
      drag.lastX = event.clientX
      if (dx === 0) return
      // Dragging right pulls the curves right, which moves the window left.
      pendingDelta -= dx * drag.pricePerPixel
      schedule()
    }

    // Registered by hand, non-passive, so preventDefault actually stops the
    // panel underneath from scrolling while the wheel is zooming.
    const onWheel = (event: WheelEvent): void => {
      if (!zoomRef.current) return
      const unit = event.deltaMode === 1 ? 16 : event.deltaMode === 2 ? 400 : 1
      const factor = Math.exp(-event.deltaY * unit * WHEEL_GAIN)
      if (!Number.isFinite(factor) || factor <= 0) return
      // A notch that cannot move the zoom any further — 25% zooming out, 500%
      // zooming in — must not be swallowed: the chart covers most of the
      // scrolling panel, so that would leave the readouts below the fold
      // unreachable by wheel. Let it scroll instead.
      if (factor > 1 ? !zoomInRef.current : !zoomOutRef.current) return
      const rect = host.getBoundingClientRect()
      const anchor = priceAtPixel(chart, event.clientX - rect.left)
      if (anchor === null) return
      event.preventDefault()
      pendingFactor *= factor
      pendingAnchor = anchor
      schedule()
    }

    const onDoubleClick = (): void => resetRef.current?.()

    host.addEventListener('pointerdown', onPointerDown)
    host.addEventListener('pointermove', onPointerMove)
    host.addEventListener('pointerup', endDrag)
    host.addEventListener('pointercancel', endDrag)
    host.addEventListener('lostpointercapture', endDrag)
    host.addEventListener('wheel', onWheel, { passive: false })
    host.addEventListener('dblclick', onDoubleClick)

    const observer = new ResizeObserver(() => chart.resize())
    observer.observe(host)
    return () => {
      if (frame !== 0) cancelAnimationFrame(frame)
      host.removeEventListener('pointerdown', onPointerDown)
      host.removeEventListener('pointermove', onPointerMove)
      host.removeEventListener('pointerup', endDrag)
      host.removeEventListener('pointercancel', endDrag)
      host.removeEventListener('lostpointercapture', endDrag)
      host.removeEventListener('wheel', onWheel)
      host.removeEventListener('dblclick', onDoubleClick)
      observer.disconnect()
      chart.dispose()
      chartRef.current = null
      appliedOptionRef.current = null
    }
  }, [])

  // One effect, so the ordering is guaranteed: a fresh notMerge rebuild of
  // the main option would otherwise race a separately-scheduled overlay
  // effect and could leave the cursor boxes under a rebuild that just wiped
  // their series slots. The rebuild itself only runs when `option` is a new
  // object — an overlay-only change (the common case, once per crosshair
  // tick) applies just the merged overlay.
  useEffect(() => {
    const chart = chartRef.current
    if (!chart) return
    if (appliedOptionRef.current !== option) {
      chart.setOption(option, { notMerge: true, lazyUpdate: false })
      appliedOptionRef.current = option
    }
    if (overlay) chart.setOption(overlay, { notMerge: false, lazyUpdate: true })
  }, [option, overlay])

  useEffect(() => {
    chartRef.current?.resize()
  }, [height])

  return (
    <div
      ref={hostRef}
      role="img"
      aria-label={ariaLabel}
      data-testid="risk-chart"
      style={{
        height: height === 'fill' ? '100%' : height,
        width: '100%',
        cursor: pannable ? (dragging ? 'grabbing' : 'grab') : undefined,
        // Only claim the touch gesture where there is something to pan;
        // with no curves drawn the panel underneath must still scroll under
        // a finger.
        touchAction: pannable ? 'none' : undefined,
      }}
    />
  )
}
