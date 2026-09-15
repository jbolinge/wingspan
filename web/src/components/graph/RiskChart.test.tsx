/** The gestures, not the pixels.
 *
 *  ECharts is stubbed with a chart whose x axis is a straight 1 price unit
 *  per pixel, so a drag of N pixels must move the window by N prices and a
 *  wheel notch must anchor on the price under the cursor. What the option
 *  itself contains is unit-tested in `lib/chartOption.test.ts`. */

import { render, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { RiskChart } from './RiskChart'

const { chart } = vi.hoisted(() => ({
  chart: {
    on: vi.fn(),
    // 1 price unit per pixel, so a delta in pixels reads straight as prices.
    convertFromPixel: vi.fn((_axis: unknown, x: number) => 6000 + x),
    setOption: vi.fn(),
    resize: vi.fn(),
    dispose: vi.fn(),
  },
}))

vi.mock('echarts/core', () => ({ use: vi.fn(), init: vi.fn(() => chart) }))
vi.mock('echarts/charts', () => ({ CustomChart: {}, LineChart: {}, ScatterChart: {} }))
vi.mock('echarts/components', () => ({
  AxisPointerComponent: {},
  GridComponent: {},
  MarkAreaComponent: {},
  MarkLineComponent: {},
  MarkPointComponent: {},
  TitleComponent: {},
  TooltipComponent: {},
}))
vi.mock('echarts/renderers', () => ({ CanvasRenderer: {} }))

/** jsdom has no PointerEvent; the listeners only read the mouse fields. */
function pointer(type: string, clientX: number): Event {
  const event = new MouseEvent(type, { bubbles: true, cancelable: true, clientX, button: 0 })
  Object.assign(event, { pointerId: 1, isPrimary: true })
  return event
}

function wheel(deltaY: number, clientX = 40): WheelEvent {
  return new WheelEvent('wheel', { bubbles: true, cancelable: true, deltaY, clientX })
}

const handlers = {
  onPan: vi.fn(),
  onZoom: vi.fn(),
  onResetView: vi.fn(),
}

function draw(
  props: { pannable?: boolean; canZoomIn?: boolean; canZoomOut?: boolean } = {},
): HTMLElement {
  const { container } = render(
    <RiskChart option={{}} height={200} ariaLabel="chart" {...props} {...handlers} />,
  )
  return container.querySelector('[data-testid="risk-chart"]') as HTMLElement
}

beforeEach(() => {
  handlers.onPan.mockClear()
  handlers.onZoom.mockClear()
  handlers.onResetView.mockClear()
})
afterEach(() => vi.clearAllMocks())

describe('RiskChart gestures', () => {
  it('pans by the price delta the drag covered, one update per frame', async () => {
    const host = draw({ pannable: true })
    host.dispatchEvent(pointer('pointerdown', 100))
    // Under the threshold it is still a click, not a drag.
    host.dispatchEvent(pointer('pointermove', 102))
    expect(handlers.onPan).not.toHaveBeenCalled()

    host.dispatchEvent(pointer('pointermove', 110))
    host.dispatchEvent(pointer('pointermove', 120))
    host.dispatchEvent(pointer('pointerup', 120))

    // Dragging right pulls the curves right, so the window moves left — and
    // the two moves inside one frame arrive as a single store update.
    await waitFor(() => expect(handlers.onPan).toHaveBeenCalledTimes(1))
    expect(handlers.onPan).toHaveBeenCalledWith(-20)
  })

  /** Pointer capture retargets the moves zrender needs for the crosshair, so
   *  it must not be taken where there is nothing drawn to drag. */
  it('does not start a drag with no window to move', async () => {
    const host = draw({ pannable: false })
    host.dispatchEvent(pointer('pointerdown', 100))
    host.dispatchEvent(pointer('pointermove', 140))
    host.dispatchEvent(pointer('pointerup', 140))
    await new Promise((resolve) => requestAnimationFrame(resolve))
    expect(handlers.onPan).not.toHaveBeenCalled()
  })

  it('zooms about the price under the cursor and swallows the scroll', async () => {
    const host = draw({ pannable: true })
    const event = wheel(-100, 40)
    host.dispatchEvent(event)

    await waitFor(() => expect(handlers.onZoom).toHaveBeenCalledTimes(1))
    const [factor, anchor] = handlers.onZoom.mock.calls[0] as [number, number]
    expect(factor).toBeGreaterThan(1)
    expect(anchor).toBe(6040)
    expect(event.defaultPrevented).toBe(true)
  })

  /** A trackpad fires wheel far faster than React can rebuild the option;
   *  a frame's worth of notches must land as one multiplied zoom. */
  it('coalesces a burst of wheel events into one zoom', async () => {
    const host = draw({ pannable: true })
    host.dispatchEvent(wheel(-100))
    host.dispatchEvent(wheel(-100))
    host.dispatchEvent(wheel(-100))

    await waitFor(() => expect(handlers.onZoom).toHaveBeenCalledTimes(1))
    const single = Math.exp(100 * 0.0015)
    expect((handlers.onZoom.mock.calls[0] as [number, number])[0]).toBeCloseTo(single ** 3, 9)
  })

  /** At 25% a wheel-down cannot change anything, and the chart covers most of
   *  the scrolling panel: that gesture has to reach the scroller. */
  it('lets a wheel it cannot act on scroll the panel instead', async () => {
    const host = draw({ pannable: true, canZoomOut: false })
    const out = wheel(100)
    host.dispatchEvent(out)
    expect(out.defaultPrevented).toBe(false)

    // Zooming *in* is still a zoom while there is room for it.
    const into = wheel(-100)
    host.dispatchEvent(into)
    expect(into.defaultPrevented).toBe(true)
    await waitFor(() => expect(handlers.onZoom).toHaveBeenCalledTimes(1))
  })

  it('lets a wheel-in scroll the panel once the zoom is at its top end', async () => {
    const host = draw({ pannable: true, canZoomIn: false })
    const into = wheel(-100)
    host.dispatchEvent(into)
    expect(into.defaultPrevented).toBe(false)

    const out = wheel(100)
    host.dispatchEvent(out)
    expect(out.defaultPrevented).toBe(true)
    await waitFor(() => expect(handlers.onZoom).toHaveBeenCalledTimes(1))
  })

  it('resets the view on a double click', () => {
    const host = draw({ pannable: true })
    host.dispatchEvent(new MouseEvent('dblclick', { bubbles: true }))
    expect(handlers.onResetView).toHaveBeenCalledTimes(1)
  })

  it('claims the touch gesture only where there is something to pan', () => {
    expect(draw({ pannable: true }).style.touchAction).toBe('none')
    expect(draw({ pannable: false }).style.touchAction).toBe('')
  })
})

describe('RiskChart overlay', () => {
  it('applies the main option then the overlay, in that order', () => {
    const option = { series: [] }
    const overlay = { series: [{ id: 'cursor-pnl' }] }
    render(<RiskChart option={option} overlay={overlay} height={200} ariaLabel="chart" />)

    expect(chart.setOption).toHaveBeenCalledTimes(2)
    expect(chart.setOption).toHaveBeenNthCalledWith(1, option, { notMerge: true, lazyUpdate: false })
    expect(chart.setOption).toHaveBeenNthCalledWith(2, overlay, { notMerge: false, lazyUpdate: true })
  })

  it('re-applies the overlay without rebuilding the option when only the overlay changes', () => {
    // A cursor move mints a new overlay far more often than the option
    // itself changes; re-running the notMerge rebuild on every one of those
    // would tear the whole chart down and redraw it per mouse move — see the
    // module comment. The same `option` object across a re-render must
    // therefore produce exactly one further `setOption` call: the overlay.
    const option = { series: [] }
    const overlay1 = { series: [{ id: 'cursor-pnl' }] }
    const { rerender } = render(
      <RiskChart option={option} overlay={overlay1} height={200} ariaLabel="chart" />,
    )
    expect(chart.setOption).toHaveBeenCalledTimes(2)

    const overlay2 = { series: [{ id: 'cursor-pnl' }, { id: 'cursor-delta' }] }
    rerender(<RiskChart option={option} overlay={overlay2} height={200} ariaLabel="chart" />)

    expect(chart.setOption).toHaveBeenCalledTimes(3)
    expect(chart.setOption).toHaveBeenNthCalledWith(3, overlay2, { notMerge: false, lazyUpdate: true })
  })

  it('re-applies both the option and the overlay when the option object actually changes', () => {
    // A fresh notMerge rebuild of the option must never leave the overlay's
    // series slots under a stale cursor box: the main option is re-applied
    // first, then the overlay right after it.
    const option1 = { series: [] }
    const overlay = { series: [{ id: 'cursor-pnl' }] }
    const { rerender } = render(
      <RiskChart option={option1} overlay={overlay} height={200} ariaLabel="chart" />,
    )
    expect(chart.setOption).toHaveBeenCalledTimes(2)

    const option2 = { series: [{ id: 'strip-pnl' }] }
    rerender(<RiskChart option={option2} overlay={overlay} height={200} ariaLabel="chart" />)

    expect(chart.setOption).toHaveBeenCalledTimes(4)
    expect(chart.setOption).toHaveBeenNthCalledWith(3, option2, { notMerge: true, lazyUpdate: false })
    expect(chart.setOption).toHaveBeenNthCalledWith(4, overlay, { notMerge: false, lazyUpdate: true })
  })

  it('applies only the main option when overlay is null', () => {
    render(<RiskChart option={{ series: [] }} overlay={null} height={200} ariaLabel="chart" />)
    expect(chart.setOption).toHaveBeenCalledTimes(1)
  })
})

describe('RiskChart cursor price', () => {
  /** The `updateAxisPointer` listener the chart registered on mount. */
  function cursorHandler(): (params: unknown) => void {
    const call = chart.on.mock.calls.find(([event]) => event === 'updateAxisPointer')
    return call?.[1] as (params: unknown) => void
  }

  it('reads the P&L plot axis, not whichever linked axis came first', () => {
    const onCursorPrice = vi.fn()
    render(
      <RiskChart option={{}} height={200} ariaLabel="chart" onCursorPrice={onCursorPrice} />,
    )
    // The strip rows' axes are linked to grid 0's and ECharts lists them in
    // its own order; only axis 0's samples are the curves the readouts price.
    cursorHandler()({
      axesInfo: [
        { axisDim: 'x', axisIndex: 3, value: 6400 },
        { axisDim: 'y', axisIndex: 0, value: 120 },
        { axisDim: 'x', axisIndex: 0, value: 6512 },
      ],
    })
    expect(onCursorPrice).toHaveBeenCalledWith(6512)
  })

  it('falls back to the first x axis when the payload carries no index', () => {
    const onCursorPrice = vi.fn()
    render(
      <RiskChart option={{}} height={200} ariaLabel="chart" onCursorPrice={onCursorPrice} />,
    )
    cursorHandler()({ axesInfo: [{ axisDim: 'x', value: 6300 }] })
    expect(onCursorPrice).toHaveBeenCalledWith(6300)
  })
})
