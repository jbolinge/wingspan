/** Small presentational primitives shared by every panel. */

import type { CSSProperties, ReactNode } from 'react'
import { useId, useState } from 'react'
import { fmtAge, stalenessLevel, type Staleness } from '../lib/format'

export type Tone = 'pos' | 'neg' | 'warn' | 'info' | 'muted' | 'accent'

const TONE_CLASS: Record<Tone, string> = {
  pos: 'text-pos border-pos/45 bg-pos/10',
  neg: 'text-neg border-neg/45 bg-neg/10',
  warn: 'text-warn border-warn/45 bg-warn/10',
  info: 'text-info border-info/45 bg-info/10',
  accent: 'text-accent border-accent/45 bg-accent-soft',
  muted: 'text-muted border-line bg-surface-2',
}

export function Pill({
  tone = 'muted',
  children,
  title,
  className = '',
}: {
  tone?: Tone
  children: ReactNode
  title?: string
  className?: string
}) {
  return (
    <span
      title={title}
      className={`inline-flex items-center gap-1 rounded border px-1.5 py-px text-[10px] font-semibold tracking-wide uppercase ${TONE_CLASS[tone]} ${className}`}
    >
      {children}
    </span>
  )
}

/**
 * Hover/focus tooltip. Also mirrors its content into `title` so the
 * information survives touch, screen readers and copy-paste.
 */
export function Tooltip({
  content,
  children,
  align = 'left',
  className = '',
}: {
  content: ReactNode
  children: ReactNode
  align?: 'left' | 'right' | 'center'
  className?: string
}) {
  const [open, setOpen] = useState(false)
  const id = useId()
  const plain = typeof content === 'string' ? content : undefined
  const position =
    align === 'right' ? 'right-0' : align === 'center' ? 'left-1/2 -translate-x-1/2' : 'left-0'
  return (
    <span
      className={`relative inline-flex ${className}`}
      onMouseEnter={() => setOpen(true)}
      onMouseLeave={() => setOpen(false)}
      onFocus={() => setOpen(true)}
      onBlur={() => setOpen(false)}
    >
      <span title={plain} aria-describedby={open ? id : undefined} tabIndex={0} className="outline-none">
        {children}
      </span>
      {open ? (
        <span
          id={id}
          role="tooltip"
          className={`pointer-events-none absolute top-full z-50 mt-1 w-max max-w-[26rem] rounded border border-line bg-surface-3 px-2 py-1.5 text-[11px] leading-snug font-normal normal-case text-fg shadow-lg ${position}`}
          style={{ boxShadow: 'var(--ws-shadow)' }}
        >
          {content}
        </span>
      ) : null}
    </span>
  )
}

/** A superscript info affordance for column-header units. */
export function InfoTip({ content }: { content: ReactNode }) {
  return (
    <Tooltip content={content} align="center">
      <span className="cursor-help text-faint select-none" aria-hidden="true">
        ⓘ
      </span>
    </Tooltip>
  )
}

const STALENESS_COLOR: Record<Staleness, string> = {
  fresh: 'var(--ws-pos)',
  aging: 'var(--ws-warn)',
  stale: 'var(--ws-neg)',
  dead: 'var(--ws-faint)',
}

export function StalenessDot({ seconds }: { seconds: number | null | undefined }) {
  const level = stalenessLevel(seconds)
  const label =
    seconds === null || seconds === undefined
      ? 'no quote backing this row'
      : `worst quote behind this row is ${fmtAge(seconds)} old (${level})`
  return (
    <Tooltip content={label} align="center">
      <span
        aria-label={label}
        role="img"
        data-staleness={level}
        className="inline-block h-2 w-2 rounded-full align-middle"
        style={{ background: STALENESS_COLOR[level] }}
      />
    </Tooltip>
  )
}

export function Card({
  title,
  actions,
  children,
  className = '',
  bodyClassName = '',
}: {
  title?: ReactNode
  actions?: ReactNode
  children: ReactNode
  className?: string
  bodyClassName?: string
}) {
  return (
    <section className={`ws-card flex min-h-0 flex-col ${className}`}>
      {title !== undefined ? (
        <header className="flex shrink-0 items-center justify-between gap-2 border-b border-line px-2.5 py-1.5">
          <h2 className="ws-label m-0 font-semibold">{title}</h2>
          {actions ? <div className="flex items-center gap-1">{actions}</div> : null}
        </header>
      ) : null}
      <div className={`min-h-0 flex-1 ${bodyClassName}`}>{children}</div>
    </section>
  )
}

