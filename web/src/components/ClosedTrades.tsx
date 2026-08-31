/** Closed-trades tab: realised P&L, fees, duration and the running totals. */

import { Fragment } from 'react'
import { useClosedTrades } from '../api/queries'
import { formatDate } from '../lib/dates'
import { fmtMoney, fmtPnl, fmtRatioPct, pnlColorClass } from '../lib/format'
import { sortTrades, warningCount } from '../lib/aggregate'
import { useAppStore } from '../store/appStore'
import { TradeDetail } from './TradeDetail'
import { Card, EmptyState, ErrorNote, Pill, Spinner } from './ui'

export function ClosedTrades() {
  const query = useClosedTrades()
  const expanded = useAppStore((s) => s.expanded)
  const toggleExpanded = useAppStore((s) => s.toggleExpanded)

  if (query.isLoading) {
    return (
      <Card title="Closed trades">
        <div className="p-3">
          <Spinner />
        </div>
      </Card>
    )
  }
  if (query.isError) {
    return (
      <Card title="Closed trades">
        <ErrorNote error={query.error} onRetry={() => void query.refetch()} />
      </Card>
    )
  }

  const trades = sortTrades(query.data?.trades ?? [], 'opened_on', 'desc')
  const totals = query.data?.totals

  return (
    <Card
      title="Closed trades"
      actions={
        totals ? (
          <div className="flex items-center gap-3 text-[11px]">
            <span className="text-muted">
              count <span className="num text-fg">{totals.count}</span>
            </span>
            <span className="text-muted">
              win rate <span className="num text-fg">{fmtRatioPct(totals.win_rate, 0)}</span>
            </span>
            <span className="text-muted">
              avg held <span className="num text-fg">{totals.avg_days_held}d</span>
            </span>
            <span className="text-muted">
              fees <span className="num text-fg">{fmtMoney(totals.fees)}</span>
            </span>
            <span className="text-muted">
              realised{' '}
              <span className={`num font-semibold ${pnlColorClass(totals.realized)}`}>
                {fmtPnl(totals.realized, 2)}
              </span>
            </span>
          </div>
        ) : null
      }
      bodyClassName="overflow-auto"
    >
      {trades.length === 0 ? (
        <EmptyState>No closed trades in the “Closed Trades” folder.</EmptyState>
      ) : (
        <table className="w-full border-collapse text-[12px]">
          <thead>
            <tr className="text-left text-muted">
              <th className="w-6 px-1 py-1" />
              <th className="px-2 py-1 font-medium">Name</th>
              <th className="px-2 py-1 font-medium">Opened</th>
              <th className="px-2 py-1 font-medium">Closed</th>
              <th className="px-2 py-1 text-right font-medium">Days</th>
              <th className="px-2 py-1 font-medium">Structure</th>
              <th className="px-2 py-1 text-right font-medium">Realised</th>
              <th className="px-2 py-1 text-right font-medium">Fees</th>
              <th className="px-2 py-1 text-right font-medium">Net P&amp;L</th>
              <th className="px-2 py-1 text-right font-medium">⚠</th>
            </tr>
          </thead>
          <tbody>
            {trades.map((trade) => {
              const isOpen = expanded.includes(trade.id)
              const warnings = warningCount(trade)
              return (
                <Fragment key={trade.id}>
                  <tr className="border-t border-line/60 hover:bg-surface-2/70">
                    <td className="px-1 py-1">
                      <button
                        type="button"
                        className="w-4 bg-transparent text-muted hover:text-fg"
                        aria-expanded={isOpen}
                        aria-label={`${isOpen ? 'Collapse' : 'Expand'} ${trade.name}`}
                        onClick={() => toggleExpanded(trade.id)}
                      >
                        {isOpen ? '▾' : '▸'}
                      </button>
                    </td>
                    <td className="px-2 py-1 font-semibold text-fg-strong">{trade.name}</td>
                    <td className="num px-2 py-1 text-muted">{formatDate(trade.opened_on)}</td>
                    <td className="num px-2 py-1 text-muted">{formatDate(trade.closed_on)}</td>
                    <td className="num px-2 py-1 text-right">{trade.dit}</td>
                    <td className="px-2 py-1 text-muted">{trade.structure}</td>
                    <td className={`num px-2 py-1 text-right ${pnlColorClass(trade.realized)}`}>
                      {fmtPnl(trade.realized)}
                    </td>
                    <td className="num px-2 py-1 text-right text-muted">{fmtMoney(trade.fees)}</td>
                    <td
                      className={`num px-2 py-1 text-right font-semibold ${pnlColorClass(
                        trade.pnl_total,
                      )}`}
                    >
                      {fmtPnl(trade.pnl_total)}
                    </td>
                    <td className="px-2 py-1 text-right">
                      {warnings > 0 ? (
                        <Pill tone={trade.quarantined.length > 0 ? 'neg' : 'warn'}>
                          ⚠ {warnings}
                        </Pill>
                      ) : null}
                    </td>
                  </tr>
                  {isOpen ? (
                    <tr>
                      <td colSpan={10} className="p-0">
                        <TradeDetail trade={trade} />
                      </td>
                    </tr>
                  ) : null}
                </Fragment>
              )
            })}
          </tbody>
        </table>
      )}
    </Card>
  )
}
