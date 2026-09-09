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
import { nearestPriceIndex } from '../../lib/readout'
import { useDebouncedValue } from '../../lib/useDebouncedValue'
import { clampZoom, MAX_ZOOM, snapZoom, xWindow, ZOOM_STEP_LOG2, zoomAt } from '../../lib/viewport'
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

export function RiskGraphPanel({ trades }: { trades: Trade[] }) {
  const selected = useAppStore((s) => s.selected)
  const graph = useAppStore((s) => s.graph)
  const theme = useAppStore((s) => s.theme)
  const graphMaximized = useAppStore((s) => s.graphMaximized)
  const setGraphMaximized = useAppStore((s) => s.setGraphMaximized)
  const patchGraph = useAppStore((s) => s.patchGraph)
  const [cursorPrice, setCursorPrice] = useState<number | null>(null)

  const request = useMemo(() => buildRiskGraphRequest(selected, graph), [selected, graph])
  // A vol/range slider fires `request` on every drag tick; only firing the
  // POST once it holds still keeps a drag from queuing dozens of full
  // pricing-grid computations, and useRiskGraph forwards an AbortSignal so a
  // request superseded before it settles is cancelled outright.
  const debouncedRequest = useDebouncedValue(request, 250)
  const query = useRiskGraph(debouncedRequest, true)
  const data = query.data

  const selectedTrades = tradesById(trades, selected)
  const remoteStats = usePortfolio(selected)
  const stats: PortfolioStats = remoteStats.data ?? aggregateTrades(selectedTrades)

  const expiries = expiriesOf(selectedTrades)
  const front = expiries[0] ?? null
  const frontDte = front ? daysBetween(todayIso(), front) : null

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

  // Read once, clamped: a corrupted persisted blob (xZoom 0, or a negative)
  // must not reach the slider as log2(0) while the chart itself, which clamps
  // on the way in, is drawing the whole range.
  const zoom = clampZoom(graph.xZoom)

  // The visible slice of the price axis. Purely client-side: the server is
  // never re-queried for a zoom or a pan, so the header's max profit / max
  // loss / PoP stay the numbers it computed over the whole range.
  const view = useMemo(
    () => (data ? xWindow(data.prices, zoom, graph.xCenter, data.spot) : null),
    [data, zoom, graph.xCenter],
  )

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

  // Every handler funnels its new centre back through `xWindow`, which is the
  // one place that keeps the window inside the computed range.
  const centreOn = useCallback(
    (nextZoom: number, centre: number): void => {
      if (!data) return
      const next = xWindow(data.prices, nextZoom, centre, data.spot)
      patchGraph({
        xZoom: clampZoom(nextZoom),
        xCenter: next ? (next.min + next.max) / 2 : centre,
      })
    },
    [data, patchGraph],
  )

  const resetView = useCallback(() => {
    // patchGraph always writes a new `graph` object, which rebuilds the whole
    // chart option and touches localStorage; a wheel spinning at 1x would
    // pay that per event for a view that is already the default.
    if (graph.xZoom === 1 && graph.xCenter === null) return
    patchGraph({ xZoom: 1, xCenter: null })
  }, [graph.xCenter, graph.xZoom, patchGraph])

  const handlePan = useCallback(
    (deltaPrice: number) => {
      // At zoom 1 the window already is the whole range: a drag has nowhere
      // to go, and recording a centre would only make `reset view` light up.
      if (!view || zoom <= 1 || !Number.isFinite(deltaPrice)) return
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
      const target = snapZoom(clampZoom(zoom * factor))
      if (target <= 1) {
        resetView()
        return
      }
      const next = zoomAt(zoom, target / zoom, anchorPrice, view)
      centreOn(next.xZoom, next.xCenter)
    },
    [centreOn, resetView, view, zoom],
  )

  const handleSliderZoom = useCallback(
    (log2Zoom: number) => {
      const next = clampZoom(2 ** log2Zoom)
      // The slider zooms about the window's own centre, not the midpoint of
      // the computed range, so the price being studied stays in view.
      if (next <= 1 || !data) {
        resetView()
        return
      }
      centreOn(next, graph.xCenter ?? data.spot)
    },
    [centreOn, data, graph.xCenter, resetView],
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
      // Only when it is actually a zoom: at 1x the window *is* the computed
      // range, and passing it would pin the y axis to a padded bound instead
      // of letting ECharts pick round ones — a visible change to the default
      // view for no gain.
      xWindow: zoom > 1 ? view : null,
    })
  }, [data, visibleLines, perTradeSeries, graph, theme, view, zoom])

  const cursorIndex =
    data && cursorPrice !== null && Number.isFinite(cursorPrice)
      ? nearestPriceIndex(data.prices, cursorPrice)
      : null

  return (
    <div className="grid min-h-0 flex-1 grid-cols-1 gap-2 xl:grid-cols-[minmax(0,1fr)_20rem]">
      <div className="ws-card flex min-h-0 flex-col">
        <header className="flex flex-wrap items-center gap-x-4 gap-y-1 border-b border-line px-2.5 py-1">
          <h2 className="ws-label m-0 font-semibold">Risk graph</h2>
          {data ? (
            <>
              <Readout
                label="max profit"
                value={fmtPnl(data.max_profit_on_range, 0)}
                tone={pnlColorClass(data.max_profit_on_range)}
                tip="Highest P&L reached by the expiration line inside the computed price range."
              />
              <Readout
                label="max loss"
                value={fmtPnl(data.max_loss_on_range, 0)}
                tone={pnlColorClass(data.max_loss_on_range)}
                tip="Lowest P&L reached by the expiration line inside the computed price range — not a guaranteed floor beyond it."
              />
              <Readout
                label="PoP"
                value={fmtRatioPct(data.pop, 1)}
                tip="Probability of finishing profitable at the front expiry, lognormal, using the front ATM IV."
              />
              <Readout
                label="ATM IV"
                value={fmtRatioPct(data.front_atm_iv, 2)}
                tip="Front-expiry at-the-money IV solved by Wingspan; it drives PoP and the σ bands."
              />
              <Readout
                label="BE"
                value={
                  (data.breakevens.expiry ?? []).map((be) => fmtNumber(be, 0)).join(' / ') || '—'
                }
                tip="Breakevens of the expiration line inside the computed price range."
              />
              {data.warnings.length > 0 ? (
                <Tooltip
                  align="left"
                  content={
                    <ul className="m-0 list-disc space-y-1 pl-4">
                      {data.warnings.map((w) => (
                        <li key={w.code + w.message}>
                          <span className="num">{w.code}</span> — {w.message}
                        </li>
                      ))}
                    </ul>
                  }
                >
                  <Pill
                    tone={data.warnings.some((w) => w.severity === 'error') ? 'neg' : 'warn'}
                    title={`${data.warnings.length} warning${data.warnings.length === 1 ? '' : 's'} behind these curves`}
                  >
                    ⚠ {data.warnings.length}
                  </Pill>
                </Tooltip>
              ) : null}
            </>
          ) : null}
          <span className="ml-auto flex items-center gap-2">
            {query.isFetching ? <Spinner label="" /> : null}
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
              {query.isError ? (
                <ErrorNote error={query.error} onRetry={() => void query.refetch()} />
              ) : null}
              <div className="min-h-[380px] flex-1">
                <RiskChart
                  option={option}
                  height="fill"
                  onCursorPrice={setCursorPrice}
                  onPan={handlePan}
                  onZoom={handleZoom}
                  onResetView={resetView}
                  pannable={zoom > 1}
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
          ) : query.isError ? (
            <ErrorNote error={query.error} onRetry={() => void query.refetch()} />
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
              value={Math.log2(zoom)}
              min={0}
              max={Math.log2(MAX_ZOOM)}
              step={ZOOM_STEP_LOG2}
              onChange={handleSliderZoom}
              format={(value) => `${(2 ** value).toFixed(1)}×`}
            />
            <button
              type="button"
              className="ws-btn"
              onClick={resetView}
              disabled={graph.xZoom === 1 && graph.xCenter === null}
              title="Back to the whole computed price range (or double-click the chart)"
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
