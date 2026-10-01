/**
 * The chain's Model cell: a compact signed-quantity field.
 *
 * Blank is zero. What the trader types is a draft until Enter, Tab or blur
 * commits it (Esc throws it away), so a half-typed `-1` never reaches the
 * store — or the risk graph — as `-`. ↑/↓ step the draft by one.
 *
 * It is a text input with `role="spinbutton"` rather than `type="number"`:
 * a number input cannot display the `+` of `+2`, turns a stray mouse-wheel
 * over the table into a quantity change, and spends half of a 52 px cell on
 * spinner arrows nobody asked for. The ARIA value attributes carry what a
 * number input would have announced.
 *
 * A modeled quantity can also be dragged to another strike. The gesture is
 * the browser's own text drag (focus selects the `-1`; drag it), which the
 * cell tags with [`MODEL_DRAG_TYPE`] so the cell it lands on can tell the
 * table to *move* the leg — the source empties, the way a trader expects when
 * sliding a short strike up or down the chain. Dropped anywhere else, nothing
 * changes: the browser's default text drag would have emptied the source and
 * the draft it leaves behind is discarded on `dragend`.
 */

import { useRef, useState, type DragEvent, type KeyboardEvent } from 'react'
import {
  carriesModelDrag,
  fmtSignedQty,
  parseModelQty,
  readModelDrag,
  writeModelDrag,
} from '../../lib/model'
import { MAX_MODEL_QTY, normalizeModelQty } from '../../store/appStore'

export interface QtyInputProps {
  /** The committed quantity; 0 renders blank. */
  value: number
  onCommit: (qty: number) => void
  /** The accessible name, e.g. `Model quantity, SPXW 16 Oct 26 7700 call`. */
  label: string
  disabled?: boolean
  /** Shown as the tooltip while disabled — why this cell cannot be edited. */
  disabledReason?: string
  /**
   * Moves focus along the column (`step` 1 = down): Enter commits and goes
   * down, Tab/Shift+Tab walk the column instead of the row. Returns whether
   * focus moved; when it did not (the end of the column), Tab falls through
   * to the browser's own order.
   */
  onNavigate?: (from: HTMLInputElement, step: 1 | -1) => boolean
  /** Extra attributes the table navigates by. */
  column?: string
  /** The contract's `tos_code`; with `onDropFrom`, the cell can be dragged
   *  from and dropped on. */
  dragKey?: string
  /** A modeled quantity was dropped here from the cell `from`. */
  onDropFrom?: (from: string, qty: number) => void
}

