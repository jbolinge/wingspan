/** Selected-vs-All portfolio statistics. */

import { usePortfolio } from '../api/queries'
import type { PortfolioStats, Trade } from '../api/types'
import {
  aggregateTrades,
  selectedPlaceholderReference,
  selectedReferenceNote,
  tradesById,
} from '../lib/aggregate'
import { fmtGreek, fmtPnl, pnlColorClass } from '../lib/format'
import { GREEK_UNITS } from './units'
import { InfoTip, Tooltip } from './ui'

function Stat({
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
    <div className="flex flex-col gap-0.5">
      <span className="ws-label flex items-center gap-1">
        {label}
        {tip ? <InfoTip content={tip} /> : null}
      </span>
      <span className={`num text-[13px] font-semibold ${tone ?? 'text-fg'}`}>{value}</span>
    </div>
  )
}

function StatsRow({ stats }: { stats: PortfolioStats }) {
  return (
    <div className="grid grid-cols-4 gap-x-3 gap-y-2 px-2.5 py-2 sm:grid-cols-8">
      <Stat label="Trades" value={String(stats.count)} />
      <Stat
        label="P&L open"
        value={fmtPnl(stats.pnl_open, 0)}
        tone={pnlColorClass(stats.pnl_open)}
        tip="Mark minus average cost over open legs. Excludes realised P&L and fees."
      />
      <Stat
        label="P&L total"
        value={fmtPnl(stats.pnl_total, 0)}
        tone={pnlColorClass(stats.pnl_total)}
        tip="Open P&L plus realised P&L minus fees."
      />
      <Stat label="Δ" value={fmtGreek('delta', stats.greeks.delta)} tip={GREEK_UNITS.delta} />
      <Stat label="Γ" value={fmtGreek('gamma', stats.greeks.gamma)} tip={GREEK_UNITS.gamma} />
      <Stat
        label="Θ"
        value={fmtGreek('theta', stats.greeks.theta)}
        tone={pnlColorClass(stats.greeks.theta)}
        tip={GREEK_UNITS.theta}
      />
      <Stat label="V" value={fmtGreek('vega', stats.greeks.vega)} tip={GREEK_UNITS.vega} />
      <Stat
        label="W.V"
        value={fmtGreek('vega', stats.weighted_vega)}
        tip={`${GREEK_UNITS.weightedVega} Reference in use: ${stats.weighted_vega_reference}.`}
      />
    </div>
  )
}

export function PortfolioCards({
  trades,
  portfolioAll,
  selected,
}: {
  trades: Trade[]
  portfolioAll: PortfolioStats | undefined
  selected: string[]
}) {
  const selectedTrades = tradesById(trades, selected)
  // Rows are measured against the book's reference, so their sum is the
  // Selected W.V against that label until /portfolio answers.
  const localSelected = aggregateTrades(
    selectedTrades,
    selectedPlaceholderReference(
      portfolioAll?.weighted_vega_reference ?? null,
      selectedTrades.length,
    ),
  )
  const remote = usePortfolio(selected)
  const selectedStats = remote.data ?? localSelected
  const selectedNote = selectedReferenceNote(
    selectedStats.weighted_vega_reference,
    remote.data === undefined,
  )
  const allStats = portfolioAll ?? aggregateTrades(trades)

  return (
    <div className="grid shrink-0 grid-cols-1 gap-2 lg:grid-cols-2">
      <section className="ws-card">
        <header className="flex items-center justify-between border-b border-line px-2.5 py-1">
          <h2 className="ws-label m-0 font-semibold text-accent">Selected</h2>
          <Tooltip
            align="right"
            content={`Weighted-vega reference: ${selectedStats.weighted_vega_reference}. ${selectedNote}`.trimEnd()}
          >
            <span className="cursor-help text-[10px] text-muted">
              W.V ref {selectedStats.weighted_vega_reference}
            </span>
          </Tooltip>
        </header>
        {selected.length === 0 ? (
          <div className="px-2.5 py-3 text-muted">
            Nothing selected — tick trades in the table to build the analysis set.
          </div>
        ) : (
          <StatsRow stats={selectedStats} />
        )}
      </section>

      <section className="ws-card">
        <header className="flex items-center justify-between border-b border-line px-2.5 py-1">
          <h2 className="ws-label m-0 font-semibold">All open trades</h2>
          <Tooltip
            align="right"
            content={`Weighted-vega reference: ${allStats.weighted_vega_reference}. Every row in the positions table is measured against it.`}
          >
            <span className="cursor-help text-[10px] text-muted">
              W.V ref {allStats.weighted_vega_reference}
            </span>
          </Tooltip>
        </header>
        <StatsRow stats={allStats} />
      </section>
    </div>
  )
}
