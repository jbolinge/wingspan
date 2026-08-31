/** The ONE-style risk graph: chart, crosshair readout, slices and controls. */

import { useMemo, useState } from 'react'
import { usePortfolio, useRiskGraph } from '../../api/queries'
import type { PortfolioStats, Trade } from '../../api/types'
import { aggregateTrades, expiriesOf, tradesById } from '../../lib/aggregate'
import { buildRiskChartOption } from '../../lib/chartOption'
import { daysBetween, todayIso } from '../../lib/dates'
import { fmtNumber, fmtPnl, fmtRatioPct, pnlColorClass } from '../../lib/format'
import { buildRiskGraphRequest } from '../../lib/graphRequest'
import { chartPalette } from '../../lib/palette'
import { nearestPriceIndex } from '../../lib/readout'
import { useAppStore } from '../../store/appStore'
import { EmptyState, ErrorNote, Spinner, Tooltip } from '../ui'
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
  const [cursorPrice, setCursorPrice] = useState<number | null>(null)

  const request = useMemo(() => buildRiskGraphRequest(selected, graph), [selected, graph])
  const query = useRiskGraph(request, true)
  const data = query.data

  const selectedTrades = tradesById(trades, selected)
  const remoteStats = usePortfolio(selected)
  const stats: PortfolioStats = remoteStats.data ?? aggregateTrades(selectedTrades)

  const expiries = expiriesOf(selectedTrades)
  const front = expiries[0] ?? null
  const frontDte = front ? daysBetween(todayIso(), front) : null

  const visibleLines = useMemo(
    () => (data?.lines ?? []).filter((line) => !graph.hiddenLines.includes(line.date)),
    [data, graph.hiddenLines],
  )

  const option = useMemo(() => {
    if (!data) return null
    return buildRiskChartOption({
      data,
      visibleLines,
      perTrade: graph.perTradeOverlay
        ? data.per_trade.map((series) => ({
            tradeId: series.trade_id,
            name: trades.find((t) => t.id === series.trade_id)?.name ?? series.trade_id,
            pnl: series.pnl,
          }))
        : [],
      palette: chartPalette(theme),
      showSigmaBands: graph.showSigmaBands,
      showBreakevens: graph.showBreakevens,
      yLock:
        graph.yLock && graph.yMin !== null && graph.yMax !== null
          ? { min: graph.yMin, max: graph.yMax }
          : null,
    })
  }, [data, visibleLines, graph, theme, trades])

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
                tip="Highest P&L reached by the expiration line inside the plotted price range."
              />
              <Readout
                label="max loss"
                value={fmtPnl(data.max_loss_on_range, 0)}
                tone={pnlColorClass(data.max_loss_on_range)}
                tip="Lowest P&L reached by the expiration line inside the plotted price range — not a guaranteed floor beyond it."
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
                tip="Breakevens of the expiration line inside the plotted range."
              />
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
          </span>
        </header>

        <div className="min-h-0 flex-1 overflow-y-auto p-2">
          {selected.length === 0 ? (
            <EmptyState>
              Select trades in the table to draw the expiration line and the T+x ladder for that
              set.
            </EmptyState>
          ) : query.isError ? (
            <ErrorNote error={query.error} onRetry={() => void query.refetch()} />
          ) : option && data ? (
            <div className="space-y-2">
              <RiskChart
                option={option}
                height={380}
                onCursorPrice={setCursorPrice}
                ariaLabel={`Risk graph for ${selected.length} selected trades: expiration line plus ${
                  visibleLines.length - 1
                } T+x lines`}
              />
              <div className="grid grid-cols-1 gap-2 lg:grid-cols-2">
                <CrosshairReadout
                  data={data}
                  lines={visibleLines}
                  index={cursorIndex}
                  theme={theme}
                />
                {graph.showSliceTable ? <SliceTable data={data} lines={visibleLines} /> : null}
              </div>
            </div>
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
          baseline={{ pnl: stats.pnl_total, greeks: stats.greeks }}
        />
      </div>
    </div>
  )
}
