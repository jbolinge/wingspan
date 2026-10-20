/** What-if: price / date / vol, answered by POST /what-if so the greeks come
 *  from the same engine as the curves rather than from a chart read-off. */

import { useState } from 'react'
import { useWhatIf } from '../../api/queries'
import type { Greeks, IsoDate, ModelLeg } from '../../api/types'
import { formatDate, todayIso } from '../../lib/dates'
import { fmtGreek, fmtNumber, fmtPnl, pnlColorClass } from '../../lib/format'
import { buildWhatIfRequest } from '../../lib/graphRequest'
import { useAppStore } from '../../store/appStore'
import { GREEK_UNITS, SPX_POINTS } from '../units'
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

/** What the vol slider adds to, and what "vs now" is measured against. */
export const WHAT_IF_VOL_NOTE =
  'Vol is added on top of the graph’s vol shifts (global and per-expiry), so the answer sits on the curves beside it.'
export const WHAT_IF_BASELINE_NOTE =
  'The change beside the P&L is measured against the position’s current P&L — today, at the live marks, with no vol shift at all.'
/** The what-if price is a price of the x-axis: SPX points, even for XSP legs. */
export const WHAT_IF_PRICE_NOTE =
  'What-if price in SPX points — the risk graph’s x-axis — even for XSP legs (XSP 650 is SPX 6500).'

/** What a what-if dated on an expiry means: the expiration itself, as the
 *  expiration line prices it — not that day's open with time value left. */
export const WHAT_IF_EXPIRY_NOTE =
  'A date that is an expiry prices at that expiration: its legs at settlement (intrinsic), exactly as the expiration line does; later legs keep their time value to that close. Dates before the server’s date cannot be priced.'

export function WhatIfPanel({
  selected,
  modelLegs = [],
  spot,
  baseline,
  today: serverToday,
  expiries = [],
}: {
  selected: string[]
  /** The modeled legs the graph draws, so the answer is for the same position. */
  modelLegs?: ModelLeg[]
  spot: number | null
  baseline: { pnl: number; greeks: Greeks } | null
  /** The server's date (the T+0 line's); the browser's day only until a
   *  response has said. */
  today?: IsoDate | null
  /** The live expiries in the drawn position, so the panel can say when the
   *  chosen date is one. */
  expiries?: readonly IsoDate[]
}) {
  const graph = useAppStore((s) => s.graph)
  const patchWhatIf = useAppStore((s) => s.patchWhatIf)
  const whatIf = graph.whatIf
  const today = serverToday ?? todayIso()

  const request = buildWhatIfRequest(selected, graph, spot, modelLegs, today)
  const query = useWhatIf(request, whatIf.enabled)
  const price = whatIf.price ?? graph.spotOverride ?? spot
  const result = query.data ?? null
  // The date actually priced: the request clamps a stored date that is now
  // before the server's (`buildWhatIfRequest`), so show the same one.
  const pricedDate = request?.date ?? (whatIf.date !== null && whatIf.date >= today ? whatIf.date : today)
  const onExpiry = expiries.includes(pricedDate)

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
          <label className="flex items-center gap-1.5" title={WHAT_IF_PRICE_NOTE}>
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
            <span className="shrink-0 text-[10px] text-muted">{SPX_POINTS}</span>
          </label>

          <label className="flex items-center gap-1.5">
            <span className="ws-label w-10 shrink-0 text-right">date</span>
            <input
              className="ws-input"
              type="date"
              min={today}
              value={pricedDate}
              aria-label="What-if date"
              title={WHAT_IF_EXPIRY_NOTE}
              onChange={(event) => {
                const value = event.target.value
                // The input's `min` is advisory — a typed date can still be
                // earlier, and the engine refuses one before its own date.
                patchWhatIf({ date: value === '' || value < today ? null : value })
              }}
            />
            <span className="text-[10px] text-muted">{formatDate(pricedDate)}</span>
          </label>
          <p className="m-0 text-[10px] text-muted" data-testid="what-if-expiry-note">
            {onExpiry
              ? `${formatDate(pricedDate)} is an expiry: priced at that expiration’s settlement, as the expiration line is.`
              : WHAT_IF_EXPIRY_NOTE}
          </p>

          <label className="flex items-center gap-1.5" title={WHAT_IF_VOL_NOTE}>
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
          <p className="m-0 text-[10px] text-muted">
            {WHAT_IF_VOL_NOTE} {WHAT_IF_BASELINE_NOTE}
          </p>

          <div className="rounded border border-line bg-surface-2 px-2 py-1.5">
            {result ? (
              <>
                <div className="flex items-baseline gap-2">
                  <span className="ws-label">P&L</span>
                  <span className={`num text-[14px] font-semibold ${pnlColorClass(result.pnl)}`}>
                    {fmtPnl(result.pnl, 0)}
                  </span>
                  {baseline ? (
                    <span
                      className={`num text-[11px] ${pnlColorClass(result.pnl - baseline.pnl)}`}
                      title={WHAT_IF_BASELINE_NOTE}
                    >
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