export function Spinner({ label = 'Loading' }: { label?: string }) {
  return (
    <span className="inline-flex items-center gap-2 text-muted" role="status">
      <span
        aria-hidden="true"
        className="inline-block h-3 w-3 animate-spin rounded-full border-2 border-line border-t-accent"
      />
      {label}
    </span>
  )
}

export function ErrorNote({ error, onRetry }: { error: unknown; onRetry?: () => void }) {
  const message = error instanceof Error ? error.message : String(error)
  return (
    <div role="alert" className="m-2 rounded border border-neg/50 bg-neg/10 px-3 py-2 text-neg">
      <strong className="font-semibold">Request failed.</strong> <span>{message}</span>
      {onRetry ? (
        <button type="button" className="ws-btn ml-2" onClick={onRetry}>
          Retry
        </button>
      ) : null}
    </div>
  )
}

export function EmptyState({ children }: { children: ReactNode }) {
  return <div className="px-3 py-6 text-center text-muted">{children}</div>
}

export function ToggleButton({
  pressed,
  onClick,
  children,
  title,
  disabled,
}: {
  pressed: boolean
  onClick: () => void
  children: ReactNode
  title?: string
  disabled?: boolean
}) {
  return (
    <button
      type="button"
      className="ws-btn"
      aria-pressed={pressed}
      title={title}
      disabled={disabled}
      onClick={onClick}
    >
      {children}
    </button>
  )
}

export function NumberField({
  label,
  value,
  onCommit,
  step = 1,
  min,
  max,
  width = '5.5rem',
  placeholder,
  disabled,
}: {
  label: string
  value: number | null
  onCommit: (value: number | null) => void
  step?: number
  min?: number
  max?: number
  width?: string
  placeholder?: string
  disabled?: boolean
}) {
  const [draft, setDraft] = useState<string | null>(null)
  const shown = draft ?? (value === null ? '' : String(value))
  const commit = (): void => {
    if (draft === null) return
    const trimmed = draft.trim()
    setDraft(null)
    if (trimmed === '') {
      onCommit(null)
      return
    }
    const parsed = Number(trimmed)
    if (Number.isFinite(parsed)) onCommit(parsed)
  }
  return (
    <label className="flex items-center gap-1">
      <span className="ws-label">{label}</span>
      <input
        className="ws-input"
        style={{ width }}
        type="number"
        inputMode="decimal"
        step={step}
        min={min}
        max={max}
        disabled={disabled}
        placeholder={placeholder}
        value={shown}
        aria-label={label}
        onChange={(event) => setDraft(event.target.value)}
        onBlur={commit}
        onKeyDown={(event) => {
          if (event.key === 'Enter') commit()
          if (event.key === 'Escape') setDraft(null)
        }}
      />
    </label>
  )
}

export function Slider({
  label,
  value,
  min,
  max,
  step,
  onChange,
  format,
  width = '9rem',
}: {
  label: string
  value: number
  min: number
  max: number
  step: number
  onChange: (value: number) => void
  format: (value: number) => string
  width?: string
}) {
  return (
    <label className="flex items-center gap-1.5">
      <span className="ws-label w-14 shrink-0 text-right">{label}</span>
      <input
        className="ws-range"
        style={{ width }}
        type="range"
        min={min}
        max={max}
        step={step}
        value={value}
        aria-label={label}
        onChange={(event) => onChange(Number(event.target.value))}
      />
      <span className="num w-14 shrink-0 text-[11px] text-fg">{format(value)}</span>
    </label>
  )
}

export function SwatchLine({ color, dashed }: { color: string; dashed?: boolean }) {
  const style: CSSProperties = dashed
    ? { borderTop: `2px dashed ${color}` }
    : { borderTop: `2px solid ${color}` }
  return <span aria-hidden="true" className="inline-block w-4 align-middle" style={style} />
}
