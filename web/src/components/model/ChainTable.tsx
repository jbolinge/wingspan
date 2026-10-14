/**
 * One expiration of the option chain, ONE-style: calls left, strike centre,
 * puts right, mirrored so each side's Pos and Model columns sit beside the
 * strike —
 *
 *   V Θ Γ Δ IV Ask Bid Mark Model Pos | STRIKE | Pos Model Mark Bid Ask IV Δ Γ Θ V
 *
 * In-the-money cells are shaded, a dashed spot line runs between the two
 * strikes that bracket spot, and the strike nearest spot carries accent
 * rails. Pos is read-only (accent tint where held); Model is the one editable
 * column (warn tint where modeled).
 */

import { memo, useCallback, type ReactNode } from 'react'
import type { UseQueryResult } from '@tanstack/react-query'
import type { ChainExpiry, ChainQuote, ChainRow, ChainUnderlying, OptionChainResponse, OptionContract } from '../../api/types'
import { fmtNumber, fmtPrice, fmtRatioPct } from '../../lib/format'
import {
  contractKey,
  contractSpokenLabel,
  expiryKey,
  fmtSignedQty,
  fmtStrike,
  heldStrikes,
  sectionTitle,
  stickySectionTitle,
  visibleStrikes,
} from '../../lib/model'
import type { StrikeCount } from '../../store/appStore'
import { chainUnits } from '../units'
import { Card, EmptyState, ErrorNote, Pill, Spinner, Tooltip } from '../ui'
import { QtyInput } from './QtyInput'

type Column = 'vega' | 'theta' | 'gamma' | 'delta' | 'iv' | 'ask' | 'bid' | 'mark' | 'model' | 'pos'

/** Left to right on the call side; the put side is the mirror image. */
const CALL_COLUMNS: readonly Column[] = ['vega', 'theta', 'gamma', 'delta', 'iv', 'ask', 'bid', 'mark', 'model', 'pos']
const PUT_COLUMNS: readonly Column[] = [...CALL_COLUMNS].reverse()
/** Every column of a row: both sides plus the strike. */
export const CHAIN_COLUMN_COUNT = CALL_COLUMNS.length * 2 + 1

const HEADER: Record<Column, string> = {
  vega: 'V',
  theta: 'Θ',
  gamma: 'Γ',
  delta: 'Δ',
  iv: 'IV',
  ask: 'Ask',
  bid: 'Bid',
  mark: 'Mark',
  model: 'Model',
  pos: 'Pos',
}

/** The shading of an in-the-money cell (`--ws-itm`); exported for the tests. */
export const ITM_CLASS = 'bg-itm'

const NO_MARK = 'No mark — this contract cannot be modeled'

export interface ChainTableProps {
  chain: OptionChainResponse
  /** Held quantity per TOS code (the trades the graph draws). */
  positions: ReadonlyMap<string, number>
  /** Modeled quantity per TOS code. */
  modelQty: ReadonlyMap<string, number>
  strikeCount: StrikeCount
  onSetQty: (contract: OptionContract, qty: number) => void
  /** A modeled quantity was dragged from the contract `from` and dropped on
   *  `to`: move it (the store's `moveModelLeg`). */
  onMoveQty: (from: string, to: OptionContract) => void
  onShowAll: () => void
}

/**
 * Walks focus along one Model column of this table — what Enter and Tab do
 * in a Model cell. Disabled cells (no mark) are skipped.
 */
function moveAlongColumn(from: HTMLInputElement, step: 1 | -1): boolean {
  const column = from.dataset.modelCol
  const table = from.closest('table')
  if (!column || !table) return false
  const inputs = Array.from(
    table.querySelectorAll<HTMLInputElement>(`input[data-model-col="${column}"]:not(:disabled)`),
  )
  const next = inputs[inputs.indexOf(from) + step]
  if (!next) return false
  next.focus()
  return true
}

function greekCell(value: number | null | undefined, dp: number): string {
  return fmtNumber(value, dp)
}

