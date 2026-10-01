/**
 * The model ticket: every modeled leg as an order line
 * (`−1 × SPX 16 OCT 26 7500 P @ 42.50`), each removable, and what the model
 * adds up to — net premium at the current marks and, while it is drawn, its
 * own greeks from the same engine as the graph (`portfolio` with no trades).
 *
 * A right rail beside the chain from 1400 px up; above it, compact, on a
 * narrower window.
 */

import { useMemo } from 'react'
import { errorDetail } from '../../api/client'
import { useModelSyncStatus, usePortfolio, useValuationDate } from '../../api/queries'
import { fmtNumber, fmtPrice } from '../../lib/format'
import {
  contractKey,
  fmtContract,
  fmtNetCost,
  fmtSignedQty,
  isLiveModelLeg,
  modelNetCost,
  toModelLegs,
} from '../../lib/model'
import { MAX_MODEL_LEGS, useAppStore, type ModelLeg } from '../../store/appStore'
import { GREEK_UNITS } from '../units'

/** A model is usually a lot or two, so its greeks get two decimals (four for
 *  gamma) where the book's whole-dollar theta and vega would print `-0`. */
const GREEKS = [
  ['delta', 'Δ', 2],
  ['gamma', 'Γ', 4],
  ['theta', 'Θ', 2],
  ['vega', 'V', 2],
] as const

export function ModelTicket({
  legs,
  marks,
  include,
}: {
  legs: readonly ModelLeg[]
  /** Marks by TOS code from the loaded chains; absent = not loaded yet. */
  marks: ReadonlyMap<string, number | null>
  include: boolean
}) {
  const removeModelLeg = useAppStore((s) => s.removeModelLeg)
  const sync = useModelSyncStatus()
  // The greeks are asked for on the legs still live on the server's date —
  // none until the first snapshot gives that date (the dashes stay up) — so
  // a leg that expired overnight cannot get the request refused.
  const valuationDate = useValuationDate()
  const request = useMemo(
    () =>
      valuationDate === null ? [] : toModelLegs(legs.filter((leg) => isLiveModelLeg(leg, valuationDate))),
    [legs, valuationDate],
  )
  // Only while the model is drawn: hidden, it is parked, and greeks for a
  // position the graph is not showing would be a number with nothing to
  // compare against.
  const stats = usePortfolio([], include ? request : [])
  const net = modelNetCost(legs, marks)

  return (
    <section aria-label="Model ticket" className="ws-card flex min-h-0 flex-1 flex-col min-[1400px]:flex-none">
      <header className="flex shrink-0 items-center gap-2 border-b border-line px-2.5 py-1.5">
        <h2 className="ws-label m-0 font-semibold">Model ticket</h2>
        <span className="num text-[10px] text-muted">
          {legs.length} leg{legs.length === 1 ? '' : 's'}
        </span>
      </header>
      {/* Its own line, sentence case and truncated (the whole story on hover):
          as a pill in the 18rem rail it wrapped to three uppercase lines and
          squeezed the heading. Red when the server refused the set — then the
          legs are not priced at all — amber when they are, off a neighbour. */}
      {sync.message !== null ? (
        <p
          role="status"
          className={`m-0 shrink-0 truncate border-b border-line px-2.5 py-1 text-[10px] ${
            sync.state === 'refused' ? 'text-neg' : 'text-warn'
          }`}
          title={sync.detail ?? undefined}
        >
          <span aria-hidden="true">⚠ </span>
          {sync.message}
        </p>
      ) : null}

      {legs.length === 0 ? (
        <p className="m-0 px-2.5 py-3 text-[11px] text-muted">
          No modeled legs. Type a quantity in a chain&apos;s <span className="text-fg">Model</span> column —{' '}
          <span className="num">+1</span> buys one, <span className="num">−1</span> sells one — then look at
          the risk graph on Positions.
        </p>
      ) : (
        <>
          <ul className="m-0 flex min-h-0 list-none flex-wrap gap-x-4 overflow-y-auto p-0 px-2.5 py-1.5 min-[1400px]:flex-col min-[1400px]:flex-nowrap">
            {legs.map((leg) => {
              const mark = marks.get(contractKey(leg.contract))
              const label = fmtContract(leg.contract)
              return (
                <li key={contractKey(leg.contract)} className="group flex min-w-0 items-center gap-1 py-0.5">
                  <span
                    className={`num w-7 shrink-0 text-right text-[11px] font-semibold ${
                      leg.qty > 0 ? 'text-pos' : 'text-neg'
                    }`}
                  >
                    {fmtSignedQty(leg.qty)}
                  </span>
                  <span className="num shrink-0 text-[11px] text-muted">×</span>
                  {/* The label gives way first (full text on hover), so a long
                      SPXW symbol or a four-figure mark never pushes the ✕ out
                      of the rail. */}
                  <span className="num min-w-0 truncate text-[11px] text-fg-strong" title={label}>
                    {label}
                  </span>
                  <span className="num shrink-0 text-[11px] whitespace-nowrap text-muted">
                    @ {mark === undefined || mark === null ? '—' : fmtPrice(mark)}
                  </span>
                  <button
                    type="button"
                    className="ml-auto shrink-0 bg-transparent px-1 text-[11px] text-faint hover:text-neg"
                    aria-label={`Remove ${label} from the model`}
                    title="Remove this leg"
                    onClick={() => removeModelLeg(contractKey(leg.contract))}
                  >
                    ✕
                  </button>
                </li>
              )
            })}
          </ul>
          {/* One line above the chain on a narrow window; a stacked block in
              the rail. */}
          <footer className="flex shrink-0 flex-wrap items-baseline gap-x-6 gap-y-1 border-t border-line px-2.5 py-1.5 min-[1400px]:block min-[1400px]:space-y-1">
            <div className="flex items-baseline gap-2 min-[1400px]:justify-between">
              <span className="ws-label">Net premium</span>
              <span
                className={`num text-[12px] font-semibold ${
                  net === null ? 'text-muted' : net > 0 ? 'text-neg' : net < 0 ? 'text-pos' : 'text-fg'
                }`}
                title="Σ qty × mark × 100 at the marks on screen: a debit is paid, a credit received."
              >
                {fmtNetCost(net)}
              </span>
            </div>
            {include && stats.isError && !stats.data ? (
              // A refused request is an answer: say what the server said
              // rather than leaving '…' on screen for good.
              <p className="m-0 truncate text-[10px] text-neg" title={errorDetail(stats.error)}>
                Greeks unavailable: {errorDetail(stats.error)}
              </p>
            ) : include ? (
              <dl className="m-0 flex gap-4 min-[1400px]:grid min-[1400px]:grid-cols-4 min-[1400px]:gap-1">
                {GREEKS.map(([key, symbol, dp]) => (
                  <div key={key} className="flex items-baseline gap-1 min-[1400px]:flex-col min-[1400px]:items-start min-[1400px]:gap-0" title={GREEK_UNITS[key]}>
                    <dt className="ws-label">{symbol}</dt>
                    <dd className="num m-0 text-[11px] text-fg">
                      {stats.data ? fmtNumber(stats.data.greeks[key], dp) : '…'}
                    </dd>
                  </div>
                ))}
              </dl>
            ) : (
              <p className="m-0 text-[10px] text-muted">Hidden from the risk graph — greeks not computed.</p>
            )}
            {legs.length >= MAX_MODEL_LEGS ? (
              <p className="m-0 text-[10px] text-warn">
                {MAX_MODEL_LEGS} legs is the limit — clear one to model another contract.
              </p>
            ) : null}
          </footer>
        </>
      )}
    </section>
  )
}
