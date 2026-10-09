/**
 * The Model ticket's Orders block: the modeled legs as thinkorswim order
 * lines, ready to paste into TOS's order entry — the way Option.Explorer's
 * "copy as TOS order" works.
 *
 * The grouping (an iron condor stays one `IRON CONDOR` order, a 1/3/2 fly
 * becomes a `BUTTERFLY` plus a `VERTICAL`, …) and every line's text and limit
 * come from the server (`model_orders`), where each line is round-trip
 * tested through the same TOS parser the trade files go through. Nothing
 * here re-spells the grammar: this block lists, labels and copies.
 */

import { useEffect, useId, useRef, useState } from 'react'
import { errorDetail } from '../../api/client'
import { useModelOrders } from '../../api/queries'
import type { ModelLeg, TosOrderDto, TosStrategy } from '../../api/types'
import { copyText } from '../../lib/clipboard'
import { Pill } from '../ui'

/** The strategy word TOS writes, for the pill — `SINGLE` where the line has
 *  none (a statement's word for it). */
const STRATEGY_LABEL: Record<TosStrategy, string> = {
  single: 'Single',
  vertical: 'Vertical',
  calendar: 'Calendar',
  diagonal: 'Diagonal',
  butterfly: 'Butterfly',
  ratio_butterfly: '~Butterfly',
  back_ratio: 'Backratio',
  straddle: 'Straddle',
  strangle: 'Strangle',
  condor: 'Condor',
  iron_condor: 'Iron condor',
}

/** How long a Copy button reads "Copied" (and the status says so). */
export const COPIED_MS = 1500

/** Which button the last successful copy came from, and how many orders it
 *  put on the clipboard. */
interface Copied {
  target: 'all' | number
  count: number
}

/** A row's identity across repricings: its place, its strategy and side,
 *  and its legs — everything in the line except the limit. */
function orderKey(i: number, order: TosOrderDto): string {
  const legs = order.legs.map((leg) => `${leg.qty}${leg.contract.tos_code}`).join(',')
  return `${i}|${order.strategy}|${order.side}|${order.qty}|${legs}`
}

function plural(n: number, word: string): string {
  return `${n} ${word}${n === 1 ? '' : 's'}`
}

/**
 * `legs` is the ticket's live, sorted `toModelLegs` output — every modeled
 * leg, whether or not the model is drawn on the graph: the orders are the
 * ticket, not the graph.
 */
