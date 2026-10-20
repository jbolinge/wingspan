/**
 * The ONE-style risk graph: chart, P&L inset, slices and controls.
 *
 * Mounted twice over: under the table on Positions, and as the Model tab's
 * "Risk graph" view beside the ticket — the same component with the same
 * props, so both hit the same query cache and switching is instant.
 *
 * The modeled legs from the Model tab join every request here (`model_legs`)
 * while the model's eye is open, as one synthetic position entered at today's
 * mark — so T+0 at spot still equals the dashboard P&L, and what moves is the
 * shape. With nothing selected the model is drawn on its own — the trader
 * asking what the legs they are building do by themselves — while the Model
 * tab's Pos column still counts the whole book, because that is what they
 * hold. The empty `trade_ids` says so: the server reads an empty list plus
 * model legs as the model alone (`docs/IPC.md`, "Model legs").
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { useLiveModelLegs, usePortfolio, useRiskGraph } from '../../api/queries'
import type { PortfolioStats, RiskGraphResponse, Trade } from '../../api/types'
import { aggregateTrades, expiriesOf, tradesById } from '../../lib/aggregate'
import { buildRiskChartOption, compareLinesOf, perTradeOverlayCurves } from '../../lib/chartOption'
import { daysBetween, todayIso } from '../../lib/dates'
import { fmtNumber, fmtPnl, fmtRatioPct, pnlColorClass } from '../../lib/format'
import { buildRiskGraphRequest, effectiveDateSpecs } from '../../lib/graphRequest'
import { buildCursorOverlay } from '../../lib/greekStrip'
import {
  describeLines,
  frontForSelection,
  headerBreakevens,
  isLineHidden,
  liveExpiries,
  selectionKey,
  serverDateOf,
  terminalLabel,
  terminalOf,
} from '../../lib/lines'
import { chartPalette } from '../../lib/palette'
import { overlayTradeName, toModelLegs } from '../../lib/model'
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
import { SPX_POINTS, XSP_AXIS_NOTE } from '../units'
import { GraphControls } from './GraphControls'
import { PnlInset } from './PnlInset'
import { RiskChart } from './RiskChart'
import { SliceTable } from './SliceTable'

function Readout({
  label,
  value,
  tone,
  tip,
  unit,
}: {
  label: string
  value: string
  tone?: string
  tip?: string
  /** A unit printed after the value, e.g. `SPX pts`. */
  unit?: string
}) {
  return (
    <Tooltip content={tip ?? label} align="center">
      <span className="flex cursor-help items-baseline gap-1">
        <span className="ws-label">{label}</span>
        <span className={`num text-[12px] font-semibold ${tone ?? 'text-fg'}`}>{value}</span>
        {unit ? <span className="text-[10px] text-muted">{unit}</span> : null}
      </span>
    </Tooltip>
  )
}

/**
 * The header's reminder that the curves include modeled legs, with the three
 * things a trader does about it from here: hide them, go and edit them, or
 * throw them away.
 */
