import { act, fireEvent, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { call } from '../../api/client'
import { mockRequest } from '../../api/mock'
import type { RiskGraphRequest, TradesResponse } from '../../api/types'
import { MIN_ZOOM } from '../../lib/viewport'
import { useAppStore } from '../../store/appStore'
import { renderWithClient, resetAppStore } from '../../test/utils'
import { RiskGraphPanel } from './RiskGraphPanel'

// ECharts needs a real canvas; the option's *content* is unit-tested
// separately in src/lib/chartOption.test.ts. This mock also records the
// option references it was passed so a test can check *identity* — whether
// an unrelated prop change forced RiskGraphPanel to rebuild it.
// It also keeps the last gesture props it was handed, so the pan/zoom/reset
// handlers — the ones a real drag or wheel calls — can be driven from a test
// without a canvas underneath.
const { riskChartOptions, chartProps } = vi.hoisted(() => ({
  riskChartOptions: [] as unknown[],
  chartProps: { current: null as null | Record<string, unknown> },
}))
vi.mock('./RiskChart', () => ({
  RiskChart: (props: { ariaLabel: string; option: unknown }) => {
    riskChartOptions.push(props.option)
    chartProps.current = props as unknown as Record<string, unknown>
    return <div data-testid="risk-chart" aria-label={props.ariaLabel} />
  },
}))

/** The gesture callbacks RiskChart was last rendered with. */
function gestures(): {
  onPan: (delta: number) => void
  onZoom: (factor: number, anchor: number) => void
  onResetView: () => void
  pannable: boolean
} {
  return chartProps.current as never
}

// Wraps the real implementation so a test can count/fail individual calls
// without losing the mock API responses underneath.
vi.mock('../../api/client', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../api/client')>()
  return { ...actual, call: vi.fn(actual.call) }
})

const trades = (mockRequest('/trades') as TradesResponse).trades

beforeEach(() => resetAppStore())
afterEach(() => {
  vi.mocked(call).mockClear()
  riskChartOptions.length = 0
  chartProps.current = null
})

/** The `risk_graph` requests that actually reached the network. */
function riskGraphRequests(): RiskGraphRequest[] {
  return vi
    .mocked(call)
    .mock.calls.filter((args) => args[0] === 'risk_graph')
    .map((args) => (args[1] as { request: RiskGraphRequest }).request)
}

/** Those of them that ask for an absolute window: the *view* query's. */
function windowRequests(): RiskGraphRequest[] {
  return riskGraphRequests().filter((r) => r.price_range !== null && 'min' in r.price_range)
}

function riskGraphCallCount(): number {
  return riskGraphRequests().length
}

/** The value beside a header readout's label. */
function readoutValue(label: string): string {
  return screen.getByText(label).nextElementSibling?.textContent ?? ''
}

/** Every x the option's series carry — how far the *data* actually reaches. */
function plottedPrices(option: unknown): number[] {
  const series = (option as { series: { data?: [number, number][] }[] }).series
  return series.flatMap((s) => (Array.isArray(s.data) ? s.data.map(([x]) => x) : []))
}

/** Lets the 250 ms request debounce fire (or prove that it had nothing to fire). */
const settle = (): Promise<void> => new Promise((resolve) => setTimeout(resolve, 400))

/** The plotted price range of the option the chart was last handed. */
function xAxisOf(option: unknown): { min: number; max: number } {
  return (option as { xAxis: { min: number; max: number } }).xAxis
}

/** The y bounds of that option — absent while ECharts is left to scale it. */
function yAxisOf(option: unknown): { min?: number; max?: number } {
  return (option as { yAxis: { min?: number; max?: number } }).yAxis
}

/** The window currently drawn. */
function plotted(): { min: number; max: number } {
  return xAxisOf(riskChartOptions.at(-1))
}