function Cell({
  column,
  quote,
  itm,
  held,
  modeled,
  side,
  onSetQty,
  onMoveQty,
}: {
  column: Column
  quote: ChainQuote | null
  itm: boolean
  held: number
  modeled: number
  side: 'call' | 'put'
  onSetQty: ChainTableProps['onSetQty']
  onMoveQty: ChainTableProps['onMoveQty']
}) {
  const shade = itm ? ITM_CLASS : ''
  const cell = 'num px-1.5 py-px text-right whitespace-nowrap'
  const base = `${cell} ${shade}`
  if (column === 'pos') {
    // Read-only by design: no border, no field — just the signed number on an
    // accent tint, the colour the expiry strip's "held" dot uses. A held cell
    // takes the tint *instead of* the ITM shade, never beside it: two
    // background utilities on one element resolve by stylesheet order, and
    // `bg-itm` comes later, so an in-the-money leg — the one a trader comes
    // here to adjust — lost its highlight.
    const tint = held > 0 ? 'bg-accent-soft text-pos' : held < 0 ? 'bg-accent-soft text-neg' : shade
    return (
      <td className={`${cell} w-9 font-semibold ${tint}`} data-col="pos">
        {fmtSignedQty(held)}
      </td>
    )
  }
  if (column === 'model') {
    return (
      <td className={`px-1 py-px ${shade}`} data-col="model">
        {quote ? (
          <QtyInput
            value={modeled}
            column={side}
            label={`Model quantity, ${contractSpokenLabel(quote.contract)}`}
            disabled={quote.mark === null}
            disabledReason={NO_MARK}
            onNavigate={moveAlongColumn}
            onCommit={(qty) => onSetQty(quote.contract, qty)}
            dragKey={quote.contract.tos_code}
            onDropFrom={(from) => onMoveQty(from, quote.contract)}
          />
        ) : null}
      </td>
    )
  }
  if (!quote) return <td className={base} />
  const g = quote.greeks
  const content: Record<Exclude<Column, 'pos' | 'model'>, [string, string]> = {
    vega: [greekCell(g?.vega, 2), 'text-muted'],
    theta: [greekCell(g?.theta, 2), 'text-muted'],
    gamma: [greekCell(g?.gamma, 4), 'text-muted'],
    delta: [greekCell(g?.delta, 3), 'text-muted'],
    iv: [fmtRatioPct(quote.iv, 1), 'text-fg'],
    ask: [fmtPrice(quote.ask), 'text-fg'],
    bid: [fmtPrice(quote.bid), 'text-fg'],
    mark: [fmtPrice(quote.mark), 'font-semibold text-fg-strong'],
  }
  const [text, tone] = content[column]
  return (
    <td
      className={`${base} ${tone}`}
      data-col={column}
      title={column === 'mark' && quote.mark === null ? NO_MARK : undefined}
    >
      {text}
    </td>
  )
}

function HeaderCell({ column, children, title }: { column?: Column; children: ReactNode; title: string }) {
  return (
    <th
      scope="col"
      title={title}
      className={`cursor-help border-b border-line bg-surface-2 px-1.5 py-1 text-[10px] font-medium tracking-wide whitespace-nowrap text-muted uppercase ${
        column === 'model' || column === 'pos' ? 'text-center' : 'text-right'
      }`}
    >
      {children}
    </th>
  )
}