function ModelPill({ legs, include }: { legs: number; include: boolean }) {
  const setModelInclude = useAppStore((s) => s.setModelInclude)
  const setTab = useAppStore((s) => s.setTab)
  const setModelView = useAppStore((s) => s.setModelView)
  const setGraphMaximized = useAppStore((s) => s.setGraphMaximized)
  const clearModel = useAppStore((s) => s.clearModel)
  // Editing happens on the chain: from Positions that is the Model tab, and
  // from the Model tab's own graph view it is the switch back to the chain —
  // `setTab` alone would leave the trader exactly where they were. It drops
  // a maximize too: editing is leaving the graph, and a maximize left set
  // would swallow the Model tab's toolbar the next time its graph opens.
  return (
    <span
      className={`flex items-center gap-1 rounded border px-1.5 py-px ${
        include ? 'border-warn/45 bg-warn/10' : 'border-line bg-surface-2'
      }`}
    >
      <span
        className={`text-[10px] font-semibold tracking-wide uppercase ${include ? 'text-warn' : 'text-muted'}`}
        title={
          include
            ? 'Modeled legs from the Model tab are drawn with the position'
            : 'Modeled legs exist but are hidden from the graph'
        }
      >
        Model · {legs} leg{legs === 1 ? '' : 's'}
      </span>
      <button
        type="button"
        className="ws-btn px-1 py-0 leading-4"
        aria-pressed={include}
        aria-label="Draw the model on the graph"
        title={include ? 'Hide the model — draw the position alone' : 'Draw the model on the graph'}
        onClick={() => setModelInclude(!include)}
      >
        <span aria-hidden="true">{include ? '◉' : '○'}</span>
      </button>
      <button
        type="button"
        className="bg-transparent px-0.5 text-[10px] text-accent underline-offset-2 hover:underline"
        title="Edit the modeled legs on the Model tab's chain"
        onClick={() => {
          setTab('model')
          setModelView('chain')
          setGraphMaximized(false)
        }}
      >
        edit
      </button>
      <button
        type="button"
        className="bg-transparent px-0.5 text-[11px] leading-none text-muted hover:text-neg"
        aria-label="Clear modeled trades"
        title="Clear the modeled trades"
        onClick={clearModel}
      >
        ✕
      </button>
    </span>
  )
}

/** Notches on the zoom slider: one octave either side of home. */
const ZOOM_MARKS = [0.5, 1, 2].map((zoom) => ({ value: zoomToSlider(zoom), label: formatZoom(zoom) }))

/**
 * What to say when there is nothing to draw. The panel is mounted on two
 * tabs, and the way to give it something differs: on Positions it is the
 * table right above; on the Model tab it is the chain, or a model whose eye
 * was closed.
 */
function EmptyGraph({ onModelTab, hiddenModel }: { onModelTab: boolean; hiddenModel: boolean }) {
  if (!onModelTab) {
    return (
      <EmptyState>
        Select trades in the table to draw the expiration line and the T+x ladder for that set.
      </EmptyState>
    )
  }
  return (
    <EmptyState>
      {hiddenModel
        ? 'The model is hidden from the graph — turn its eye (◉) back on above, or select trades on the Positions tab.'
        : 'Nothing to draw yet. Type a quantity into a Model cell on the Chain, or select trades on the Positions tab, to draw the expiration line and the T+x ladder.'}
    </EmptyState>
  )
}

