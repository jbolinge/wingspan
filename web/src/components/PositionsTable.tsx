/** The open-positions blotter. Every column from the plan, sortable, with
 *  expandable leg detail and an explicit selection model. */

import { Fragment, type ReactNode } from 'react'
import type { Trade } from '../api/types'
import {
  invertIds,
  selectAllIds,
  sortTrades,
  warningCount,
  worstSeverity,
  type SortKey,
} from '../lib/aggregate'
import { formatDate } from '../lib/dates'
import { fmtGreek, fmtMoney, fmtPct, fmtPnl, pnlColorClass } from '../lib/format'
import { useAppStore } from '../store/appStore'
import { TradeDetail } from './TradeDetail'
import { COLUMN_UNITS, GREEK_UNITS } from './units'
import { EmptyState, InfoTip, Pill, StalenessDot, Tooltip } from './ui'

interface Column {
  key: SortKey
  label: string
  tip?: string
  align?: 'left' | 'right'
  width?: string
}

const COLUMNS: Column[] = [
  { key: 'name', label: 'Name', align: 'left' },
  { key: 'opened_on', label: 'Opened', align: 'left' },
  { key: 'dit', label: 'DIT', align: 'right', tip: COLUMN_UNITS.dit },
  { key: 'dte', label: 'DTE', align: 'right', tip: COLUMN_UNITS.dte },
  { key: 'structure', label: 'Structure', align: 'left', tip: COLUMN_UNITS.structure },
  { key: 'cost', label: 'Cost', align: 'right', tip: COLUMN_UNITS.cost },
  { key: 'value', label: 'Value', align: 'right', tip: COLUMN_UNITS.value },
  { key: 'pnl_open', label: 'P&L open', align: 'right', tip: COLUMN_UNITS.pnlOpen },
  { key: 'pnl_total', label: 'P&L total', align: 'right', tip: COLUMN_UNITS.pnlTotal },
  { key: 'pnl_pct', label: '%', align: 'right', tip: COLUMN_UNITS.pnlPct },
  { key: 'delta', label: 'Δ', align: 'right', tip: GREEK_UNITS.delta },
  { key: 'gamma', label: 'Γ', align: 'right', tip: GREEK_UNITS.gamma },
  { key: 'theta', label: 'Θ', align: 'right', tip: GREEK_UNITS.theta },
  { key: 'vega', label: 'V', align: 'right', tip: GREEK_UNITS.vega },
  { key: 'weighted_vega', label: 'W.V', align: 'right', tip: GREEK_UNITS.weightedVega },
  { key: 'staleness_secs', label: '●', align: 'right', tip: COLUMN_UNITS.staleness },
  { key: 'warnings', label: '⚠', align: 'right', tip: 'Warnings and quarantined orders on this trade.' },
]

function HeaderCell({
  column,
  sortKey,
  sortDir,
  onSort,
  extra,
}: {
  column: Column
  sortKey: SortKey
  sortDir: 'asc' | 'desc'
  onSort: (key: SortKey) => void
  extra?: ReactNode
}) {
  const active = sortKey === column.key
  return (
    <th
      scope="col"
      className={`sticky top-0 z-10 bg-surface-2 px-2 py-1 font-medium whitespace-nowrap ${
        column.align === 'right' ? 'text-right' : 'text-left'
      }`}
      aria-sort={active ? (sortDir === 'asc' ? 'ascending' : 'descending') : 'none'}
    >
      <button
        type="button"
        className="inline-flex items-center gap-1 bg-transparent text-[10px] tracking-wide text-muted uppercase hover:text-fg"
        onClick={() => onSort(column.key)}
        title={`Sort by ${column.label}`}
      >
        {column.label}
        <span aria-hidden="true" className={active ? 'text-accent' : 'text-transparent'}>
          {sortDir === 'asc' ? '▲' : '▼'}
        </span>
      </button>
      {column.tip ? <InfoTip content={column.tip} /> : null}
      {extra}
    </th>
  )
}