describe('RiskGraphPanel', () => {
  it('asks for a selection before drawing anything', () => {
    renderWithClient(<RiskGraphPanel trades={trades} />)
    expect(screen.getByText(/Select trades in the table/)).toBeInTheDocument()
    expect(screen.queryByTestId('risk-chart')).not.toBeInTheDocument()
  })

  it('draws the expiration line plus the T+x ladder for the selected set', async () => {
    useAppStore.getState().setSelected([trades[1]!.id])
    renderWithClient(<RiskGraphPanel trades={trades} />)
    const chart = await screen.findByTestId('risk-chart')
    expect(chart.getAttribute('aria-label')).toMatch(/expiration line plus \d+ T\+x lines/)
  })

  it('reports max profit, max loss, PoP and the breakevens', async () => {
    useAppStore.getState().setSelected([trades[1]!.id])
    renderWithClient(<RiskGraphPanel trades={trades} />)
    expect(await screen.findByText('max profit')).toBeInTheDocument()
    expect(screen.getByText('max loss')).toBeInTheDocument()
    expect(screen.getByText('PoP')).toBeInTheDocument()
    expect(screen.getByText('ATM IV')).toBeInTheDocument()
  })

  it('reads out every line at spot before the cursor has moved', async () => {
    useAppStore.getState().setSelected([trades[1]!.id])
    renderWithClient(<RiskGraphPanel trades={trades} />)
    expect(await screen.findByText('At spot')).toBeInTheDocument()
    expect(screen.getAllByText('T+0').length).toBeGreaterThan(0)
  })

  it('exposes the per-expiry vol sliders of the selected set', async () => {
    const calendar = trades.find((t) => t.id === 'jan-calendar-2026-08-20')!
    useAppStore.getState().setSelected([calendar.id])
    renderWithClient(<RiskGraphPanel trades={trades} />)
    expect(await screen.findByLabelText('global')).toBeInTheDocument()
    expect(screen.getByLabelText('DEC26')).toBeInTheDocument()
    expect(screen.getByLabelText('JAN27')).toBeInTheDocument()
  })

  it('records a per-expiry vol shift in the store, keyed by expiry', async () => {
    const calendar = trades.find((t) => t.id === 'jan-calendar-2026-08-20')!
    useAppStore.getState().setSelected([calendar.id])
    renderWithClient(<RiskGraphPanel trades={trades} />)
    fireEvent.change(await screen.findByLabelText('DEC26'), { target: { value: '0.02' } })
    await waitFor(() =>
      expect(useAppStore.getState().graph.volShiftByExpiry).toEqual({ '2026-12-18': 0.02 }),
    )
  })

  it('toggles include-realised, which changes the request the server sees', async () => {
    const user = userEvent.setup()
    useAppStore.getState().setSelected([trades[1]!.id])
    renderWithClient(<RiskGraphPanel trades={trades} />)
    await screen.findByTestId('risk-chart')
    await user.click(screen.getByRole('button', { name: 'include realised' }))
    expect(useAppStore.getState().graph.includeRealized).toBe(false)
  })

  it('hides a curve when its line toggle is unticked', async () => {
    const user = userEvent.setup()
    useAppStore.getState().setSelected([trades[1]!.id])
    renderWithClient(<RiskGraphPanel trades={trades} />)
    const toggle = (await screen.findAllByRole('checkbox')).at(0) as HTMLInputElement
    await user.click(toggle)
    expect(useAppStore.getState().graph.hiddenLines).toHaveLength(1)
  })

  /** The chart, the crosshair readout and the slice table must agree on which
   *  curves are switched on — the readout used to keep listing hidden lines,
   *  labelled by their raw ISO date. */
  it('drops a hidden line from the crosshair readout, not just from the chart', async () => {
    const user = userEvent.setup()
    useAppStore.getState().setSelected([trades[1]!.id])
    renderWithClient(<RiskGraphPanel trades={trades} />)
    await screen.findByTestId('risk-chart')

    const bodyRows = (): HTMLElement[] => {
      const card = screen.getByText('At spot').closest('.ws-card') as HTMLElement
      return [...card.querySelectorAll('tbody tr')] as HTMLElement[]
    }

    const before = bodyRows().length
    expect(bodyRows().some((row) => row.textContent?.includes('T+0'))).toBe(true)

    await user.click(screen.getByRole('checkbox', { name: 'Show T+0' }))
    await waitFor(() => expect(bodyRows().length).toBe(before - 1))
    expect(bodyRows().some((row) => row.textContent?.includes('T+0'))).toBe(false)
    // No row falls back to a raw ISO date because its label lookup missed.
    expect(bodyRows().some((row) => /\d{4}-\d{2}-\d{2}/.test(row.textContent ?? ''))).toBe(false)
  })

  it('locks the y-axis to the current extent', async () => {
    const user = userEvent.setup()
    useAppStore.getState().setSelected([trades[1]!.id])
    renderWithClient(<RiskGraphPanel trades={trades} />)
    await screen.findByTestId('risk-chart')
    await user.click(screen.getByRole('button', { name: 'lock y-axis' }))
    const graph = useAppStore.getState().graph
    expect(graph.yLock).toBe(true)
    expect(graph.yMin).toBeLessThan(graph.yMax ?? 0)
  })

  it('shows the price-slice table on demand', async () => {
    const user = userEvent.setup()
    useAppStore.getState().setSelected([trades[1]!.id])
    renderWithClient(<RiskGraphPanel trades={trades} />)
    await screen.findByTestId('risk-chart')
    await user.click(screen.getByRole('button', { name: 'price slices' }))
    expect(await screen.findByText('Price slices')).toBeInTheDocument()
    expect(screen.getByText('vs spot')).toBeInTheDocument()
  })

  it('computes the what-if answer from the server, not from the chart', async () => {
    const user = userEvent.setup()
    useAppStore.getState().setSelected([trades[1]!.id])
    renderWithClient(<RiskGraphPanel trades={trades} />)
    await screen.findByTestId('risk-chart')
    await user.click(screen.getByRole('button', { name: /what-if off/ }))
    expect(await screen.findByLabelText('What-if price')).toBeInTheDocument()
    await waitFor(() => expect(screen.getByText(/vs now/)).toBeInTheDocument())
  })

  /** The what-if panel sits inches from the T+0 curve, so it must answer on
   *  the same basis: with realised switched off both show open-only P&L, and
   *  the "vs now" baseline moves with them. */
  it('answers the what-if on the same basis as the curves', async () => {
    const user = userEvent.setup()
    const trade = trades.find((t) => t.realized !== 0)!
    useAppStore.getState().setSelected([trade.id])
    renderWithClient(<RiskGraphPanel trades={trades} />)
    await screen.findByTestId('risk-chart')
    await user.click(screen.getByRole('button', { name: /what-if off/ }))
    await screen.findByLabelText('What-if price')

    // The P&L and the "(… vs now)" delta are the two <span>s after the label.
    const cells = (): number[] => {
      const row = screen.getByText(/vs now/).parentElement as HTMLElement
      return [...row.querySelectorAll('span')]
        .slice(1)
        .map((el) => Number((el.textContent ?? '').replace(/[^0-9.-]/g, '')))
    }

    await waitFor(() => expect(Math.abs(cells()[0]! - trade.pnl_total)).toBeLessThan(2))
    expect(Math.abs(cells()[1]!)).toBeLessThan(2)

    await user.click(screen.getByRole('button', { name: 'include realised' }))
    await waitFor(() => expect(Math.abs(cells()[0]! - trade.pnl_open)).toBeLessThan(2))
    // Baseline follows the toggle too, so at spot/today the delta stays ~0.
    expect(Math.abs(cells()[1]!)).toBeLessThan(2)
  })

  it('lets the date list be edited away from the server preset', async () => {
    const user = userEvent.setup()
    useAppStore.getState().setSelected([trades[1]!.id])
    renderWithClient(<RiskGraphPanel trades={trades} />)
    await screen.findByTestId('risk-chart')
    expect(screen.getByText(/Server preset in use/)).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'edit preset' }))
    await waitFor(() => expect(useAppStore.getState().graph.dates).not.toBeNull())
    expect(screen.getAllByText('T+0').length).toBeGreaterThan(0)
  })

  /** A native range input fires onChange on every drag tick, not just on
   *  release — without a debounce this fired one full /risk-graph POST per
   *  tick. */
  it('collapses a burst of slider ticks into one settled request', async () => {
    useAppStore.getState().setSelected([trades[1]!.id])
    renderWithClient(<RiskGraphPanel trades={trades} />)
    await screen.findByTestId('risk-chart')
    const before = riskGraphCallCount()

    const globalSlider = screen.getByLabelText('global')
    for (const value of ['0.01', '0.02', '0.03', '0.04', '0.05']) {
      fireEvent.change(globalSlider, { target: { value } })
    }

    // Right after the burst, none of the intermediate ticks should have
    // reached the network — only the settled value does, once the debounce
    // window elapses.
    expect(riskGraphCallCount()).toBe(before)

    await waitFor(() => expect(riskGraphCallCount()).toBe(before + 1), { timeout: 2000 })
  })

  /** The slider must reflect whatever zoom the store already holds — a
   *  persisted view, or one seeded straight into the store by a test —
   *  not silently reset to 100% on mount. */
  it('renders the zoom slider at the store\'s xZoom on first paint', async () => {
    useAppStore.getState().setSelected([trades[1]!.id])
    useAppStore.setState((s) => ({ graph: { ...s.graph, xZoom: 4, xCenter: null } }))
    renderWithClient(<RiskGraphPanel trades={trades} />)
    await screen.findByTestId('risk-chart')

    // log2 scale: xZoom 4 is slider value 2, and the readout spells it "400%".
    expect(screen.getByLabelText('zoom')).toHaveValue('2')
    expect(screen.getByText('400%')).toBeInTheDocument()
  })

  it('narrows the price axis to the zoom window', async () => {
    useAppStore.getState().setSelected([trades[1]!.id])
    renderWithClient(<RiskGraphPanel trades={trades} />)
    await screen.findByTestId('risk-chart')
    const full = xAxisOf(riskChartOptions.at(-1))

    // log2 scale: 1 on the slider is 200%.
    fireEvent.change(screen.getByLabelText('zoom'), { target: { value: '1' } })

    await waitFor(() => expect(useAppStore.getState().graph.xZoom).toBe(2))
    const zoomed = xAxisOf(riskChartOptions.at(-1))
    expect(zoomed.max - zoomed.min).toBeCloseTo((full.max - full.min) / 2, 6)
  })

  /** The base query is the whole story until the trader moves the view: no
   *  second request, and the option stays the pre-zoom one. */
  it('asks only for the computed range while the default view is on screen', async () => {
    useAppStore.getState().setSelected([trades[1]!.id])
    renderWithClient(<RiskGraphPanel trades={trades} />)
    await screen.findByTestId('risk-chart')
    await settle()

    expect(riskGraphCallCount()).toBeGreaterThan(0)
    expect(windowRequests()).toHaveLength(0)
  })

  /** Panned, the request follows the window: an absolute price range around
   *  it, sampled at the base grid's own density. */
  it('asks the server to cover the window once the view has been panned', async () => {
    useAppStore.getState().setSelected([trades[1]!.id])
    renderWithClient(<RiskGraphPanel trades={trades} />)
    await screen.findByTestId('risk-chart')
    const full = plotted()

    act(() => gestures().onPan((full.max - full.min) * 1.5))
    await waitFor(() => expect(windowRequests().length).toBeGreaterThan(0), { timeout: 2000 })

    const request = windowRequests().at(-1)!
    const range = request.price_range as { min: number; max: number }
    expect(range.min).toBeGreaterThan(0)
    expect(range.max).toBeGreaterThan(plotted().max)
    expect(range.min).toBeLessThan(plotted().min)
    // Same sample density as the base grid, which is 241 points wide.
    expect(request.points).toBeGreaterThan(241)
    expect(request.points).toBeLessThanOrEqual(2001)
  })

  /** The header answers for the price range the range mode computes — the
   *  base range — so it must not wander while the chart follows the eye. */
  it('keeps the header on the base numbers while the chart draws the view', async () => {
    useAppStore.getState().setSelected([trades[1]!.id])
    renderWithClient(<RiskGraphPanel trades={trades} />)
    await screen.findByTestId('risk-chart')
    const full = plotted()
    const before = {
      maxProfit: readoutValue('max profit'),
      maxLoss: readoutValue('max loss'),
      pop: readoutValue('PoP'),
      be: readoutValue('BE'),
    }

    act(() => gestures().onPan((full.max - full.min) * 1.5))
    // The view response has landed once the curves reach past the base range.
    await waitFor(
      () => expect(Math.max(...plottedPrices(riskChartOptions.at(-1)))).toBeGreaterThan(full.max),
      { timeout: 2000 },
    )

    expect(readoutValue('max profit')).toBe(before.maxProfit)
    expect(readoutValue('max loss')).toBe(before.maxLoss)
    expect(readoutValue('PoP')).toBe(before.pop)
    expect(readoutValue('BE')).toBe(before.be)
  })

  it('restores the whole computed range from the reset-view button', async () => {
    const user = userEvent.setup()
    useAppStore.getState().setSelected([trades[1]!.id])
    renderWithClient(<RiskGraphPanel trades={trades} />)
    await screen.findByTestId('risk-chart')
    const full = xAxisOf(riskChartOptions.at(-1))

    fireEvent.change(screen.getByLabelText('zoom'), { target: { value: '2' } })
    await waitFor(() => expect(useAppStore.getState().graph.xZoom).toBe(4))

    await user.click(screen.getByRole('button', { name: 'reset view' }))
    expect(useAppStore.getState().graph).toMatchObject({ xZoom: 1, xCenter: null })
    const restored = xAxisOf(riskChartOptions.at(-1))
    expect([restored.min, restored.max]).toEqual([full.min, full.max])
  })


  /** At the default view the window *is* the computed range, so the option
   *  must be exactly what it was before zooming existed: pinning the y axis
   *  to a padded bound costs the default view ECharts' own rounded scale. */
  it('leaves the y axis to ECharts by default, and pins it only once zoomed', async () => {
    useAppStore.getState().setSelected([trades[1]!.id])
    renderWithClient(<RiskGraphPanel trades={trades} />)
    await screen.findByTestId('risk-chart')
    expect(yAxisOf(riskChartOptions.at(-1))).not.toHaveProperty('min')

    fireEvent.change(screen.getByLabelText('zoom'), { target: { value: '2' } })
    await waitFor(() => expect(yAxisOf(riskChartOptions.at(-1)).min).toBeTypeOf('number'))
  })

  /** ONE parity: the window is no longer pinned inside the computed range, so
   *  a drag at 100% moves it — same width, new prices. */
  it('pans at 100%, where the window used to be pinned', async () => {
    useAppStore.getState().setSelected([trades[1]!.id])
    renderWithClient(<RiskGraphPanel trades={trades} />)
    await screen.findByTestId('risk-chart')
    const full = plotted()

    expect(gestures().pannable).toBe(true)
    act(() => gestures().onPan(120))

    await waitFor(() => expect(plotted().min).toBeCloseTo(full.min + 120, 6))
    expect(plotted().max - plotted().min).toBeCloseTo(full.max - full.min, 6)
    expect(useAppStore.getState().graph.xCenter).not.toBeNull()
    expect(useAppStore.getState().graph.xZoom).toBe(1)
  })

  /** A drag moves the window by the price delta it is handed, off the ends of
   *  the computed range and all — the only floor is price 0. */
  it('pans the zoomed window past the computed range, floored at zero', async () => {
    useAppStore.getState().setSelected([trades[1]!.id])
    renderWithClient(<RiskGraphPanel trades={trades} />)
    await screen.findByTestId('risk-chart')
    const full = plotted()

    fireEvent.change(screen.getByLabelText('zoom'), { target: { value: '1' } })
    await waitFor(() => expect(useAppStore.getState().graph.xZoom).toBe(2))
    const before = plotted()
    expect(gestures().pannable).toBe(true)

    const step = (before.max - before.min) / 8
    act(() => gestures().onPan(step))
    await waitFor(() => expect(plotted().min).toBeCloseTo(before.min + step, 6))
    expect(plotted().max - plotted().min).toBeCloseTo(before.max - before.min, 6)

    // Right off the top of the computed range: nothing pulls it back.
    act(() => gestures().onPan(full.max - full.min))
    await waitFor(() => expect(plotted().min).toBeGreaterThan(full.max))

    // A drag towards zero stops flush against it, still a full window wide,
    // never inverted, empty or negative.
    act(() => gestures().onPan(-1e6))
    await waitFor(() => expect(plotted().min).toBe(0))
    expect(plotted().max - plotted().min).toBeCloseTo(before.max - before.min, 6)
  })

  /** The wheel zooms about the price under the cursor, and lands on the
   *  slider's own grid so the thumb, the readout and the store agree. */
  it('keeps the price under the wheel put, on the slider grid', async () => {
    useAppStore.getState().setSelected([trades[1]!.id])
    renderWithClient(<RiskGraphPanel trades={trades} />)
    await screen.findByTestId('risk-chart')

    fireEvent.change(screen.getByLabelText('zoom'), { target: { value: '1' } })
    await waitFor(() => expect(useAppStore.getState().graph.xZoom).toBe(2))
    const before = plotted()
    const anchor = before.min + (before.max - before.min) * 0.25

    act(() => gestures().onZoom(1.4123, anchor))
    await waitFor(() => expect(useAppStore.getState().graph.xZoom).toBeGreaterThan(2))

    const after = plotted()
    expect((anchor - after.min) / (after.max - after.min)).toBeCloseTo(0.25, 6)
    // On the grid: the slider's own value round-trips to the stored zoom.
    const slider = screen.getByLabelText('zoom') as HTMLInputElement
    expect(2 ** Number(slider.value)).toBeCloseTo(useAppStore.getState().graph.xZoom, 9)
  })

  /** 100% is the *default*, not the floor: the wheel keeps going out to 25%,
   *  and stops there rather than wrapping or drifting. */
  it('wheels out past 100% and bottoms out at 25%', async () => {
    useAppStore.getState().setSelected([trades[1]!.id])
    renderWithClient(<RiskGraphPanel trades={trades} />)
    await screen.findByTestId('risk-chart')
    fireEvent.change(screen.getByLabelText('zoom'), { target: { value: '2' } })
    await waitFor(() => expect(useAppStore.getState().graph.xZoom).toBe(4))
    const full = plotted()

    act(() => gestures().onZoom(0.05, plotted().min))
    await waitFor(() => expect(useAppStore.getState().graph.xZoom).toBe(MIN_ZOOM))
    // A quarter zoom is four times the base range on screen.
    await waitFor(() =>
      expect(plotted().max - plotted().min).toBeGreaterThan((full.max - full.min) * 4),
    )

    // Another notch out changes nothing at all, including the store.
    const centre = useAppStore.getState().graph.xCenter
    act(() => gestures().onZoom(0.05, plotted().min))
    expect(useAppStore.getState().graph.xZoom).toBe(MIN_ZOOM)
    expect(useAppStore.getState().graph.xCenter).toBe(centre)
  })

  it('double-click (onResetView) puts the whole range back', async () => {
    useAppStore.getState().setSelected([trades[1]!.id])
    renderWithClient(<RiskGraphPanel trades={trades} />)
    await screen.findByTestId('risk-chart')
    const full = plotted()
    fireEvent.change(screen.getByLabelText('zoom'), { target: { value: '2' } })
    await waitFor(() => expect(useAppStore.getState().graph.xZoom).toBe(4))

    act(() => gestures().onResetView())
    await waitFor(() => expect([plotted().min, plotted().max]).toEqual([full.min, full.max]))
  })

  /** A centre is a price on one particular grid: kept across a selection
   *  change it pins the view to an edge sliver of a different position. */
  it('drops a stale centre when the selection changes, keeping the zoom', async () => {
    useAppStore.getState().setSelected([trades[1]!.id])
    renderWithClient(<RiskGraphPanel trades={trades} />)
    await screen.findByTestId('risk-chart')
    fireEvent.change(screen.getByLabelText('zoom'), { target: { value: '2' } })
    await waitFor(() => expect(useAppStore.getState().graph.xCenter).not.toBeNull())

    act(() => useAppStore.getState().setSelected([trades[0]!.id]))
    await waitFor(() => expect(useAppStore.getState().graph.xCenter).toBeNull())
    expect(useAppStore.getState().graph.xZoom).toBe(4)
  })

  /** 'lock y-axis' means "hold what I am looking at": zoomed in, that is the
   *  extent of the visible slice, not of the whole computed range. */
  it('freezes the visible extent when the axis is locked while zoomed', async () => {
    const user = userEvent.setup()
    useAppStore.getState().setSelected([trades[1]!.id])
    renderWithClient(<RiskGraphPanel trades={trades} />)
    await screen.findByTestId('risk-chart')

    // The slider's top end: 500%, the most it now zooms in.
    fireEvent.change(screen.getByLabelText('zoom'), { target: { value: '3' } })
    await waitFor(() => expect(useAppStore.getState().graph.xZoom).toBe(5))
    const onScreen = yAxisOf(riskChartOptions.at(-1))

    await user.click(screen.getByRole('button', { name: 'lock y-axis' }))
    const graph = useAppStore.getState().graph
    expect(graph.yLock).toBe(true)
    expect(graph.yMin).toBeCloseTo(onScreen.min!, 6)
    expect(graph.yMax).toBeCloseTo(onScreen.max!, 6)
    // The axis does not jump when it is frozen.
    expect(yAxisOf(riskChartOptions.at(-1)).min).toBeCloseTo(onScreen.min!, 6)
  })

  /** `trades` (App's `openTrades`) is a new array reference on every 30s
   *  poll tick; that used to force a full chart-option rebuild — an ECharts
   *  teardown — even with the per-trade overlay off and nothing about the
   *  selected graph having changed. */
  it('does not rebuild the chart option on an unrelated trades prop change', async () => {
    useAppStore.getState().setSelected([trades[1]!.id])
    const { rerender } = renderWithClient(<RiskGraphPanel trades={trades} />)
    await screen.findByTestId('risk-chart')
    const before = riskChartOptions.at(-1)

    // Same content, a fresh reference — what a trades poll resolving hands
    // down as `openTrades`.
    rerender(<RiskGraphPanel trades={trades.map((t) => ({ ...t }))} />)

    expect(riskChartOptions.at(-1)).toBe(before)
  })

  it('surfaces the response warnings, keyed to the selected trades', async () => {
    useAppStore.getState().setSelected(['jan-calendar-2026-08-20'])
    renderWithClient(<RiskGraphPanel trades={trades} />)
    await screen.findByTestId('risk-chart')
    expect(await screen.findByText('⚠ 1')).toBeInTheDocument()
    expect(screen.getByTitle(/1 warning behind these curves/)).toBeInTheDocument()
  })

  it('keeps the last-drawn curves on screen through a background refetch failure', async () => {
    useAppStore.getState().setSelected([trades[1]!.id])
    const { client } = renderWithClient(<RiskGraphPanel trades={trades} />)
    await screen.findByTestId('risk-chart')

    vi.mocked(call).mockRejectedValueOnce(new Error('boom'))
    await client.refetchQueries({ queryKey: ['risk-graph'] })

    expect(await screen.findByText('Request failed.')).toBeInTheDocument()
    // The stale-but-valid curve is still on screen, not replaced by the error.
    expect(screen.getByTestId('risk-chart')).toBeInTheDocument()
  })
})
