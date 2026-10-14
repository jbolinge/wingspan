/**
 * The expiration picker: one toggle chip per listed `(expiry, root)`, in date
 * order, scrolling sideways. The pressed chip (at most one) is the chain on
 * screen; none is pressed when the trader has closed the chain.
 *
 * Each chip reads `16 OCT 26` over `46d · AM`; when a date lists both roots
 * (a third Friday: SPX/AM and SPXW/PM) the root rides along as a small suffix
 * so the two chips can be told apart at a glance. Monthlies are set brighter
 * than weeklies and dailies — the listing's own hierarchy. A dot in the
 * corner says the position holds legs there (accent), a second that the
 * model does (warn).
 */

import { useEffect, useRef, useState, type KeyboardEvent } from 'react'
import type { ChainExpiry } from '../../api/types'
import { formatExpiry } from '../../lib/dates'
import { expiryKey } from '../../lib/model'

export interface ExpiryStripProps {
  expirations: readonly ChainExpiry[]
  /** Key (`expiry|root`) of the section on screen; null = no chain open. */
  selected: string | null
  /** Keys where the position holds legs. */
  held: ReadonlySet<string>
  /** Keys where the model has legs. */
  modeled: ReadonlySet<string>
  /** A chip was clicked: the parent opens it, or closes it if it is the open one. */
  onToggle: (key: string) => void
  /**
   * The root a chip names in its title and spoken label, when the chain on
   * screen is not the listing's own product: under XSP the strip lists the
   * SPXW (PM) expirations the XSP chain is built from, and says `XSP`. The
   * keys stay the listing's.
   */
  displayRoot?: string
}

const KIND_TONE: Record<string, string> = {
  monthly: 'text-fg-strong font-semibold',
  quarterly: 'text-fg-strong',
  eom: 'text-fg-strong',
  weekly: 'text-fg',
  daily: 'text-muted',
}

/** The keys that move focus along the strip, and where each one goes. */
const ROVING_KEYS: Record<string, (index: number, last: number) => number> = {
  ArrowRight: (index, last) => Math.min(last, index + 1),
  ArrowLeft: (index) => Math.max(0, index - 1),
  Home: () => 0,
  End: (_, last) => last,
}

export function ExpiryStrip({ expirations, selected, held, modeled, onToggle, displayRoot }: ExpiryStripProps) {
  const scroller = useRef<HTMLDivElement>(null)
  const keys = expirations.map(expiryKey)

  // One tab stop for the whole strip (a roving tabindex), not one per chip: a
  // live SPX listing runs to seventy-odd expirations, and Tab had to cross
  // every one of them to reach the chain's first Model cell. Inside the strip
  // ←/→/Home/End move between chips and Enter/Space toggle, as in any
  // toolbar. The stop is the chip last focused, else the section on
  // screen, else the first chip.
  const [focusKey, setFocusKey] = useState<string | null>(null)
  const tabStop =
    focusKey !== null && keys.includes(focusKey)
      ? focusKey
      : (selected !== null && keys.includes(selected) ? selected : (keys[0] ?? null))
  const onChipKeyDown = (event: KeyboardEvent<HTMLButtonElement>, index: number): void => {
    const move = ROVING_KEYS[event.key]
    if (!move) return
    event.preventDefault()
    const next = keys[move(index, keys.length - 1)]
    if (next === undefined) return
    setFocusKey(next)
    const chip = scroller.current?.querySelector<HTMLElement>(`[data-key="${next}"]`)
    chip?.focus()
    chip?.scrollIntoView?.({ block: 'nearest', inline: 'nearest' })
  }
  const perDate = new Map<string, number>()
  for (const e of expirations) perDate.set(e.expiry, (perDate.get(e.expiry) ?? 0) + 1)

  // Bring the open section's chip into view once, when the listing arrives:
  // in the demo the October monthly is the twenty-somethingth chip, well off
  // the right edge of a 1440 px window.
  const scrolled = useRef(false)
  useEffect(() => {
    if (scrolled.current || selected === null || expirations.length === 0) return
    const chip = scroller.current?.querySelector<HTMLElement>(`[data-key="${selected}"]`)
    if (!chip) return
    scrolled.current = true
    // jsdom has no scrollIntoView; the browser always does.
    chip.scrollIntoView?.({ block: 'nearest', inline: 'center' })
  }, [expirations.length, selected])

  return (
    <div className="flex flex-col gap-0.5">
      <div
        ref={scroller}
        role="toolbar"
        aria-label="Expirations"
        aria-orientation="horizontal"
        className="flex items-stretch gap-1 overflow-x-auto px-2 pt-1.5 pb-1.5"
      >
        {expirations.map((e, index) => {
          const key = expiryKey(e)
          const on = key === selected
          const both = (perDate.get(e.expiry) ?? 0) > 1
          const date = formatExpiry(e.expiry)
          const isHeld = held.has(key)
          const isModeled = modeled.has(key)
          const previous = expirations[index - 1]
          const newMonth = previous !== undefined && previous.expiry.slice(0, 7) !== e.expiry.slice(0, 7)
          const settlement = e.settlement.toUpperCase()
          const root = displayRoot ?? e.root
          const label = [
            `${date} · ${e.dte}d · ${settlement}`,
            root,
            e.kind,
            isHeld ? 'position held' : null,
            isModeled ? 'modeled' : null,
          ]
            .filter(Boolean)
            .join(', ')
          return (
            <div key={key} className="flex shrink-0 items-stretch gap-1">
              {newMonth ? <span aria-hidden="true" className="mx-0.5 w-px self-stretch bg-line" /> : null}
              <button
                type="button"
                data-key={key}
                aria-pressed={on}
                aria-label={label}
                tabIndex={key === tabStop ? 0 : -1}
                onFocus={() => setFocusKey(key)}
                onKeyDown={(event) => onChipKeyDown(event, index)}
                title={`${root} · ${settlement}-settled · ${e.kind}${isHeld ? ' · position held here' : ''}${
                  isModeled ? ' · modeled legs here' : ''
                }`}
                onClick={() => onToggle(key)}
                className={`relative flex min-w-[4.4rem] flex-col items-start rounded border px-2 py-0.5 text-left ${
                  on
                    ? 'border-accent bg-accent-soft'
                    : 'border-line bg-surface-2 hover:border-line-strong hover:bg-surface-3'
                }`}
              >
                <span className={`num text-[11px] leading-tight ${KIND_TONE[e.kind] ?? 'text-fg'}`}>
                  {date}
                  {both ? <span className="ml-1 text-[8px] font-normal text-faint">{root}</span> : null}
                </span>
                <span className="num text-[10px] leading-tight text-muted">
                  {e.dte}d · {settlement}
                </span>
                {isHeld || isModeled ? (
                  <span aria-hidden="true" className="absolute right-1 bottom-1 flex gap-0.5">
                    {isHeld ? <span className="h-1.5 w-1.5 rounded-full bg-accent" /> : null}
                    {isModeled ? <span className="h-1.5 w-1.5 rounded-full bg-warn" /> : null}
                  </span>
                ) : null}
              </button>
            </div>
          )
        })}
      </div>
    </div>
  )
}
