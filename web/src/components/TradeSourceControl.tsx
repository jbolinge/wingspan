/**
 * Status-bar control naming the book being analysed, and the doorway to
 * loading one.
 *
 * There is one way to load trades: import a Google Sheet "Legs" tab, or a CSV
 * file with the same columns (`TradeSourceDialog`). The sheet is fetched over
 * HTTPS by Rust — the webview never talks to Google — and a CSV is read in
 * place; neither file is moved, renamed or changed. A deployer may still point `[trades]
 * sheet_url`/`csv_path` at one, and `--demo` has its bundled book, but
 * neither can be changed from here, and with neither there is simply nothing
 * imported until a sheet or CSV is chosen.
 *
 * The button shows where the book came from; the full detail — the source,
 * the counts, whether it is stale — lives in the tooltip, mirrored into
 * `title` so it survives touch and screen readers.
 */

import { useEffect, useId, useState } from 'react'
import { ApiError } from '../api/client'
import { useTradeSource } from '../api/queries'
import type { TradeSourceKind } from '../api/types'
import { claimFirstRunPrompt } from '../lib/firstRun'
import { fmtAge } from '../lib/format'
import { usePickedFile } from '../lib/pickedFile'
import { useUiSignals } from '../lib/uiSignals'
import { TradeSourceDialog } from './TradeSourceDialog'

const SOURCE_LABEL: Record<TradeSourceKind, string> = {
  sheet: 'Google Sheet',
  csv: 'CSV file on this computer',
  config: 'set in the config file',
  demo: 'demo book',
  empty: 'nothing imported',
  manual: 'manual entry',
}

/**
 * Sheet or file is decided by `path`, never by `kind`: a deployer's `[trades]
 * sheet_url` and a deployer's `[trades] csv_path` are *both* `kind: 'config'`
 * (`docs/IPC.md`), and only `path` tells them apart — it is non-null exactly
 * when the book came off a file. Reading `kind === 'sheet'` here is what used
 * to chop a masked sheet label (`docs.google.com/…/1AbCdE…abcd · Legs`) at its
 * last slash for a configured sheet.
 */
function isFile(data: { path: string | null }): boolean {
  return data.path !== null && data.path !== ''
}

/** What `kind` means in a sentence, with `config` resolved to what it is. */
function kindLabel(data: { kind: TradeSourceKind; path: string | null }): string {
  if (data.kind !== 'config') return SOURCE_LABEL[data.kind]
  return isFile(data) ? 'CSV file set in the config file' : 'Google Sheet set in the config file'
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

export function TradeSourceControl() {
  const source = useTradeSource()
  const [open, setOpen] = useState(false)
  const [tip, setTip] = useState(false)
  const tipId = useId()

  const data = source.data

  // First run only: a brand-new install with nothing imported and nothing
  // configured is shown the import dialog itself, once per process.
  // `claimFirstRunPrompt` is what stops a later refetch — a Clear that empties
  // the book — reopening it behind the user's back.
  useEffect(() => {
    if (!source.isSuccess || !claimFirstRunPrompt()) return
    if (source.data.kind === 'empty') setOpen(true)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [source.isSuccess])

  // The native menu's "Import trades…" and Settings' "Import trades…" have no
  // component of their own to open this dialog on, so they emit a signal
  // instead of a prop.
  useEffect(() => useUiSignals.subscribe((state) => {
    if (state.last?.signal === 'open-source-dialog') setOpen(true)
  }), [])

  // A CSV file picked in the OS chooser is acted on even if the dialog that
  // asked for it is gone by the time it comes back — the chooser is a window
  // of its own and nothing stops the user closing ours behind it. This
  // control outlives the dialog, so it is the one that can reopen it; the
  // dialog then claims the pick and probes it. See `lib/pickedFile`.
  useEffect(() => usePickedFile.subscribe((state) => {
    if (state.pending !== null) setOpen(true)
  }), [])

  const label = data
    ? data.kind === 'empty'
      ? 'none'
      : data.kind === 'demo'
        ? 'demo book'
        : isFile(data)
          ? basename(data.path ?? data.label ?? '') || 'none'
          : (data.label ?? 'sheet')
    : source.isError
      ? 'unknown'
      : '…'

  let ageSeconds: number | null = null
  if (data?.fetched_at) {
    const parsed = Date.parse(data.fetched_at)
    if (!Number.isNaN(parsed)) ageSeconds = (Date.now() - parsed) / 1000
  }
  const chip =
    data?.stale === true ? (ageSeconds !== null ? `cached · ${fmtAge(ageSeconds)}` : 'cached') : null

  const where = data ? (data.label ?? 'nothing imported') : ''
  const demoNote =
    data?.kind === 'demo' ? ' The demo book is loaded; the imported source is untouched.' : ''
  const tabNote = data?.tab ? ` Tab ${data.tab}.` : ''
  const legsNote = data
    ? ` ${data.legs} legs${data.rows_skipped > 0 ? `, ${data.rows_skipped} skipped` : ''}.`
    : ''
  const detailNote = data?.detail ? ` ${data.detail}` : ''
  const summary = data
    ? `${where} — ${kindLabel(data)}.${tabNote} ${data.open_trades} open / ${
        data.closed_trades
      } closed trades.${legsNote}${demoNote}${detailNote} Click to import a Legs sheet or CSV file.`
    : source.isError
      ? `Could not read the trade source: ${
          source.error instanceof ApiError
            ? (source.error.problem?.detail ?? source.error.message)
            : String(source.error)
        }`
      : 'Checking which trades are imported…'

  return (
    <span className="relative inline-flex items-center gap-1">
      <button
        type="button"
        className="ws-btn"
        aria-label={`Trade source: ${label}`}
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
        <span className="num max-w-[12rem] truncate text-fg-strong">
          {data?.kind === 'empty' ? 'import trades…' : label}
        </span>
        {chip !== null ? <span className="num text-faint">{chip}</span> : null}
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
                {kindLabel(data)}
                {data.tab ? ` · tab ${data.tab}` : ''} · {data.open_trades} open /{' '}
                {data.closed_trades} closed
              </span>
              {data.kind === 'demo' ? (
                <span className="block text-muted">
                  The demo book is loaded; the imported source is untouched.
                </span>
              ) : null}
              {data.detail ? <span className="block text-warn">{data.detail}</span> : null}
              <span className="block text-muted">Click to import a Legs sheet or CSV file.</span>
            </span>
          ) : (
            summary
          )}
        </span>
      ) : null}
      {open ? <TradeSourceDialog onClose={() => setOpen(false)} /> : null}
    </span>
  )
}