export function ModelOrders({ legs }: { legs: readonly ModelLeg[] }) {
  const query = useModelOrders(legs)
  const labelId = useId()
  const [copied, setCopied] = useState<Copied | null>(null)
  const [copyError, setCopyError] = useState<string | null>(null)
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null)
  useEffect(
    () => () => {
      if (timer.current !== null) clearTimeout(timer.current)
    },
    [],
  )

  const data = query.data
  const orders = data?.orders ?? []
  // A refetch behind lines already on screen keeps them copyable; a failed
  // one does not — the lines may no longer be what the model is.
  const stale = query.isError
  const canCopyAll = data !== undefined && orders.length > 0 && !stale

  const copy = async (target: Copied['target'], text: string, count: number): Promise<void> => {
    setCopyError(null)
    if (timer.current !== null) clearTimeout(timer.current)
    try {
      await copyText(text)
      setCopied({ target, count })
      timer.current = setTimeout(() => setCopied(null), COPIED_MS)
    } catch (error) {
      setCopied(null)
      setCopyError(errorDetail(error))
    }
  }

  // Every distinct contract priced off a neighbour's vol rather than a quote
  // of its own: its share of a limit is a model number, not a market.
  const unquoted = [...new Set(orders.flatMap((order) => order.unquoted))]

  return (
    <div
      role="group"
      aria-labelledby={labelId}
      className="min-w-[16rem] flex-1 basis-0 space-y-0.5 min-[1400px]:mt-1 min-[1400px]:min-w-0 min-[1400px]:border-t min-[1400px]:border-line min-[1400px]:pt-1.5"
    >
      <div className="flex items-center gap-2">
        <span id={labelId} className="ws-label">
          Orders
        </span>
        {data !== undefined ? (
          <span className="num text-[10px] text-muted">{plural(orders.length, 'order')}</span>
        ) : null}
        <button
          type="button"
          className="ws-btn ml-auto px-1.5 py-0 text-[10px] leading-4"
          disabled={!canCopyAll}
          // A name that holds still while the text flips to "Copied" — the
          // status line is what announces the copy.
          aria-label="Copy all orders"
          title="Copy every order line, one per line, to paste into thinkorswim"
          onClick={() => void copy('all', data?.text ?? '', orders.length)}
        >
          {copied?.target === 'all' ? 'Copied' : 'Copy all'}
        </button>
      </div>

      {data === undefined ? (
        query.isError ? (
          <p className="m-0 truncate text-[10px] text-neg" title={errorDetail(query.error)}>
            Orders unavailable: {errorDetail(query.error)}
          </p>
        ) : (
          <p className="num m-0 text-[11px] text-muted">…</p>
        )
      ) : orders.length === 0 ? (
        <p className="m-0 text-[10px] text-muted">No orders — the modeled legs net to nothing.</p>
      ) : (
        // Three lines' worth above the chain, where the ticket is short of
        // height; the rail can afford a longer list.
        <ol className="m-0 max-h-[4.5rem] list-none space-y-0.5 overflow-y-auto p-0 min-[1400px]:max-h-60">
          {orders.map((order, i) => (
            // Keyed on what the order *is*, not on its line: the line ends in
            // the limit, which moves with the snapshot, and a remounted row
            // would drop keyboard focus off its Copy button.
            <li key={orderKey(i, order)} className="flex min-w-0 items-center gap-1.5 min-[1400px]:flex-wrap">
              <Pill tone={order.side === 'buy' ? 'pos' : 'neg'} className="shrink-0 leading-4">
                {STRATEGY_LABEL[order.strategy]}
              </Pill>
              {/* Above the chain the line gives way first (whole on hover,
                  and one click selects all of it). In the rail it takes a
                  row of its own under the pill and the button, and wraps
                  there: truncated at 18rem, every line ended `… [AM] …`
                  before its strikes and limit, and five strangles looked
                  alike. */}
              <span
                className="num min-w-0 flex-1 truncate text-[11px] text-fg-strong select-all min-[1400px]:order-last min-[1400px]:basis-full min-[1400px]:break-words min-[1400px]:whitespace-normal"
                title={order.line}
              >
                {order.line}
              </span>
              <button
                type="button"
                className="ws-btn ml-auto shrink-0 px-1.5 py-0 text-[10px] leading-4"
                aria-label={`Copy order ${i + 1}: ${order.line}`}
                disabled={stale}
                title="Copy this order line to paste into thinkorswim"
                onClick={() => void copy(i, order.line, 1)}
              >
                {copied?.target === i ? 'Copied' : 'Copy'}
              </button>
            </li>
          ))}
        </ol>
      )}

      {data !== undefined && stale ? (
        <p className="m-0 truncate text-[10px] text-neg" title={errorDetail(query.error)}>
          Orders not refreshed: {errorDetail(query.error)}
        </p>
      ) : null}
      {copied !== null ? (
        <p role="status" className="m-0 text-[10px] text-pos">
          Copied {plural(copied.count, 'order')}
        </p>
      ) : null}
      {copyError !== null ? (
        <p className="m-0 truncate text-[10px] text-neg" title={copyError}>
          Couldn&apos;t copy: {copyError}
        </p>
      ) : null}
      {orders.length > 0 ? (
        <p className="m-0 text-[10px] text-muted">Prices are the net mark per spread — set your own limit in TOS.</p>
      ) : null}
      {unquoted.length > 0 ? (
        <p className="m-0 truncate text-[10px] text-warn" title={unquoted.join(', ')}>
          <span aria-hidden="true">⚠ </span>
          {unquoted.length === 1
            ? `${unquoted[0]} has no quote; its price is modeled.`
            : `${unquoted.join(', ')} have no quote; their prices are modeled.`}
        </p>
      ) : null}
    </div>
  )
}
