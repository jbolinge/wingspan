/** The ONE-style risk graph: chart, crosshair readout, slices and controls. */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { usePortfolio, useRiskGraph } from '../../api/queries'
import type { PortfolioStats, Trade } from '../../api/types'
import { aggregateTrades, expiriesOf, tradesById } from '../../lib/aggregate'
import { buildRiskChartOption, perTradeOverlayCurves } from '../../lib/chartOption'
import { daysBetween, todayIso } from '../../lib/dates'
import { fmtNumber, fmtPnl, fmtRatioPct, pnlColorClass } from '../../lib/format'
import { buildRiskGraphRequest } from '../../lib/graphRequest'
import { chartPalette } from '../../lib/palette'
import { indexAt } from '../../lib/readout'
import { useDebouncedValue } from '../../lib/useDebouncedValue'
import {
  baseCovers,
  clampZoom,
  coverageFor,
  DEFAULT_ZOOM,
  formatZoom,
  isDefaultView,
  MAX_ZOOM,
  MIN_ZOOM,
  MIN_ZOOM_LOG2,
  priceRange,
  SLIDER_MAX_LOG2,
  snapZoom,
  xWindow,
  ZOOM_STEP_LOG2,
  zoomAt,
  zoomFromSlider,
  zoomToSlider,
} from '../../lib/viewport'
import { useAppStore } from '../../store/appStore'
import { EmptyState, ErrorNote, Pill, Slider, Spinner, ToggleButton, Tooltip } from '../ui'
import { CrosshairReadout } from './CrosshairReadout'
import { GraphControls } from './GraphControls'
import { RiskChart } from './RiskChart'
import { SliceTable } from './SliceTable'

function Readout({
  label,
  value,
  tone,
  tip,
}: {
  label: string
  value: string
  tone?: string
  tip?: string
}) {
  return (
    <Tooltip content={tip ?? label} align="center">
      <span className="flex cursor-help items-baseline gap-1">
        <span className="ws-label">{label}</span>
        <span className={`num text-[12px] font-semibold ${tone ?? 'text-fg'}`}>{value}</span>
      </span>
    </Tooltip>
  )
}

/** Notches on the zoom slider: one octave either side of home. */
const ZOOM_MARKS = [0.5, 1, 2].map((zoom) => ({ value: zoomToSlider(zoom), label: formatZoom(zoom) }))