export function QtyInput({
  value,
  onCommit,
  label,
  disabled = false,
  disabledReason,
  onNavigate,
  column,
  dragKey,
  onDropFrom,
}: QtyInputProps) {
  const [draft, setDraftState] = useState<string | null>(null)
  const [dropping, setDropping] = useState(false)
  // Between dragstart and dragend the browser may rewrite the input itself
  // (a text drag's default is to cut the selection from its source); that is
  // not the trader typing, so it must not become a draft that a later blur
  // would commit as zero.
  const dragging = useRef(false)
  // Read through a ref as well: Enter (or Tab) commits and then moves focus,
  // and the blur that follows fires before React has re-rendered — a commit
  // reading the state would see the draft it just committed and send it twice.
  const draftRef = useRef<string | null>(null)
  const setDraft = (next: string | null): void => {
    draftRef.current = next
    setDraftState(next)
  }
  const shown = draft ?? fmtSignedQty(value)
  const active = value !== 0
  const draggable = dragKey !== undefined && onDropFrom !== undefined && !disabled

  const commit = (): void => {
    const pending = draftRef.current
    if (pending === null) return
    const parsed = parseModelQty(pending)
    setDraft(null)
    // Unreadable text reverts rather than guessing what was meant.
    if (parsed === null) return
    const next = normalizeModelQty(parsed)
    if (next !== value) onCommit(next)
  }

  const step = (delta: 1 | -1): void => {
    const pending = draftRef.current
    const base = pending === null ? value : (parseModelQty(pending) ?? value)
    const next = Math.max(-MAX_MODEL_QTY, Math.min(MAX_MODEL_QTY, normalizeModelQty(base) + delta))
    setDraft(fmtSignedQty(next))
  }

  const onKeyDown = (event: KeyboardEvent<HTMLInputElement>): void => {
    const input = event.currentTarget
    switch (event.key) {
      case 'Enter':
        event.preventDefault()
        commit()
        onNavigate?.(input, 1)
        break
      case 'Escape':
        // Only swallow Esc when there is something to revert; otherwise it
        // still belongs to whatever is listening above.
        if (draftRef.current !== null) {
          event.preventDefault()
          event.stopPropagation()
          setDraft(null)
        }
        break
      case 'ArrowUp':
        event.preventDefault()
        step(1)
        break
      case 'ArrowDown':
        event.preventDefault()
        step(-1)
        break
      case 'Tab':
        commit()
        if (onNavigate?.(input, event.shiftKey ? -1 : 1)) event.preventDefault()
        break
      default:
        break
    }
  }

  const onDragStart = (event: DragEvent<HTMLInputElement>): void => {
    if (!draggable || !active || dragKey === undefined) return
    dragging.current = true
    writeModelDrag(event.dataTransfer, dragKey, value)
  }
  const onDragEnd = (): void => {
    dragging.current = false
    // Whatever the browser did to the text, the cell shows the store again:
    // a successful drop has already moved the leg, a failed one left it.
    setDraft(null)
  }
  const onDragOver = (event: DragEvent<HTMLElement>): void => {
    if (!draggable || !carriesModelDrag(event.dataTransfer)) return
    event.preventDefault()
    event.dataTransfer.dropEffect = 'move'
    if (!dropping) setDropping(true)
  }
  const onDragLeave = (): void => setDropping(false)
  const onDrop = (event: DragEvent<HTMLElement>): void => {
    setDropping(false)
    const drag = readModelDrag(event.dataTransfer)
    if (!draggable || drag === null) return
    // Ours: stop the browser pasting the text into the field as well.
    event.preventDefault()
    event.stopPropagation()
    setDraft(null)
    if (drag.from !== dragKey) onDropFrom?.(drag.from, drag.qty)
  }

  return (
    <span
      className={`group/qty relative mx-auto block w-fit rounded ${active ? 'bg-warn/15' : ''} ${
        dropping ? 'ring-2 ring-accent' : ''
      }`}
      title={disabled ? disabledReason : undefined}
      data-drop-target={dropping || undefined}
      onDragOver={onDragOver}
      onDragLeave={onDragLeave}
      onDrop={onDrop}
    >
      <input
        type="text"
        inputMode="numeric"
        role="spinbutton"
        autoComplete="off"
        spellCheck={false}
        aria-label={label}
        aria-valuenow={value}
        aria-valuemin={-MAX_MODEL_QTY}
        aria-valuemax={MAX_MODEL_QTY}
        aria-valuetext={active ? fmtSignedQty(value) : 'none'}
        data-model-col={column}
        disabled={disabled}
        value={shown}
        onFocus={(event) => event.currentTarget.select()}
        onChange={(event) => {
          if (!dragging.current) setDraft(event.target.value)
        }}
        onBlur={commit}
        onKeyDown={onKeyDown}
        onDragStart={onDragStart}
        onDragEnd={onDragEnd}
        // No `outline-none`: the cell keeps the app's own 2 px focus ring,
        // which is what makes walking the column with Tab/Enter followable —
        // a 1 px border change alone is not. Nor a focus background: on a
        // modeled cell it painted over the warn tint, so the cell being
        // edited looked like an empty one.
        className={`num block h-[1.15rem] w-[3.25rem] rounded border px-1 text-right text-[11px] leading-none focus:border-accent disabled:cursor-not-allowed disabled:opacity-40 ${
          active
            ? 'border-warn/60 bg-transparent font-semibold text-fg-strong'
            : 'border-line bg-bg text-fg hover:border-line-strong'
        }`}
      />
      {active && !disabled ? (
        // A mouse affordance only: from the keyboard, deleting the text and
        // pressing Enter does the same, and a tab stop per cell would break
        // walking the column with Tab.
        <button
          type="button"
          tabIndex={-1}
          aria-label={`Clear ${label}`}
          title="Clear this modeled quantity"
          className="absolute top-1/2 left-0.5 hidden -translate-y-1/2 bg-transparent px-0.5 text-[9px] leading-none text-muted group-hover/qty:block hover:text-neg"
          // Keeping focus where it is means the input never blurs, so a
          // pending draft cannot commit itself over the clear.
          onMouseDown={(event) => event.preventDefault()}
          onClick={() => {
            setDraft(null)
            onCommit(0)
          }}
        >
          ✕
        </button>
      ) : null}
    </span>
  )
}