export const ChainTable = memo(function ChainTable({
  chain,
  positions,
  modelQty,
  strikeCount,
  onSetQty,
  onMoveQty,
  onShowAll,
}: ChainTableProps) {
  const spot = chain.spot
  const units = chainUnits(chain.underlying)
  const pinned = heldStrikes(chain.rows, positions, modelQty)
  const rows = visibleStrikes(chain.rows, spot, strikeCount, pinned)
  const indexOf = new Map(chain.rows.map((row, index) => [row.strike, index]))
  const hidden = chain.rows.length - rows.length

  // The strike nearest spot gets the accent rails; the spot line goes after
  // the last strike at or below spot.
  let nearest: number | null = null
  for (const row of rows) {
    if (nearest === null || Math.abs(row.strike - spot) < Math.abs(nearest - spot)) nearest = row.strike
  }
  const spotAfter = rows.reduce((at, row, index) => (row.strike <= spot ? index : at), -1)

  const spotRow = (
    // `data-section` is how the Model tab finds this section's row to centre
    // the scroll container on.
    <tr
      key="spot"
      data-testid="spot-row"
      data-section={expiryKey(chain)}
      aria-label={`Spot ${chain.underlying} ${spot.toFixed(2)}`}
    >
      <td colSpan={CHAIN_COLUMN_COUNT} className="p-0">
        <div className="flex items-center gap-2 py-px">
          <span aria-hidden="true" className="h-0 flex-1 border-t border-dashed border-warn/80" />
          <span className="num rounded border border-warn/50 bg-warn/10 px-1.5 text-[10px] leading-4 font-semibold text-warn">
            {chain.underlying} {spot.toFixed(2)}
          </span>
          <span aria-hidden="true" className="h-0 flex-1 border-t border-dashed border-warn/80" />
        </div>
      </td>
    </tr>
  )

  const body: ReactNode[] = []
  if (spotAfter === -1) body.push(spotRow)
  rows.forEach((row: ChainRow, index) => {
    const previous = rows[index - 1]
    const gap = previous ? (indexOf.get(row.strike) ?? 0) - (indexOf.get(previous.strike) ?? 0) - 1 : 0
    if (gap > 0) {
      body.push(
        <tr key={`gap-${row.strike}`}>
          <td colSpan={CHAIN_COLUMN_COUNT} className="py-px text-center text-[10px] text-faint">
            ⋯ {gap} strike{gap === 1 ? '' : 's'} ⋯
          </td>
        </tr>,
      )
    }
    const callKey = row.call ? contractKey(row.call.contract) : null
    const putKey = row.put ? contractKey(row.put.contract) : null
    const callItm = row.strike < spot
    const putItm = row.strike > spot
    const isNearest = row.strike === nearest
    body.push(
      <tr key={row.strike} data-strike={row.strike} className="group/row border-t border-line/40">
        {CALL_COLUMNS.map((column) => (
          <Cell
            key={`c-${column}`}
            column={column}
            quote={row.call}
            itm={callItm}
            held={callKey ? (positions.get(callKey) ?? 0) : 0}
            modeled={callKey ? (modelQty.get(callKey) ?? 0) : 0}
            side="call"
            onSetQty={onSetQty}
            onMoveQty={onMoveQty}
          />
        ))}
        <th
          scope="row"
          className={`num border-x bg-strike px-3 py-px text-center text-[12px] font-bold text-fg-strong ${
            isNearest ? 'border-x-2 border-x-accent' : 'border-x-line-strong'
          }`}
          data-col="strike"
        >
          {fmtStrike(row.strike)}
        </th>
        {PUT_COLUMNS.map((column) => (
          <Cell
            key={`p-${column}`}
            column={column}
            quote={row.put}
            itm={putItm}
            held={putKey ? (positions.get(putKey) ?? 0) : 0}
            modeled={putKey ? (modelQty.get(putKey) ?? 0) : 0}
            side="put"
            onSetQty={onSetQty}
            onMoveQty={onMoveQty}
          />
        ))}
      </tr>,
    )
    if (index === spotAfter) body.push(spotRow)
  })

  return (
    <>
      <table className="w-full border-collapse text-[11px]">
        <caption className="sr-only">
          {sectionTitle(chain)} option chain: calls on the left, puts on the right
        </caption>
        <thead className="sticky top-0 z-10">
          <tr>
            <th
              scope="colgroup"
              colSpan={CALL_COLUMNS.length}
              className="bg-surface-2 px-2 pt-1 text-right text-[10px] font-semibold tracking-[0.12em] text-fg uppercase"
            >
              {/* Only this header row stays pinned while the chain scrolls
                  under it — the card title goes — so it names the expiration
                  itself: without it, a long chain scrolled to its middle no
                  longer says which expiration a quantity is being typed
                  into. The caption already says this to a screen reader. */}
              <span className="flex items-baseline justify-between gap-3">
                <span
                  aria-hidden="true"
                  className="num truncate tracking-normal text-accent"
                  data-testid="sticky-section"
                >
                  {stickySectionTitle(chain)}
                </span>
                <span>Calls</span>
              </span>
            </th>
            <th
              scope="col"
              rowSpan={2}
              title={units.strike}
              className="cursor-help border-x border-b border-line-strong bg-strike px-3 py-1 text-center align-middle"
            >
              <span className="block text-[10px] font-semibold tracking-wide text-fg uppercase">Strike</span>
              <span className="num block text-[10px] font-normal text-muted">F {chain.forward.toFixed(2)}</span>
            </th>
            <th
              scope="colgroup"
              colSpan={PUT_COLUMNS.length}
              className="bg-surface-2 px-2 pt-1 text-left text-[10px] font-semibold tracking-[0.12em] text-fg uppercase"
            >
              Puts
            </th>
          </tr>
          <tr>
            {CALL_COLUMNS.map((column) => (
              <HeaderCell key={`c-${column}`} column={column} title={units[column]}>
                {HEADER[column]}
              </HeaderCell>
            ))}
            {PUT_COLUMNS.map((column) => (
              <HeaderCell key={`p-${column}`} column={column} title={units[column]}>
                {HEADER[column]}
              </HeaderCell>
            ))}
          </tr>
        </thead>
        <tbody>{body}</tbody>
      </table>
      <footer className="flex items-center gap-2 border-t border-line px-2.5 py-1 text-[10px] text-muted">
        <span className="num">
          {rows.length} of {chain.rows.length} strikes
          {hidden > 0 && pinned.length > 0 ? ' · held and modeled strikes always shown' : ''}
        </span>
        {hidden > 0 ? (
          <button type="button" className="bg-transparent text-accent hover:underline" onClick={onShowAll}>
            Show all
          </button>
        ) : null}
        {chain.warnings.length > 0 ? (
          <Tooltip
            content={
              <ul className="m-0 list-disc space-y-1 pl-4">
                {chain.warnings.map((w) => (
                  <li key={w.code + w.message}>
                    <span className="num">{w.code}</span> — {w.message}
                  </li>
                ))}
              </ul>
            }
          >
            <Pill tone="warn" title={`${chain.warnings.length} pricing warnings on this chain`}>
              ⚠ {chain.warnings.length}
            </Pill>
          </Tooltip>
        ) : null}
        {chain.stale ? (
          <Pill tone="warn" title={chain.degraded.join('; ') || 'served from cache after a failed refetch'}>
            stale
          </Pill>
        ) : null}
      </footer>
    </>
  )
})

