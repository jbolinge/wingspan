/**
 * Status-bar control naming the book being analysed, and the doorway to
 * loading one.
 *
 * There is one way to load trades: choose a folder on the computer showing
 * this page (`TradesFolderDialog`) — it is read in place and watched, never
 * copied. The server never keeps a copy of its own — a deployer may still
 * point `[trades] root` at one, and `--demo` has its bundled book, but
 * neither can be changed from here, and with neither there is simply nothing
 * loaded until a folder is chosen.
 *
 * The button shows where the book came from; the full detail — the source,
 * the counts, whether it is being watched — lives in the tooltip, mirrored
 * into `title` so it survives touch and screen readers.
 */

import { useEffect, useId, useState } from 'react'
import { ApiError } from '../api/client'
import { useTradesRoot } from '../api/queries'
import type { TradesRootSource } from '../api/types'
import { claimFirstRunPrompt } from '../lib/firstRun'
import { TradesFolderDialog } from './TradesFolderDialog'

const SOURCE_LABEL: Record<TradesRootSource, string> = {
  config: 'from the config file',
  demo: 'demo book',
  local: 'folder on this computer',
  empty: 'nothing loaded',
}

/**
 * Splits on either `/` or `\`. Only `/` is a real separator on Linux — a
 * backslash is a legal character in a directory name there — but treating
 * both this way is the safe default: a path that arrived with backslashes as
 * its only separators (pasted from elsewhere) would otherwise never match
 * `/`, and the *entire* string would become `label`, which the button's
 * `truncate` class then chops visually instead of showing a clean basename.
 */
function basename(path: string): string {
  const trimmed = path.replace(/[\\/]+$/, '')
  const cut = trimmed.search(/[\\/][^\\/]*$/)
  return cut < 0 ? trimmed : (trimmed.slice(cut + 1) || '/')
}

export function TradesRootControl() {
  const root = useTradesRoot()
  const [open, setOpen] = useState(false)
  const [tip, setTip] = useState(false)
  const tipId = useId()

  const data = root.data

  // First run only: a brand-new install with no book and nothing configured
  // is shown the folder dialog itself, once per process. `claimFirstRunPrompt`
  // is what stops a later refetch — a Clear that empties the book — reopening
  // it behind the user's back.
  useEffect(() => {
    if (!root.isSuccess || !claimFirstRunPrompt()) return
    if (root.data.source === 'empty' && root.data.config_root === null) setOpen(true)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [root.isSuccess])

  const label = data
    ? data.root === null
      ? 'none'
      : basename(data.root)
    : root.isError
      ? 'unknown'
      : '…'
  const watchingChip = data ? (data.watching ? 'watching' : 'not watching') : null

  const where = data ? (data.root ?? 'No trades loaded') : ''
  const summary = data
    ? `${where} — ${SOURCE_LABEL[data.source]}. ${data.open_trades} open / ${
        data.closed_trades
      } closed trades. Click to choose a folder on this computer.`
    : root.isError
      ? `Could not read the trades root: ${
          root.error instanceof ApiError
            ? (root.error.problem?.detail ?? root.error.message)
            : String(root.error)
        }`
      : 'Checking which trades are loaded…'

  return (
    <span className="relative inline-flex items-center gap-1">
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
        {watchingChip !== null ? <span className="num text-faint">{watchingChip}</span> : null}
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
              <span className="num block break-all text-fg-strong">{where}</span>
              <span className="block text-muted">
                {SOURCE_LABEL[data.source]} · {data.open_trades} open / {data.closed_trades} closed
              </span>
              <span className="block text-muted">
                Click to choose a folder on this computer.
              </span>
            </span>
          ) : (
            summary
          )}
        </span>
      ) : null}
      {open ? <TradesFolderDialog onClose={() => setOpen(false)} /> : null}
    </span>
  )
}
