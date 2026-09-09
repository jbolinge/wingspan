/** A horizontal drag bar between two stacked panels.
 *
 *  Reports the pointer's vertical travel from the press as a new size for
 *  the panel above it; the owner clamps and stores that. Keyboard users get
 *  the same through the arrow keys. */

import { useRef, type KeyboardEvent, type PointerEvent } from 'react'

const KEY_STEP = 24

export function SplitHandle({
  label,
  value,
  measure,
  onResize,
  onReset,
}: {
  label: string
  /** The stored size (px), or null while the panel above sits at its
   *  default; only `aria-valuenow` reads it. */
  value: number | null
  /** The rendered height (px) of the panel above, read when a drag or key
   *  press starts — the default size is whatever the content came to. */
  measure: () => number
  onResize: (height: number) => void
  /** Double-click / Enter puts the panel back to its default size. */
  onReset?: () => void
}) {
  const drag = useRef<{ pointerId: number; startY: number; startSize: number } | null>(null)

  const onPointerDown = (event: PointerEvent<HTMLDivElement>): void => {
    if (event.button !== 0) return
    event.preventDefault()
    event.currentTarget.setPointerCapture(event.pointerId)
    drag.current = { pointerId: event.pointerId, startY: event.clientY, startSize: measure() }
  }
  const onPointerMove = (event: PointerEvent<HTMLDivElement>): void => {
    const state = drag.current
    if (state === null || state.pointerId !== event.pointerId) return
    onResize(state.startSize + (event.clientY - state.startY))
  }
  const onPointerEnd = (event: PointerEvent<HTMLDivElement>): void => {
    if (drag.current?.pointerId !== event.pointerId) return
    drag.current = null
    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId)
    }
  }
  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>): void => {
    if (event.key === 'ArrowUp') onResize(measure() - KEY_STEP)
    else if (event.key === 'ArrowDown') onResize(measure() + KEY_STEP)
    else if (event.key === 'Enter' && onReset) onReset()
    else return
    event.preventDefault()
  }

  return (
    <div
      role="separator"
      aria-orientation="horizontal"
      aria-label={label}
      aria-valuenow={value ?? undefined}
      tabIndex={0}
      title={`${label} (drag; double-click to reset)`}
      className="group -my-1 flex h-3 shrink-0 cursor-row-resize items-center justify-center touch-none select-none"
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerEnd}
      onPointerCancel={onPointerEnd}
      onDoubleClick={onReset}
      onKeyDown={onKeyDown}
    >
      <span
        aria-hidden="true"
        className="h-1 w-16 rounded-full bg-line-strong opacity-60 group-hover:bg-accent group-hover:opacity-100 group-focus-visible:bg-accent"
      />
    </div>
  )
}
