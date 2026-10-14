/**
 * The model ticket: every modeled leg as an order line
 * (`−1 × SPX 16 OCT 26 7500 P @ 42.50`), each removable, and what the model
 * adds up to — net premium at the current marks and a greeks table of what
 * the risk graph draws: the model alone, the selected trades alone, and the
 * trades with the model on top (each one `portfolio` call, the same engine
 * and the same cache keys as the graph's header) — and, under those, the
 * legs as thinkorswim orders to copy (`ModelOrders`).
 *
 * A right rail beside the chain (or the Model tab's risk graph) from 1400 px
 * up; above it, compact, on a narrower window.
 */

import { useMemo } from 'react'
import type { UseQueryResult } from '@tanstack/react-query'
import { errorDetail } from '../../api/client'
import { useModelSyncStatus, usePortfolio, useValuationDate } from '../../api/queries'
import type { PortfolioStats } from '../../api/types'
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
import { RAIL_QUERY, useMediaQuery } from '../../lib/useMediaQuery'
import { MAX_MODEL_LEGS, useAppStore, type ModelLeg } from '../../store/appStore'
import { GREEK_UNITS } from '../units'
import { ModelOrders } from './ModelOrders'

/** A model is usually a lot or two, so its greeks get two decimals (four for
 *  gamma) where the book's whole-dollar theta and vega would print `-0`. */
const GREEKS = [
  ['Δ', 2, GREEK_UNITS.delta, (s: PortfolioStats) => s.greeks.delta],
  ['Γ', 4, GREEK_UNITS.gamma, (s: PortfolioStats) => s.greeks.gamma],
  ['Θ', 2, GREEK_UNITS.theta, (s: PortfolioStats) => s.greeks.theta],
  ['V', 2, GREEK_UNITS.vega, (s: PortfolioStats) => s.greeks.vega],
  ['W.V', 2, GREEK_UNITS.weightedVega, (s: PortfolioStats) => s.weighted_vega],
] as const

/** Every line's greeks are the server's, in SPX points — an XSP leg (the
 *  modeled ones and a held hedge alike) at one tenth of its own-terms greek. */
const EXPOSURE = 'SPX-point exposure; an XSP leg counts one tenth.'

/** One line of the greeks table: which numbers it holds, and how it is
 *  labelled — `short` where the 18rem rail has a column for it, not a line. */
interface GreekLine {
  label: string
  short: string
  title: string
  query: UseQueryResult<PortfolioStats>
}

/** Every row of the table is a subgrid of it, so a column — the labels'
 *  `auto` one above all — is as wide as its widest cell in any row and the
 *  values line up under their headers. */
const ROW = 'col-span-full grid grid-cols-subgrid items-baseline'
/** A value: right-aligned so the decimals line up, and clipped with the full
 *  number on hover rather than spilling into its neighbour. */
const CELL = 'num truncate text-right text-[11px] text-fg'

function value(query: UseQueryResult<PortfolioStats>, pick: (s: PortfolioStats) => number, dp: number): string {
  return query.data ? fmtNumber(pick(query.data), dp) : query.isError ? '—' : '…'
}

/**
 * The greeks table, one of two ways round. On a narrow window, where the
 * ticket sits above the chain and is short of height, one row per line
 * (Model, Position, Position + model) and a column per greek. In the 18rem
 * rail, which is short of width, five greek columns of a real book's vega
 * (`-1,841.25`) do not fit at 50 px each, so it turns: a row per greek and a
 * column per line, at most three of them.
 *
 * Both have a real corner header, so assistive technology lines every value
 * up under its own column rather than one to the left.
 */
function GreeksTable({ lines, rail }: { lines: readonly GreekLine[]; rail: boolean }) {
  const corner = (text: string) => (
    <span role="columnheader">
      <span className="sr-only">{text}</span>
    </span>
  )
  if (rail) {
    return (
      <div
        role="table"
        aria-label="Greeks"
        className="grid gap-x-1 gap-y-0.5"
        style={{ gridTemplateColumns: `2rem repeat(${lines.length}, minmax(0, 1fr))` }}
      >
        <div role="row" className={ROW}>
          {corner('Greek')}
          {lines.map((line) => (
            <span key={line.label} role="columnheader" className="ws-label truncate text-right" title={line.title}>
              {line.short}
            </span>
          ))}
        </div>
        {GREEKS.map(([symbol, dp, unit, pick]) => (
          <div key={symbol} role="row" className={ROW}>
            <span role="rowheader" className="ws-label" title={unit}>
              {symbol}
            </span>
            {lines.map((line) => {
              const text = value(line.query, pick, dp)
              return (
                <span key={line.label} role="cell" className={CELL} title={text}>
                  {text}
                </span>
              )
            })}
          </div>
        ))}
      </div>
    )
  }
  return (
    <div
      role="table"
      aria-label="Greeks"
      className="grid min-w-[17rem] grid-cols-[auto_repeat(5,minmax(3.25rem,1fr))] gap-x-2 gap-y-0.5"
    >
      <div role="row" className={ROW}>
        {corner('Line')}
        {GREEKS.map(([symbol, , unit]) => (
          <span key={symbol} role="columnheader" className="ws-label text-right" title={unit}>
            {symbol}
          </span>
        ))}
      </div>
      {lines.map((line) => (
        <div key={line.label} role="row" className={ROW}>
          <span role="rowheader" className="ws-label truncate" title={line.title}>
            {line.label}
          </span>
          {GREEKS.map(([symbol, dp, , pick]) => {
            const text = value(line.query, pick, dp)
            return (
              <span key={symbol} role="cell" className={CELL} title={text}>
                {text}
              </span>
            )
          })}
        </div>
      ))}
    </div>
  )
}

