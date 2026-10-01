/** What-if: price / date / vol, answered by POST /what-if so the greeks come
 *  from the same engine as the curves rather than from a chart read-off. */

import { useState } from 'react'
import { useWhatIf } from '../../api/queries'
import type { Greeks, ModelLeg } from '../../api/types'
import { formatDate, todayIso } from '../../lib/dates'
import { fmtGreek, fmtNumber, fmtPnl, pnlColorClass } from '../../lib/format'
import { buildWhatIfRequest } from '../../lib/graphRequest'
import { useAppStore } from '../../store/appStore'
import { GREEK_UNITS } from '../units'
import { InfoTip, Spinner, ToggleButton } from '../ui'

function GreekCell({ label, value, tip }: { label: string; value: number; tip: string }) {
  return (
    <div className="flex flex-col">
      <span className="ws-label flex items-center gap-0.5">
        {label}
        <InfoTip content={tip} />
      </span>
      <span className="num text-[12px] text-fg">{fmtGreek(label === 'Γ' ? 'gamma' : label === 'Δ' ? 'delta' : label === 'Θ' ? 'theta' : 'vega', value)}</span>
    </div>
  )
}

export function WhatIfPanel({
  selected,
  modelLegs = [],
  spot,
  baseline,
}: {
  selected: string[]
  /** The modeled legs the graph draws, so the answer is for the same position. */
  modelLegs?: ModelLeg[]
  spot: number | null
  baseline: { pnl: number; greeks: Greeks } | null
}) {
  const graph = useAppStore((s) => s.graph)
  const patchWhatIf = useAppStore((s) => s.patchWhatIf)
  const whatIf = graph.whatIf
  const today = todayIso()

  const request = buildWhatIfRequest(selected, graph, spot, modelLegs)
  const query = useWhatIf(request, whatIf.enabled)
  const price = whatIf.price ?? graph.spotOverride ?? spot
  const result = query.data ?? null

  // Buffers the number input so an emptied field commits `null` ("follow
  // spot") instead of `Number('')` === 0 — mirrors NumberField's draft/commit.
  const [priceDraft, setPriceDraft] = useState<string | null>(null)
  const priceShown = priceDraft ?? (price === null ? '' : String(price))
  const commitPriceDraft = (): void => {
    if (priceDraft === null) return
    const trimmed = priceDraft.trim()
    setPriceDraft(null)
    if (trimmed === '') {
      patchWhatIf({ price: null })
      return
    }
    const parsed = Number(trimmed)
    if (Number.isFinite(parsed)) patchWhatIf({ price: parsed })
  }

  return (
    <div className="space-y-1.5">
      <div className="flex items-center gap-2">
        <ToggleButton
          pressed={whatIf.enabled}
          onClick={() => patchWhatIf({ enabled: !whatIf.enabled })}
          title="Recompute P&L and greeks at an arbitrary price, date and vol"
        >
          what-if {whatIf.enabled ? 'on' : 'off'}
        </ToggleButton>
        {whatIf.enabled && query.isFetching ? <Spinner label="" /> : null}
      </div>

      {whatIf.enabled ? (
        <>
          <label className="flex items-center gap-1.5">
            <span className="ws-label w-10 shrink-0 text-right">price</span>
            <input
              className="ws-range flex-1"
              type="range"
              min={spot ? spot * 0.8 : 0}
              max={spot ? spot * 1.2 : 100}
              step={1}
              value={price ?? 0}
              aria-label="What-if price"
              onChange={(event) => patchWhatIf({ price: Number(event.target.value) })}
            />
            <input
              className="ws-input w-20"
              type="number"
              step={1}
              value={priceShown}
              aria-label="What-if price value"
              onChange={(event) => setPriceDraft(event.target.value)}
              onBlur={commitPriceDraft}
              onKeyDown={(event) => {
                if (event.key === 'Enter') commitPriceDraft()
                if (event.key === 'Escape') setPriceDraft(null)
              }}
            />
          </label>

          <label className="flex items-center gap-1.5">
            <span className="ws-label w-10 shrink-0 text-right">date</span>
            <input
              className="ws-input"
              type="date"
              min={today}
              value={whatIf.date ?? today}
              aria-label="What-if date"
              onChange={(event) => patchWhatIf({ date: event.target.value })}
            />
            <span className="text-[10px] text-muted">{formatDate(whatIf.date ?? today)}</span>
          </label>

          <label className="flex items-center gap-1.5">
            <span className="ws-label w-10 shrink-0 text-right">vol</span>
            <input
              className="ws-range flex-1"
              type="range"
              min={-0.1}
              max={0.1}
              step={0.005}
              value={whatIf.volShift}
              aria-label="What-if vol shift"
              onChange={(event) => patchWhatIf({ volShift: Number(event.target.value) })}
            />
            <span className="num w-14 text-[11px]">
              {whatIf.volShift >= 0 ? '+' : ''}
              {fmtNumber(whatIf.volShift * 100, 1)} vp
            </span>
          </label>

          <div className="rounded border border-line bg-surface-2 px-2 py-1.5">
            {result ? (
              <>
                <div className="flex items-baseline gap-2">
                  <span className="ws-label">P&L</span>
                  <span className={`num text-[14px] font-semibold ${pnlColorClass(result.pnl)}`}>
                    {fmtPnl(result.pnl, 0)}
                  </span>
                  {baseline ? (
                    <span className={`num text-[11px] ${pnlColorClass(result.pnl - baseline.pnl)}`}>
                      ({fmtPnl(result.pnl - baseline.pnl, 0)} vs now)
                    </span>
                  ) : null}
                </div>
                <div className="mt-1 grid grid-cols-4 gap-2">
                  <GreekCell label="Δ" value={result.greeks.delta} tip={GREEK_UNITS.delta} />
                  <GreekCell label="Γ" value={result.greeks.gamma} tip={GREEK_UNITS.gamma} />
                  <GreekCell label="Θ" value={result.greeks.theta} tip={GREEK_UNITS.theta} />
                  <GreekCell label="V" value={result.greeks.vega} tip={GREEK_UNITS.vega} />
                </div>
              </>
            ) : (
              <span className="text-muted">
                {selected.length === 0 && modelLegs.length === 0 ? 'Select at least one trade.' : 'Computing…'}
              </span>
            )}
          </div>
        </>
      ) : null}
    </div>
  )
}
