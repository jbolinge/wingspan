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
vi.mock('echarts/charts', () => ({ LineChart: {}, ScatterChart: {} }))
vi.mock('echarts/components', () => ({
  GridComponent: {},
  MarkAreaComponent: {},
  MarkLineComponent: {},
  MarkPointComponent: {},
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

function draw(pannable: boolean): HTMLElement {
  const { container } = render(
    <RiskChart option={{}} height={200} ariaLabel="chart" pannable={pannable} {...handlers} />,
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
    const host = draw(true)
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
   *  it must not be taken where there is nothing to pan. */
  it('does not start a drag while the whole range is shown', async () => {
    const host = draw(false)
    host.dispatchEvent(pointer('pointerdown', 100))
    host.dispatchEvent(pointer('pointermove', 140))
    host.dispatchEvent(pointer('pointerup', 140))
    await new Promise((resolve) => requestAnimationFrame(resolve))
    expect(handlers.onPan).not.toHaveBeenCalled()
  })

  it('zooms about the price under the cursor and swallows the scroll', async () => {
    const host = draw(true)
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
    const host = draw(true)
    host.dispatchEvent(wheel(-100))
    host.dispatchEvent(wheel(-100))
    host.dispatchEvent(wheel(-100))

    await waitFor(() => expect(handlers.onZoom).toHaveBeenCalledTimes(1))
    const single = Math.exp(100 * 0.0015)
    expect((handlers.onZoom.mock.calls[0] as [number, number])[0]).toBeCloseTo(single ** 3, 9)
  })

  /** At 1x a wheel-down cannot change anything, and the chart covers most of
   *  the scrolling panel: that gesture has to reach the scroller. */
  it('lets a zoom-out at 1x scroll the panel instead', async () => {
    const host = draw(false)
    const out = wheel(100)
    host.dispatchEvent(out)
    expect(out.defaultPrevented).toBe(false)

    // Zooming *in* is still a zoom, even at 1x.
    const into = wheel(-100)
    host.dispatchEvent(into)
    expect(into.defaultPrevented).toBe(true)
    await waitFor(() => expect(handlers.onZoom).toHaveBeenCalledTimes(1))
  })

  it('resets the view on a double click', () => {
    const host = draw(true)
    host.dispatchEvent(new MouseEvent('dblclick', { bubbles: true }))
    expect(handlers.onResetView).toHaveBeenCalledTimes(1)
  })

  it('claims the touch gesture only where there is something to pan', () => {
    expect(draw(true).style.touchAction).toBe('none')
    expect(draw(false).style.touchAction).toBe('')
  })
})