export function ModelTicket({
  legs,
  marks,
  include,
  selectedIds,
}: {
  legs: readonly ModelLeg[]
  /** Marks by TOS code from the loaded chains; absent = not loaded yet. */
  marks: ReadonlyMap<string, number | null>
  include: boolean
  /** The trades ticked on the Positions tab (stale ids already pruned). Empty
   *  means the graph draws the model alone — NOT "the whole book", which is
   *  what the chain's Pos column counts then; the two are kept apart here. */
  selectedIds: readonly string[]
}) {
  const removeModelLeg = useAppStore((s) => s.removeModelLeg)
  const sync = useModelSyncStatus()
  const rail = useMediaQuery(RAIL_QUERY)
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
  const drawn = include ? request : []
  const model = usePortfolio([], drawn)
  // The selected trades by themselves — only with a selection: with none the
  // graph draws the model alone and "Position" would have nothing to mean.
  const position = usePortfolio(selectedIds, [])
  // Exactly what the risk graph draws (the panel's header asks for the same
  // thing). Equal to one of the rows above unless there are both trades and
  // drawn legs, so it is asked, and shown, only then.
  const showTotal = selectedIds.length > 0 && drawn.length > 0
  const total = usePortfolio(showTotal ? selectedIds : [], showTotal ? drawn : [])
  const net = modelNetCost(legs, marks)

  const lines: GreekLine[] = [
    ...(include && legs.length > 0
      ? [{ label: 'Model',
          short: 'Model',
          title: `The modeled legs alone. ${EXPOSURE}`,
          query: model }]
      : []),
    ...(selectedIds.length > 0
      ? [
          {
            label: 'Position',
            short: 'Position',
            title: `The trades selected on the Positions tab, without the model. ${EXPOSURE}`,
            query: position,
          },
        ]
      : []),
    ...(showTotal
      ? [
          {
            label: 'Position + model',
            short: 'Total',
            title: `Position + model — what the risk graph draws. ${EXPOSURE}`,
            query: total,
          },
        ]
      : []),
  ]
  const failed = lines.filter((line) => line.query.isError && !line.query.data)

  return (
    // In the rail the ticket is as tall as its content but may shrink to its
    // column, so a long legs list scrolls instead of pushing the Orders block
    // off the bottom of the window.
    <section aria-label="Model ticket" className="ws-card flex min-h-0 flex-1 flex-col min-[1400px]:flex-initial">
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
          <span className="num">+1</span> buys one, <span className="num">−1</span> sells one — and the{' '}
          <span className="text-fg">Risk graph</span> view above draws what it does.
        </p>
      ) : (
        // Named, because the Orders block below is a list inside this region
        // too. It scrolls when the footer needs the height, but never below
        // one row of legs.
        <ul
          aria-label="Modeled legs"
          className="m-0 flex min-h-8 list-none flex-wrap gap-x-4 overflow-y-auto p-0 px-2.5 py-1.5 min-[1400px]:flex-col min-[1400px]:flex-nowrap"
        >
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
      )}
      {/* There is a footer whenever there is anything to price: modeled legs,
          or trades selected on Positions (whose greeks this panel owes the
          trader even with no model). One line of net premium beside the
          table on a narrow window — where, if the Orders block has to wrap
          under the greeks, the footer scrolls rather than crushing the legs
          list above it — and a stacked block in the rail. */}
      {legs.length > 0 || selectedIds.length > 0 ? (
        <footer className="flex min-h-0 flex-wrap items-start gap-x-6 gap-y-1 overflow-y-auto border-t border-line px-2.5 py-1.5 min-[1400px]:block min-[1400px]:shrink-0 min-[1400px]:space-y-1 min-[1400px]:overflow-visible">
          {legs.length > 0 ? (
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
          ) : null}
          {lines.length > 0 ? <GreeksTable lines={lines} rail={rail} /> : null}
          {legs.length > 0 && !include ? (
            <p className="m-0 text-[10px] text-muted">Hidden from the risk graph — greeks not computed.</p>
          ) : null}
          {failed.map(({ label, query }) => (
            // A refused request is an answer: say what the server said
            // rather than leaving '…' on screen for good.
            <p
              key={label}
              className="m-0 basis-full truncate text-[10px] text-neg"
              title={errorDetail(query.error)}
            >
              {label} greeks unavailable: {errorDetail(query.error)}
            </p>
          ))}
          {/* Every live leg, drawn or not: the orders are the ticket, not
              the graph. Beside the greeks above the chain when there is
              room; under them in the rail. */}
          {request.length > 0 ? <ModelOrders legs={request} /> : null}
          {legs.length >= MAX_MODEL_LEGS ? (
            <p className="m-0 text-[10px] text-warn">
              {MAX_MODEL_LEGS} legs is the limit — clear one to model another contract.
            </p>
          ) : null}
        </footer>
      ) : null}
    </section>
  )
}
