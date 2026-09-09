/** The ONE-style risk graph: chart, crosshair readout, slices and controls. */

import { useCallback, useMemo, useState } from 'react'
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
import { clampZoom, xWindow, zoomAt } from '../../lib/viewport'
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

  // The visible slice of the price axis. Purely client-side: the server is
  // never re-queried for a zoom or a pan, so the header's max profit / max
  // loss / PoP stay the numbers it computed over the whole range.
  const view = useMemo(
    () => (data ? xWindow(data.prices, graph.xZoom, graph.xCenter, data.spot) : null),
    [data, graph.xZoom, graph.xCenter],
  )

  // Every handler funnels its new centre back through `xWindow`, which is the
  // one place that keeps the window inside the computed range.
  const centreOn = useCallback(
    (zoom: number, centre: number): void => {
      if (!data) return
      const next = xWindow(data.prices, zoom, centre, data.spot)
      patchGraph({
        xZoom: clampZoom(zoom),
        xCenter: next ? (next.min + next.max) / 2 : centre,
      })
    },
    [data, patchGraph],
  )

  const resetView = useCallback(() => patchGraph({ xZoom: 1, xCenter: null }), [patchGraph])

  const handlePan = useCallback(
    (deltaPrice: number) => {
      // At zoom 1 the window already is the whole range: a drag has nowhere
      // to go, and recording a centre would only make `reset view` light up.
      if (!view || graph.xZoom <= 1 || !Number.isFinite(deltaPrice)) return
      centreOn(graph.xZoom, (view.min + view.max) / 2 + deltaPrice)
    },
    [centreOn, graph.xZoom, view],
  )

  const handleZoom = useCallback(
    (factor: number, anchorPrice: number) => {
      if (!view) return
      const next = zoomAt(graph.xZoom, factor, anchorPrice, view)
      if (next.xZoom <= 1) {
        resetView()
        return
      }
      centreOn(next.xZoom, next.xCenter)
    },
    [centreOn, graph.xZoom, resetView, view],
  )

  const handleSliderZoom = useCallback(
    (log2Zoom: number) => {
      const zoom = clampZoom(2 ** log2Zoom)
      // The slider zooms about the window's own centre, not the midpoint of
      // the computed range, so the price being studied stays in view.
      if (zoom <= 1 || !data) {
        resetView()
        return
      }
      centreOn(zoom, graph.xCenter ?? data.spot)
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
      xWindow: view,
    })
  }, [data, visibleLines, perTradeSeries, graph, theme, view])

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
                  pannable={graph.xZoom > 1}
                  ariaLabel={`Risk graph for ${selected.length} selected trades: expiration line plus ${
                    visibleLines.length - 1
                  } T+x lines`}
                />
              </div>
              {/* Zoom lives with the chart, not in the control column: it is
                  the same gesture as the drag and the wheel, and the trader's
                  eye is already here. */}
              <div className="flex shrink-0 flex-wrap items-center gap-x-3 gap-y-1">
                <Slider
                  label="zoom"
                  value={Math.log2(graph.xZoom)}
                  min={0}
                  max={4}
                  step={0.05}
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
