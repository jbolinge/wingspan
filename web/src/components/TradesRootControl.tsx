/**
 * Status-bar control naming the folder the trade files come from, and the
 * doorway to the server-side folder picker.
 *
 * The button shows only the folder's basename — the full path and *how* that
 * path was decided (config, chosen here, or the demo fixtures) live in the
 * tooltip, which is mirrored into `title` so it survives touch and screen
 * readers.
 */

import { useId, useState } from 'react'
import { ApiError } from '../api/client'
import { useTradesRoot } from '../api/queries'
import type { TradesRootSource } from '../api/types'
import { TradesRootDialog } from './TradesRootDialog'

const SOURCE_LABEL: Record<TradesRootSource, string> = {
  config: 'from config',
  override: 'chosen in app',
  demo: 'demo fixtures',
}

function basename(path: string): string {
  const trimmed = path.replace(/\/+$/, '')
  const cut = trimmed.lastIndexOf('/')
  return cut < 0 ? trimmed : (trimmed.slice(cut + 1) || '/')
}

export function TradesRootControl() {
  const root = useTradesRoot()
  const [open, setOpen] = useState(false)
  const [tip, setTip] = useState(false)
  const tipId = useId()

  const data = root.data
  const label = data ? basename(data.root) : root.isError ? 'unknown' : '…'

  const summary = data
    ? `${data.root} — ${SOURCE_LABEL[data.source]}. ${data.open_trades} open / ${data.closed_trades} closed trades. Click to pick another folder.`
    : root.isError
      ? `Could not read the trades root: ${
          root.error instanceof ApiError
            ? (root.error.problem?.detail ?? root.error.message)
            : String(root.error)
        }`
      : 'Asking the server where the trade files are…'

  return (
    <span className="relative inline-flex">
      <button
        type="button"
        className="ws-btn"
        aria-label={`Trades folder: ${label}`}
        aria-describedby={tip ? tipId : undefined}
        aria-haspopup="dialog"
        aria-expanded={open}
        title={summary}
        onClick={() => setOpen(true)}
        onMouseEnter={() => setTip(true)}
        onMouseLeave={() => setTip(false)}
        onFocus={() => setTip(true)}
        onBlur={() => setTip(false)}
      >
        <span className="ws-label">trades</span>
        <span className="num max-w-[12rem] truncate text-fg-strong">{label}</span>
      </button>
      {tip ? (
        <span
          id={tipId}
          role="tooltip"
          className="pointer-events-none absolute top-full left-0 z-50 mt-1 w-max max-w-[26rem] rounded border border-line bg-surface-3 px-2 py-1.5 text-[11px] leading-snug font-normal text-fg shadow-lg"
          style={{ boxShadow: 'var(--ws-shadow)' }}
        >
          {data ? (
            <span className="block space-y-0.5">
              <span className="num block break-all text-fg-strong">{data.root}</span>
              <span className="block text-muted">
                {SOURCE_LABEL[data.source]} · {data.open_trades} open / {data.closed_trades} closed
              </span>
              <span className="block text-muted">Click to pick another folder.</span>
            </span>
          ) : (
            summary
          )}
        </span>
      ) : null}
      {open ? <TradesRootDialog root={data} onClose={() => setOpen(false)} /> : null}
    </span>
  )
}