export interface ChainSectionProps {
  listing: ChainExpiry
  /** The index the chain is on; XSP renames the card (`XSP · … · PM`). */
  underlying?: ChainUnderlying
  query: UseQueryResult<OptionChainResponse>
  positions: ReadonlyMap<string, number>
  modelQty: ReadonlyMap<string, number>
  strikeCount: StrikeCount
  onSetQty: ChainTableProps['onSetQty']
  onMoveQty: ChainTableProps['onMoveQty']
  onShowAll: () => void
  onHide: (key: string) => void
  /** Scroll the tab so this section's spot row sits mid-view. */
  onCentre: (key: string) => void
  sectionKey: string
}

/** The section card: the title, the forward, a hide action, and whatever state
 *  this chain is in — an error here stays inside this card. */
export function ChainSection({
  listing,
  underlying = 'SPX',
  query,
  positions,
  modelQty,
  strikeCount,
  onSetQty,
  onMoveQty,
  onShowAll,
  onHide,
  onCentre,
  sectionKey,
}: ChainSectionProps) {
  const title = sectionTitle(underlying === 'XSP' ? { ...listing, root: 'XSP', settlement: 'pm' } : listing)
  const hide = useCallback(() => onHide(sectionKey), [onHide, sectionKey])
  const centre = useCallback(() => onCentre(sectionKey), [onCentre, sectionKey])
  const chain = query.data
  return (
    <Card
      title={title}
      className="shrink-0"
      actions={
        <>
          {chain ? (
            <span className="num text-[10px] text-muted" title="The forward this expiration's IVs were solved against">
              fwd {chain.forward.toFixed(2)}
            </span>
          ) : null}
          {query.isFetching && chain ? <Spinner label="" /> : null}
          {/* The tab centres a section by itself only when it opens; this
              is the way back to the money after scrolling away. */}
          <button
            type="button"
            className="ws-btn px-1.5 py-0 text-[10px] leading-4"
            aria-label={`Centre ${title} on spot`}
            title="Scroll this expiration's spot line to the middle of the view"
            disabled={!chain || chain.rows.length === 0}
            onClick={centre}
          >
            ⌖ spot
          </button>
          <button
            type="button"
            className="ws-btn px-1.5 py-0 text-[10px] leading-4"
            aria-label={`Hide ${title}`}
            title="Hide this expiration"
            onClick={hide}
          >
            ✕ hide
          </button>
        </>
      }
    >
      {/* A failed refetch keeps the last chain on screen under the note; a
          first load that failed has only the note. */}
      {query.isError ? <ErrorNote error={query.error} onRetry={() => void query.refetch()} /> : null}
      {chain ? (
        chain.rows.length === 0 ? (
          <EmptyState>No strikes are listed for this expiration.</EmptyState>
        ) : (
          <ChainTable
            chain={chain}
            positions={positions}
            modelQty={modelQty}
            strikeCount={strikeCount}
            onSetQty={onSetQty}
            onMoveQty={onMoveQty}
            onShowAll={onShowAll}
          />
        )
      ) : query.isError ? null : (
        <div className="px-3 py-4">
          <Spinner label="Loading chain…" />
        </div>
      )}
    </Card>
  )
}