export function PositionsTable({
  trades,
  weightedVegaReference,
}: {
  trades: Trade[]
  weightedVegaReference: string | undefined
}) {
  const selected = useAppStore((s) => s.selected)
  const setSelected = useAppStore((s) => s.setSelected)
  const toggleSelected = useAppStore((s) => s.toggleSelected)
  const expanded = useAppStore((s) => s.expanded)
  const toggleExpanded = useAppStore((s) => s.toggleExpanded)
  const sortKey = useAppStore((s) => s.sortKey)
  const sortDir = useAppStore((s) => s.sortDir)
  const setSort = useAppStore((s) => s.setSort)

  const rows = sortTrades(trades, sortKey, sortDir)
  const allSelected = trades.length > 0 && selected.length === trades.length
  const someSelected = selected.length > 0 && !allSelected

  if (trades.length === 0) {
    return (
      <EmptyState>
        No open trades. Point <code className="num">[trades] root</code> at the folder holding
        “Open Trades”, “Closed Trades” and “Daily Trade Statement”.
      </EmptyState>
    )
  }

  return (
    <div className="flex min-h-0 flex-col">
      <div className="flex shrink-0 items-center gap-2 border-b border-line px-2 py-1">
        <span className="ws-label">Selection</span>
        <button type="button" className="ws-btn" onClick={() => setSelected(selectAllIds(trades))}>
          all
        </button>
        <button type="button" className="ws-btn" onClick={() => setSelected([])}>
          none
        </button>
        <button
          type="button"
          className="ws-btn"
          onClick={() => setSelected(invertIds(trades, selected))}
        >
          invert
        </button>
        <span className="num text-[11px] text-muted">
          {selected.length} of {trades.length} selected
        </span>
        {weightedVegaReference ? (
          <span className="ml-auto text-[10px] text-muted">
            W.V reference: <span className="num text-fg">{weightedVegaReference}</span>
          </span>
        ) : null}
      </div>

      <div className="min-h-0 flex-1 overflow-auto">
        <table className="w-full border-collapse text-[12px]">
          <caption className="sr-only">Open option trades with P&amp;L and position greeks</caption>
          <thead>
            <tr>
              <th scope="col" className="sticky top-0 z-10 w-8 bg-surface-2 px-2 py-1">
                <input
                  type="checkbox"
                  aria-label="Select all trades"
                  checked={allSelected}
                  ref={(node) => {
                    if (node) node.indeterminate = someSelected
                  }}
                  onChange={() => setSelected(allSelected ? [] : selectAllIds(trades))}
                />
              </th>
              <th scope="col" className="sticky top-0 z-10 w-6 bg-surface-2" />
              {COLUMNS.map((column) => (
                <HeaderCell
                  key={column.key}
                  column={column}
                  sortKey={sortKey}
                  sortDir={sortDir}
                  onSort={setSort}
                />
              ))}
            </tr>
          </thead>
          <tbody>
            {rows.map((trade) => {
              const isOpen = expanded.includes(trade.id)
              const isSelected = selected.includes(trade.id)
              const warnings = warningCount(trade)
              const severity = worstSeverity(trade)
              return (
                <Fragment key={trade.id}>
                  <tr
                    className={`border-t border-line/60 hover:bg-surface-2/70 ${
                      isSelected ? 'bg-accent-soft' : ''
                    }`}
                  >
                    <td className="px-2 py-1">
                      <input
                        type="checkbox"
                        aria-label={`Select ${trade.name}`}
                        checked={isSelected}
                        onChange={() => toggleSelected(trade.id)}
                      />
                    </td>
                    <td className="px-1 py-1">
                      <button
                        type="button"
                        className="w-4 bg-transparent text-muted hover:text-fg"
                        aria-expanded={isOpen}
                        aria-label={`${isOpen ? 'Collapse' : 'Expand'} ${trade.name} legs`}
                        onClick={() => toggleExpanded(trade.id)}
                      >
                        {isOpen ? '▾' : '▸'}
                      </button>
                    </td>
                    <td className="px-2 py-1 font-semibold text-fg-strong">{trade.name}</td>
                    <td className="num px-2 py-1 text-muted">{formatDate(trade.opened_on)}</td>
                    <td className="num px-2 py-1 text-right">{trade.dit}</td>
                    <td className="num px-2 py-1 text-right">{trade.dte ?? '—'}</td>
                    <td className="px-2 py-1 whitespace-nowrap text-muted">
                      <Tooltip content={trade.structure}>
                        <span className="block max-w-[26rem] truncate">{trade.structure}</span>
                      </Tooltip>
                    </td>
                    <td className="num px-2 py-1 text-right text-muted">
                      {fmtMoney(trade.cost, { dp: 0 })}
                    </td>
                    <td className="num px-2 py-1 text-right">{fmtMoney(trade.value, { dp: 0 })}</td>
                    <td className={`num px-2 py-1 text-right ${pnlColorClass(trade.pnl_open)}`}>
                      {fmtPnl(trade.pnl_open, 0)}
                    </td>
                    <td
                      className={`num px-2 py-1 text-right font-semibold ${pnlColorClass(
                        trade.pnl_total,
                      )}`}
                    >
                      {fmtPnl(trade.pnl_total, 0)}
                    </td>
                    <td className={`num px-2 py-1 text-right ${pnlColorClass(trade.pnl_pct)}`}>
                      {fmtPct(trade.pnl_pct)}
                    </td>
                    <td className="num px-2 py-1 text-right">
                      {fmtGreek('delta', trade.greeks.delta)}
                    </td>
                    <td className="num px-2 py-1 text-right">
                      {fmtGreek('gamma', trade.greeks.gamma)}
                    </td>
                    <td
                      className={`num px-2 py-1 text-right ${pnlColorClass(trade.greeks.theta)}`}
                    >
                      {fmtGreek('theta', trade.greeks.theta)}
                    </td>
                    <td className="num px-2 py-1 text-right">
                      {fmtGreek('vega', trade.greeks.vega)}
                    </td>
                    <td className="num px-2 py-1 text-right">
                      {fmtGreek('vega', trade.weighted_vega)}
                    </td>
                    <td className="px-2 py-1 text-right">
                      <StalenessDot seconds={trade.staleness_secs} />
                    </td>
                    <td className="px-2 py-1 text-right">
                      {warnings > 0 ? (
                        <Tooltip
                          align="right"
                          content={
                            <ul className="m-0 list-disc space-y-1 pl-4">
                              {trade.warnings.map((w) => (
                                <li key={w.code + w.message}>
                                  <span className="num">{w.code}</span> — {w.message}
                                </li>
                              ))}
                              {trade.quarantined.length > 0 ? (
                                <li>
                                  {trade.quarantined.length} quarantined order
                                  {trade.quarantined.length === 1 ? '' : 's'} — expand the row for
                                  the fix.
                                </li>
                              ) : null}
                            </ul>
                          }
                        >
                          <Pill
                            tone={
                              severity === 'error' ? 'neg' : severity === 'warn' ? 'warn' : 'info'
                            }
                            title={`${warnings} warning${warnings === 1 ? '' : 's'} on ${trade.name}`}
                          >
                            ⚠ {warnings}
                          </Pill>
                        </Tooltip>
                      ) : null}
                    </td>
                  </tr>
                  {isOpen ? (
                    <tr>
                      <td colSpan={COLUMNS.length + 2} className="p-0">
                        <TradeDetail trade={trade} />
                      </td>
                    </tr>
                  ) : null}
                </Fragment>
              )
            })}
          </tbody>
        </table>
      </div>
    </div>
  )
}