export function RiskGraphPanel({ trades }: { trades: Trade[] }) {
  const selected = useAppStore((s) => s.selected)
  // Mounted on Positions and on the Model tab's graph view; only the wording
  // of the empty state and of the maximize button follows the tab.
  const onModelTab = useAppStore((s) => s.tab === 'model')
  const graph = useAppStore((s) => s.graph)
  const theme = useAppStore((s) => s.theme)
  const graphMaximized = useAppStore((s) => s.graphMaximized)
  const setGraphMaximized = useAppStore((s) => s.setGraphMaximized)
  const patchGraph = useAppStore((s) => s.patchGraph)
  const model = useAppStore((s) => s.model)
  const [cursorPrice, setCursorPrice] = useState<number | null>(null)

  // What the model contributes to the requests: nothing while its eye is
  // closed, so a hidden model costs exactly the request it would without one.
  // Only legs still live on the server's date go out; until the first
  // snapshot says what that date is (`liveLegs === null`) the graph waits,
  // rather than send a leg that expired overnight and be refused outright.
  const liveLegs = useLiveModelLegs()
  const modelPending = model.include && liveLegs === null
  const drawnModel = useMemo(
    () => (model.include && liveLegs !== null ? toModelLegs(liveLegs) : []),
    [model.include, liveLegs],
  )
  // A model still waiting on that date already counts as drawn, so the
  // panel does not flash "select trades" for the moment it takes.
  const hasModel = drawnModel.length > 0 || modelPending
  // The selection, as it stands: with a model and nothing selected the empty
  // list is what asks the server for the model alone. (Sending the open
  // book's ids here instead is what used to draw every open trade under a
  // model the trader meant to see by itself.)
  const graphTradeIds = selected
  // The model drawn with no trades under it — the header says so, and there
  // is no position for "compare position" to draw.
  const modelOnly = selected.length === 0 && hasModel
  // Whether the graph draws anything at all — the same rule as the empty
  // state below.
  const drawing = graphTradeIds.length > 0 || hasModel

  // The BASE query: the range mode's own request, untouched by the view. Its
  // answer defines the base range and every header readout, so those stay put
  // while the trader pans — and it is always kept alive, because the window
  // is measured against it.
  const request = useMemo(
    () => buildRiskGraphRequest(graphTradeIds, graph, drawnModel),
    [graphTradeIds, graph, drawnModel],
  )
  // A vol/range slider fires `request` on every drag tick; only firing the
  // POST once it holds still keeps a drag from queuing dozens of full
  // pricing-grid computations, and useRiskGraph forwards an AbortSignal so a
  // request superseded before it settles is cancelled outright.
  //
  // While the model waits on the server's date there is no base request at
  // all — not a model-less one: debounced, that would still be the value in
  // hand the moment the date arrives, and go out without the model.
  const debouncedRequest = useDebouncedValue(modelPending ? null : request, 250)
  const baseQuery = useRiskGraph(debouncedRequest, true)
  const base = baseQuery.data
  // The selection the base answer in hand was computed for, noted when it
  // arrives: a placeholder kept across a selection change is the previous
  // selection's answer, and so is the last answer while a new selection's
  // request is still debouncing.
  const [answered, setAnswered] = useState<{ data: RiskGraphResponse; key: string | null } | null>(
    null,
  )
  if (base !== undefined && !baseQuery.isPlaceholderData && answered?.data !== base) {
    setAnswered({ data: base, key: selectionKey(debouncedRequest) })
  }

  const selectedTrades = tradesById(trades, graphTradeIds)
  // The model rides along so the header P&L and the what-if baseline describe
  // the same position as the curves and the greek strip.
  const remoteStats = usePortfolio(graphTradeIds, drawnModel)
  const stats: PortfolioStats = remoteStats.data ?? aggregateTrades(selectedTrades)

  // The server's date, as the base response's T+0 line states it — the New
  // York day the curves are valued on. The browser's own day disagrees with
  // it for part of every evening (and a pinned snapshot can be days behind),
  // so a ladder counted from `todayIso()` was a day long, and an expiry that
  // had settled on the server still looked live. The browser clock is only
  // the fallback before the first answer.
  const today = serverDateOf(base?.lines) ?? todayIso()

  // A modeled leg's expiry gets a vol slider too: the per-expiry shift applies
  // to every leg on that date, modeled or held. Only the live ones, counted
  // from the server's date: a settled expiry still sitting in a trade's legs
  // is no front at all — the engine's own live-front rule — and taking it as
  // one made the DTE negative and the preset ladder vanish.
  // Today's own expiry stays live until the server's live front has moved
  // past it (its settlement instant is the server's to know).
  const expiries = liveExpiries(
    [...new Set([...expiriesOf(selectedTrades), ...drawnModel.map((leg) => leg.contract.expiry)])].sort(),
    today,
    frontForSelection(answered?.data === base ? base : undefined, answered?.key, selectionKey(request)),
  )
  const front = expiries[0] ?? null
  const frontDte = front ? daysBetween(today, front) : null
  // The x-axis is SPX points by design, even for XSP legs; say so whenever
  // the drawn position holds any, so an XSP strike is never typed as a price.
  const holdsXsp =
    selectedTrades.some((trade) => trade.legs.some((leg) => leg.qty !== 0 && leg.contract.underlying === 'XSP')) ||
    drawnModel.some((leg) => leg.contract.underlying === 'XSP')

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
    // The base grid's density is its `points` uniform samples — the request's
    // own count, not `prices.length`, which also counts spot and the in-range
    // strikes the server inserts, and changes with the selection.
    const coverage = coverageFor(view, baseRange, request.points)
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

  // The COMPARE query: whichever request is being drawn, minus the model —
  // the position as it stands, for the ghost lines under the model's own —
  // at the dates the drawn response actually has, so the two families line
  // up key for key (see `effectiveDateSpecs`). With the model alone there is
  // no position: the same request minus the model would be `trade_ids: []`
  // with no legs, which the server reads as the whole book — ghost lines for
  // trades the graph is not drawing.
  const comparing = model.comparePosition && hasModel && !modelOnly
  const compareRequest = useMemo(() => {
    if (!comparing) return null
    const drawn = viewRequest ?? request
    if (drawn.dates !== null) return { ...drawn, model_legs: [] }
    // No request before there are lines to pin to: one sent on `dates: null`
    // would be re-sent the moment the base answer lands. The last base
    // answer serves while a new one is in flight — its ladder is almost
    // always this one's, and a stale ladder costs one extra request, where
    // dropping the request would blink the ghosts out on every slider tick.
    if (!base) return null
    return { ...drawn, dates: effectiveDateSpecs(base.lines), model_legs: [] }
  }, [base, comparing, request, viewRequest])
  const debouncedCompareRequest = useDebouncedValue(compareRequest, 250)
  // Window-only placeholder for the same reason as the view query: a compare
  // response for another selection must never sit under this one's curves.
  const compareQuery = useRiskGraph(debouncedCompareRequest, debouncedCompareRequest !== null, true)
  const compareLines = useMemo(
    () => (comparing ? compareLinesOf(compareQuery.data, graph.hiddenLines) : null),
    [comparing, compareQuery.data, graph.hiddenLines],
  )

  // What is drawn: the view response wherever there is one, the base response
  // otherwise. A pan past the fetched coverage leaves the previous view data
  // on screen until the next one lands — ECharts simply draws nothing beyond
  // the samples it has, which is the honest picture of a curve still loading.
  const data = viewRequest !== null && viewQuery.data ? viewQuery.data : base

  // The base query is the one that can leave the panel with nothing to draw;
  // a failing view query is a note over curves that are still good.
  const failing = baseQuery.isError ? baseQuery : viewQuery.isError ? viewQuery : null

  const visibleLines = useMemo(
    () => (data?.lines ?? []).filter((line) => !isLineHidden(line, graph.hiddenLines)),
    [data, graph.hiddenLines],
  )

  // Once a real base answer is in hand (not a placeholder carried over from
  // another request), retire what its server date shows to be stale: date
  // specs and hidden `expiry:<date>` keys before it, and an older blob's bare
  // `expiry` hide key. Never a hidden key just because this selection does
  // not draw it — the trader's hide survives a look at another trade.
  const pruneExpiredGraphDates = useAppStore((s) => s.pruneExpiredGraphDates)
  const settledBase = baseQuery.isPlaceholderData ? undefined : base
  useEffect(() => {
    if (!settledBase) return
    const serverDay = serverDateOf(settledBase.lines)
    if (serverDay !== null) pruneExpiredGraphDates(serverDay)
  }, [settledBase, pruneExpiredGraphDates])

  // The line the header's max profit, max loss, PoP and BE are read from,
  // named beside them: with several expirations drawn, or none, "max loss"
  // alone does not say which curve it is the minimum of.
  const terminal = base ? terminalOf(base) : null
  const terminalName = terminal ? terminalLabel(terminal) : null
  const terminalTip = terminal
    ? terminal.drawn
      ? `Max profit, max loss, PoP and BE are all measured on this line — the front expiration.`
      : `The date list does not draw the front expiration, so max profit, max loss, PoP and BE are measured on it — which is not drawn. Add it under Dates to see the curve they describe.`
    : ''

  // `trades` (App's openTrades) is a new array reference every 30s poll tick,
  // even when nothing the overlay draws has changed. Depending on it directly
  // would rebuild the whole chart option — a full ECharts teardown — on every
  // tick; this key is stable unless the id/name pairs the overlay actually
  // shows have changed.
  const overlayTradeIds = graph.perTradeOverlay ? perTradeOverlayCurves(data).map((s) => s.trade_id) : []
  const overlayNameKey = overlayTradeIds.map((id) => `${id}:${overlayTradeName(id, trades)}`).join('|')

  const perTradeSeries = useMemo(() => {
    if (!graph.perTradeOverlay) return []
    return perTradeOverlayCurves(data).map((series) => ({
      tradeId: series.trade_id,
      name: overlayTradeName(series.trade_id, trades),
      pnl: series.pnl,
    }))
    // eslint-disable-next-line react-hooks/exhaustive-deps -- see overlayNameKey above
  }, [data, graph.perTradeOverlay, overlayNameKey])

  // A centre is a price on *this* price grid. When the grid itself moves —
  // another selection, another range mode, a different point count — a
  // centre left over from the old one clings to whichever end of the new
  // range is nearest, so the trader lands on an edge sliver of a position
  // they were not looking at. Keep the zoom, drop the centre.
  const gridKey = `${graphTradeIds.join(',')}|${graph.rangeMode}|${graph.rangePct}|${graph.rangeMin}|${graph.rangeMax}|${graph.points}|${graph.spotOverride}`
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
      compare: compareLines,
    })
  }, [data, visibleLines, perTradeSeries, graph, theme, view, viewDiffers, compareLines])

  // Off the fetched grid — the blank half of a window panned past its
  // coverage — there is no sample to read: null, which the inset shows as a
  // parked-on-spot card, or as "outside the fetched range" when spot is off
  // the grid too. Clamping to an edge sample would print a P&L for a price
  // nowhere near the crosshair.
  const cursorIndex = data && cursorPrice !== null ? indexAt(data.prices, cursorPrice) : null

  // A merged partial option, applied on top of the main one without a
  // rebuild — the strip's cursor boxes move on every mouse tick, and
  // rebuilding the whole chart option for that would tear down and redraw
  // seven series groups on every frame of a drag.
  const overlay = useMemo(
    () =>
      data
        ? buildCursorOverlay({
            data,
            index: cursorIndex ?? indexAt(data.prices, data.spot),
            window: viewDiffers ? view : null,
            palette: chartPalette(theme),
          })
        : null,
    [cursorIndex, data, theme, view, viewDiffers],
  )

  return (
    <div className="grid min-h-0 flex-1 grid-cols-1 gap-2 xl:grid-cols-[minmax(0,1fr)_20rem]">
      <div className="ws-card flex min-h-0 flex-col">
        <header className="flex flex-wrap items-center gap-x-4 gap-y-1 border-b border-line px-2.5 py-1">
          <h2 className="ws-label m-0 font-semibold">Risk graph</h2>
          {model.legs.length > 0 ? <ModelPill legs={model.legs.length} include={model.include} /> : null}
          {/* Every readout here is the BASE response's, never the panned or
              zoomed view's: a number that wandered as the trader dragged
              would be unreadable, and "max loss" is a claim about a price
              range, not about a screenful. */}
          {/* Only while something is drawn. Hiding the model with nothing
              selected disables the base query, and its keep-the-last-answer
              placeholder would otherwise leave the model's max
              profit, PoP and breakevens beside an empty graph. */}
          {base && drawing ? (
            <>
              {terminalName ? (
                <Tooltip content={terminalTip} align="center">
                  <span
                    className={`num cursor-help text-[10px] ${terminal?.drawn === false ? 'text-warn' : 'text-muted'}`}
                    data-testid="terminal-label"
                  >
                    {terminalName}
                    {terminal?.drawn === false ? ' · not drawn' : ''}
                  </span>
                </Tooltip>
              ) : null}
              <Readout
                label="max profit"
                value={fmtPnl(base.max_profit_on_range, 0)}
                tone={pnlColorClass(base.max_profit_on_range)}
                tip={`Highest P&L reached ${terminalName ?? 'by the expiration line'} over the base range — the price range the range mode computes, not the slice on screen.`}
              />
              <Readout
                label="max loss"
                value={fmtPnl(base.max_loss_on_range, 0)}
                tone={pnlColorClass(base.max_loss_on_range)}
                tip={`Lowest P&L reached ${terminalName ?? 'by the expiration line'} over the base range — the price range the range mode computes, not the slice on screen, and not a guaranteed floor beyond it.`}
              />
              <Readout
                label="PoP"
                value={fmtRatioPct(base.pop, 1)}
                tip={`Probability of finishing profitable ${terminalName ?? 'at the front expiry'}, lognormal, from the ATM IV. It reads — when there is no vol, or no time left, to measure it with.`}
              />
              <Readout
                label="ATM IV"
                value={fmtRatioPct(base.front_atm_iv, 2)}
                tip="Front-expiry at-the-money IV solved by Wingspan; it drives PoP and the σ bands. It reads — when there is none to read."
              />
              <Readout
                label="BE"
                value={
                  headerBreakevens(base, terminal)
                    .map((be) => `${fmtNumber(be.value, 0)}${be.offRange ? ' (off-range)' : ''}`)
                    .join(' / ') || '—'
                }
                unit={SPX_POINTS}
                tip={`Breakevens ${terminalName ?? 'of the expiration line'} inside the base range the range mode computes, in SPX points. One marked off-range lies outside it: the engine found it on the wider axis PoP is measured on.`}
              />
              {holdsXsp ? (
                <Tooltip content={XSP_AXIS_NOTE} align="center">
                  <span className="cursor-help text-[10px] text-warn" data-testid="xsp-header-note">
                    XSP in SPX pts
                  </span>
                </Tooltip>
              ) : null}
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
            {baseQuery.isFetching || viewQuery.isFetching || compareQuery.isFetching ? (
              <Spinner label="" />
            ) : null}
            <span className="text-[10px] text-muted">
              {modelOnly
                ? 'model only'
                : `${graphTradeIds.length} trade${graphTradeIds.length === 1 ? '' : 's'}${hasModel ? ' + model' : ''}`}{' '}
              · P&L{' '}
              <span className={`num ${pnlColorClass(stats.pnl_total)}`}>
                {fmtPnl(stats.pnl_total, 0)}
              </span>
            </span>
            <ToggleButton
              pressed={graphMaximized}
              onClick={() => setGraphMaximized(!graphMaximized)}
              title={
                graphMaximized
                  ? onModelTab
                    ? 'Bring the toolbar and the model ticket back (Esc)'
                    : 'Bring the portfolio cards and the table back (Esc)'
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
          {!drawing ? (
            <EmptyGraph onModelTab={onModelTab} hiddenModel={model.legs.length > 0 && !model.include} />
          ) : option && data ? (
            <>
              {/* A transient background-refetch failure keeps the last-drawn
                  curves on screen with an inline note, rather than blanking a
                  graph a trader may be actively watching. */}
              {failing ? (
                <ErrorNote error={failing.error} onRetry={() => void failing.refetch()} />
              ) : null}
              <div className="relative min-h-[500px] flex-1">
                <RiskChart
                  option={option}
                  overlay={overlay}
                  height="fill"
                  onCursorPrice={setCursorPrice}
                  onPan={handlePan}
                  onZoom={handleZoom}
                  onResetView={resetView}
                  pannable={view !== null}
                  canZoomIn={zoom < MAX_ZOOM}
                  canZoomOut={zoom > MIN_ZOOM}
                  ariaLabel={`Risk graph for ${
                    modelOnly ? 'the model alone' : `${graphTradeIds.length} selected trades`
                  }${hasModel && !modelOnly ? ' and the model' : ''}: ${describeLines(visibleLines)}${compareLines ? ', with the position without the model dashed behind' : ''}`}
                />
                <PnlInset data={data} lines={visibleLines} index={cursorIndex} theme={theme} />
              </div>
              <div className="shrink-0">
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
          selected={graphTradeIds}
          holdsXsp={holdsXsp}
          modelLegs={drawnModel}
          compare={compareLines}
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