export function RiskGraphPanel({ trades }: { trades: Trade[] }) {
  const selected = useAppStore((s) => s.selected)
  const graph = useAppStore((s) => s.graph)
  const theme = useAppStore((s) => s.theme)
  const graphMaximized = useAppStore((s) => s.graphMaximized)
  const setGraphMaximized = useAppStore((s) => s.setGraphMaximized)
  const patchGraph = useAppStore((s) => s.patchGraph)
  const [cursorPrice, setCursorPrice] = useState<number | null>(null)

  // The BASE query: the range mode's own request, untouched by the view. Its
  // answer defines the base range and every header readout, so those stay put
  // while the trader pans — and it is always kept alive, because the window
  // is measured against it.
  const request = useMemo(() => buildRiskGraphRequest(selected, graph), [selected, graph])
  // A vol/range slider fires `request` on every drag tick; only firing the
  // POST once it holds still keeps a drag from queuing dozens of full
  // pricing-grid computations, and useRiskGraph forwards an AbortSignal so a
  // request superseded before it settles is cancelled outright.
  const debouncedRequest = useDebouncedValue(request, 250)
  const baseQuery = useRiskGraph(debouncedRequest, true)
  const base = baseQuery.data

  const selectedTrades = tradesById(trades, selected)
  const remoteStats = usePortfolio(selected)
  const stats: PortfolioStats = remoteStats.data ?? aggregateTrades(selectedTrades)

  const expiries = expiriesOf(selectedTrades)
  const front = expiries[0] ?? null
  const frontDte = front ? daysBetween(todayIso(), front) : null

  // Read once, clamped: a corrupted persisted blob (xZoom 0, or a negative) —
  // or one written by 0.2.6, whose scale ran to 16× — must not reach the
  // slider as log2(0) while the chart itself, which clamps on the way in, is
  // drawing something else.
  const zoom = clampZoom(graph.xZoom)
  // Until the trader zooms or pans there is nothing the base response does
  // not already answer, so the second query does not exist and the option is
  // exactly what it was before any of this.
  const viewDiffers = zoom !== DEFAULT_ZOOM || graph.xCenter !== null

  const baseRange = useMemo(() => (base ? priceRange(base.prices) : null), [base])
  // The visible slice of the price axis, in absolute prices. It may sit
  // outside the base range entirely — panning is not clamped to it.
  const view = useMemo(
    () => (base ? xWindow(base.prices, zoom, graph.xCenter) : null),
    [base, zoom, graph.xCenter],
  )

  // The VIEW query: the same request with the tile the window falls in as an
  // absolute price range, sampled at the base grid's density. Tiling is what
  // keeps a drag from firing a request per frame — `coverageFor` answers the
  // same range until the window has left it — and the 250 ms debounce catches
  // the rest.
  const viewRequest = useMemo(() => {
    if (!viewDiffers || !base || !baseRange || !view) return null
    // Zoomed in without panning, the window is inside the base range and the
    // base response already holds every price in it at the density a coverage
    // request would ask for: no second pricing grid for a screenful that
    // would look identical.
    if (baseCovers(baseRange, view)) return null
    const coverage = coverageFor(view, baseRange, base.prices.length)
    if (!coverage) return null
    return {
      ...request,
      price_range: { min: coverage.min, max: coverage.max },
      points: coverage.points,
    }
  }, [base, baseRange, request, view, viewDiffers])
  const debouncedViewRequest = useDebouncedValue(viewRequest, 250)
  // `windowOnly`: a view response is drawn *under the base response's header
  // numbers*, so a placeholder left over from another selection (or another
  // vol shift, or another T+x list) would put one position's curves beneath
  // another's readouts for as long as the debounce and the request take.
  const viewQuery = useRiskGraph(debouncedViewRequest, debouncedViewRequest !== null, true)

  // What is drawn: the view response wherever there is one, the base response
  // otherwise. A pan past the fetched coverage leaves the previous view data
  // on screen until the next one lands — ECharts simply draws nothing beyond
  // the samples it has, which is the honest picture of a curve still loading.
  const data = viewRequest !== null && viewQuery.data ? viewQuery.data : base

  // The base query is the one that can leave the panel with nothing to draw;
  // a failing view query is a note over curves that are still good.
  const failing = baseQuery.isError ? baseQuery : viewQuery.isError ? viewQuery : null

  const visibleLines = useMemo(
    () => (data?.lines ?? []).filter((line) => !graph.hiddenLines.includes(line.key)),
    [data, graph.hiddenLines],
  )

  // `trades` (App's openTrades) is a new array reference every 30s poll tick,
  // even when nothing the overlay draws has changed. Depending on it directly
  // would rebuild the whole chart option — a full ECharts teardown — on every
  // tick; this key is stable unless the id/name pairs the overlay actually
  // shows have changed.
  const overlayTradeIds = graph.perTradeOverlay ? perTradeOverlayCurves(data).map((s) => s.trade_id) : []
  const overlayNameKey = overlayTradeIds
    .map((id) => `${id}:${trades.find((t) => t.id === id)?.name ?? id}`)
    .join('|')

  const perTradeSeries = useMemo(() => {
    if (!graph.perTradeOverlay) return []
    return perTradeOverlayCurves(data).map((series) => ({
      tradeId: series.trade_id,
      name: trades.find((t) => t.id === series.trade_id)?.name ?? series.trade_id,
      pnl: series.pnl,
    }))
    // eslint-disable-next-line react-hooks/exhaustive-deps -- see overlayNameKey above
  }, [data, graph.perTradeOverlay, overlayNameKey])

  // A centre is a price on *this* price grid. When the grid itself moves —
  // another selection, another range mode, a different point count — a
  // centre left over from the old one clings to whichever end of the new
  // range is nearest, so the trader lands on an edge sliver of a position
  // they were not looking at. Keep the zoom, drop the centre.
  const gridKey = `${selected.join(',')}|${graph.rangeMode}|${graph.rangePct}|${graph.rangeMin}|${graph.rangeMax}|${graph.points}|${graph.spotOverride}`
  const lastGridKey = useRef(gridKey)
  useEffect(() => {
    if (lastGridKey.current === gridKey) return
    lastGridKey.current = gridKey
    if (useAppStore.getState().graph.xCenter !== null) patchGraph({ xCenter: null })
  }, [gridKey, patchGraph])

  const resetView = useCallback(() => {
    // patchGraph always writes a new `graph` object, which rebuilds the whole
    // chart option and touches localStorage; a double-click on a view that is
    // already the default would pay all of that for nothing.
    if (graph.xZoom === DEFAULT_ZOOM && graph.xCenter === null) return
    patchGraph({ xZoom: DEFAULT_ZOOM, xCenter: null })
  }, [graph.xCenter, graph.xZoom, patchGraph])

  // Every handler funnels its new centre back through `xWindow`, which is the
  // one place that keeps the window off the negative half of the price axis.
  const centreOn = useCallback(
    (nextZoom: number, centre: number): void => {
      if (!base) return
      const next = xWindow(base.prices, nextZoom, centre)
      const zoomNext = clampZoom(nextZoom)
      const xCenter = next ? (next.min + next.max) / 2 : centre
      // Landing back on 100% over the base midpoint — the ordinary "slide to
      // 200%, slide back" — is the default view, and has to be stored as one:
      // a centre there would keep the view query and the pinned y axis alive
      // for a window byte-identical to the base range, and leave `reset view`
      // lit with nothing to reset.
      if (baseRange && isDefaultView(baseRange, zoomNext, xCenter)) {
        resetView()
        return
      }
      patchGraph({ xZoom: zoomNext, xCenter })
    },
    [base, baseRange, patchGraph, resetView],
  )

  const handlePan = useCallback(
    (deltaPrice: number) => {
      // Panning is allowed at every zoom, 100% and 25% included: the window
      // is no longer held inside the computed range, it takes the request
      // with it.
      if (!view || !Number.isFinite(deltaPrice)) return
      centreOn(zoom, (view.min + view.max) / 2 + deltaPrice)
    },
    [centreOn, view, zoom],
  )

  const handleZoom = useCallback(
    (factor: number, anchorPrice: number) => {
      if (!view) return
      // Snap the target to the slider's own grid *before* zooming, so the
      // anchor stays exactly put and the thumb, the readout and the store all
      // hold the same number after a wheel notch.
      const target = snapZoom(zoom * factor)
      // Already against a limit: nothing would move, and patching the store
      // would rebuild the whole option for it.
      if (target === zoom) return
      const next = zoomAt(zoom, target / zoom, anchorPrice, view)
      centreOn(next.xZoom, next.xCenter)
    },
    [centreOn, view, zoom],
  )

  const handleSliderZoom = useCallback(
    (log2Zoom: number) => {
      if (!baseRange) return
      // The slider zooms about the window's own centre, so the price being
      // studied stays in view; with no centre yet that is the midpoint of the
      // base range, which keeps 100% exactly the computed range — and a slide
      // back to it is recognised by `centreOn` as the default view.
      centreOn(zoomFromSlider(log2Zoom), graph.xCenter ?? (baseRange.min + baseRange.max) / 2)
    },
    [baseRange, centreOn, graph.xCenter],
  )

  const option = useMemo(() => {
    if (!data) return null
    return buildRiskChartOption({
      data,
      visibleLines,
      perTrade: perTradeSeries,
      palette: chartPalette(theme),
      showSigmaBands: graph.showSigmaBands,
      showBreakevens: graph.showBreakevens,
      yLock:
        graph.yLock && graph.yMin !== null && graph.yMax !== null
          ? { min: graph.yMin, max: graph.yMax }
          : null,
      // Only once the view differs from the base: untouched, the window *is*
      // the computed range, and passing it would pin the y axis to a padded
      // bound instead of letting ECharts pick round ones — a visible change
      // to the default view for no gain.
      xWindow: viewDiffers ? view : null,
    })
  }, [data, visibleLines, perTradeSeries, graph, theme, view, viewDiffers])

  // Off the fetched grid — the blank half of a window panned past its
  // coverage — there is no sample to read: null, which the readout shows as a
  // parked-on-spot card, or as "outside the fetched range" when spot is off
  // the grid too. Clamping to an edge sample would print a P&L for a price
  // nowhere near the crosshair.
  const cursorIndex = data && cursorPrice !== null ? indexAt(data.prices, cursorPrice) : null

  return (
    <div className="grid min-h-0 flex-1 grid-cols-1 gap-2 xl:grid-cols-[minmax(0,1fr)_20rem]">
      <div className="ws-card flex min-h-0 flex-col">
        <header className="flex flex-wrap items-center gap-x-4 gap-y-1 border-b border-line px-2.5 py-1">
          <h2 className="ws-label m-0 font-semibold">Risk graph</h2>
          {/* Every readout here is the BASE response's, never the panned or
              zoomed view's: a number that wandered as the trader dragged
              would be unreadable, and "max loss" is a claim about a price
              range, not about a screenful. */}
          {base ? (
            <>
              <Readout
                label="max profit"
                value={fmtPnl(base.max_profit_on_range, 0)}
                tone={pnlColorClass(base.max_profit_on_range)}
                tip="Highest P&L reached by the expiration line over the base range — the price range the range mode computes, not the slice on screen."
              />
              <Readout
                label="max loss"
                value={fmtPnl(base.max_loss_on_range, 0)}
                tone={pnlColorClass(base.max_loss_on_range)}
                tip="Lowest P&L reached by the expiration line over the base range — the price range the range mode computes, not the slice on screen, and not a guaranteed floor beyond it."
              />
              <Readout
                label="PoP"
                value={fmtRatioPct(base.pop, 1)}
                tip="Probability of finishing profitable at the front expiry, lognormal, using the front ATM IV, over the base range."
              />
              <Readout
                label="ATM IV"
                value={fmtRatioPct(base.front_atm_iv, 2)}
                tip="Front-expiry at-the-money IV solved by Wingspan; it drives PoP and the σ bands."
              />
              <Readout
                label="BE"
                value={
                  (base.breakevens.expiry ?? []).map((be) => fmtNumber(be, 0)).join(' / ') || '—'
                }
                tip="Breakevens of the expiration line inside the base range the range mode computes."
              />
              {base.warnings.length > 0 ? (
                <Tooltip
                  align="left"
                  content={
                    <ul className="m-0 list-disc space-y-1 pl-4">
                      {base.warnings.map((w) => (
                        <li key={w.code + w.message}>
                          <span className="num">{w.code}</span> — {w.message}
                        </li>
                      ))}
                    </ul>
                  }
                >
                  <Pill
                    tone={base.warnings.some((w) => w.severity === 'error') ? 'neg' : 'warn'}
                    title={`${base.warnings.length} warning${base.warnings.length === 1 ? '' : 's'} behind these curves`}
                  >
                    ⚠ {base.warnings.length}
                  </Pill>
                </Tooltip>
              ) : null}
            </>
          ) : null}
          <span className="ml-auto flex items-center gap-2">
            {baseQuery.isFetching || viewQuery.isFetching ? <Spinner label="" /> : null}
            <span className="text-[10px] text-muted">
              {selected.length} trade{selected.length === 1 ? '' : 's'} · P&L{' '}
              <span className={`num ${pnlColorClass(stats.pnl_total)}`}>
                {fmtPnl(stats.pnl_total, 0)}
              </span>
            </span>
            <ToggleButton
              pressed={graphMaximized}
              onClick={() => setGraphMaximized(!graphMaximized)}
              title={
                graphMaximized
                  ? 'Bring the portfolio cards and the table back (Esc)'
                  : 'Give the graph the whole tab; Esc brings the rest back'
              }
            >
              <span aria-hidden="true">{graphMaximized ? '⤡' : '⤢'}</span>
              {graphMaximized ? 'restore' : 'maximize'}
            </ToggleButton>
          </span>
        </header>

        {/* The chart takes whatever height the readouts below leave; when
            the panel is too short for both, the chart keeps its floor and
            the panel scrolls instead. */}
        <div className="flex min-h-0 flex-1 flex-col gap-2 overflow-y-auto p-2">
          {selected.length === 0 ? (
            <EmptyState>
              Select trades in the table to draw the expiration line and the T+x ladder for that
              set.
            </EmptyState>
          ) : option && data ? (
            <>
              {/* A transient background-refetch failure keeps the last-drawn
                  curves on screen with an inline note, rather than blanking a
                  graph a trader may be actively watching. */}
              {failing ? (
                <ErrorNote error={failing.error} onRetry={() => void failing.refetch()} />
              ) : null}
              <div className="min-h-[380px] flex-1">
                <RiskChart
                  option={option}
                  height="fill"
                  onCursorPrice={setCursorPrice}
                  onPan={handlePan}
                  onZoom={handleZoom}
                  onResetView={resetView}
                  pannable={view !== null}
                  canZoomIn={zoom < MAX_ZOOM}
                  canZoomOut={zoom > MIN_ZOOM}
                  ariaLabel={`Risk graph for ${selected.length} selected trades: expiration line plus ${
                    visibleLines.length - 1
                  } T+x lines`}
                />
              </div>
              <div className="grid shrink-0 grid-cols-1 gap-2 lg:grid-cols-2">
                <CrosshairReadout
                  data={data}
                  lines={visibleLines}
                  index={cursorIndex}
                  theme={theme}
                />
                {graph.showSliceTable ? <SliceTable data={data} lines={visibleLines} /> : null}
              </div>
            </>
          ) : baseQuery.isError ? (
            <ErrorNote error={baseQuery.error} onRetry={() => void baseQuery.refetch()} />
          ) : (
            <div className="px-3 py-6">
              <Spinner label="Computing curves…" />
            </div>
          )}
        </div>

        {/* Zoom lives with the chart, not in the control column: it is the
            same gesture as the drag and the wheel, and the trader's eye is
            already here. It sits *outside* the scroll area on purpose — the
            wheel over the chart is a zoom, so the control that replaces it
            (and `reset view`, the way back) must never be below the fold. */}
        {option && data ? (
          <div className="flex shrink-0 flex-wrap items-center gap-x-3 gap-y-1 border-t border-line px-2.5 py-1">
            <Slider
              label="zoom"
              // The track ends on its own last step, and both ends of it read
              // exactly: pointed at log2(5), which is off the step grid, the
              // browser sanitises the thumb back a step, so `End` fired
              // nothing and 500% was unreachable by mouse or keyboard.
              value={zoomToSlider(zoom)}
              min={MIN_ZOOM_LOG2}
              max={SLIDER_MAX_LOG2}
              step={ZOOM_STEP_LOG2}
              onChange={handleSliderZoom}
              format={(value) => formatZoom(zoomFromSlider(value))}
              // Notches at the halvings around home, so 100% is a stop the
              // thumb finds by feel rather than by reading the number.
              marks={ZOOM_MARKS}
            />
            <button
              type="button"
              className="ws-btn"
              onClick={resetView}
              disabled={graph.xZoom === DEFAULT_ZOOM && graph.xCenter === null}
              title="Back to 100% over the whole computed price range (or double-click the chart)"
            >
              reset view
            </button>
            <span className="text-[10px] text-muted">drag to pan · wheel to zoom</span>
          </div>
        ) : null}
      </div>

      <div className="ws-card min-h-0">
        <GraphControls
          data={data}
          view={viewDiffers ? view : null}
          trades={trades}
          expiries={expiries}
          frontDte={frontDte}
          spot={data?.spot ?? null}
          effectiveDates={(data?.lines ?? []).map((line) => line.date)}
          selected={selected}
          theme={theme}
          baseline={{
            // The what-if answer follows the include-realised toggle, so its
            // "vs now" baseline has to be measured on the same basis.
            pnl: graph.includeRealized ? stats.pnl_total : stats.pnl_open,
            greeks: stats.greeks,
          }}
        />
      </div>
    </div>
  )
}
