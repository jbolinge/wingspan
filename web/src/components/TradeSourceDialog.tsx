/**
 * Importing a book: a Google Sheet "Legs" tab, or a CSV file with the same
 * columns.
 *
 * Neither is moved, renamed or changed. The sheet is fetched over HTTPS by
 * Rust — the webview never talks to Google — and a CSV is read in place from
 * wherever it sits on this computer (its rows are then cached inside the app's
 * own state directory, as `pick_csv_file`'s confirmation says). Loading is a two-step flow — probe, then commit — so
 * a wrong link or a mistyped path is cheap to back out of: `probe_trade_source`
 * validates and summarises a candidate source without touching anything that
 * is loaded; only `set_trade_source` (behind "Load & analyze") actually
 * adopts it. See `docs/IPC.md` for the full command contract, including
 * `trade_source` (the current book) and `clear_trade_source` (Clear).
 *
 * **Load & analyze commits the request that was probed**, held in
 * `lastRequest`, never anything rebuilt from `probe.data` — `probe.data.label`
 * is a display string (masked for a sheet), not a value the server can act on
 * a second time.
 *
 * Inside the desktop app "Pick a CSV file…" opens the OS chooser
 * (`pick_csv_file`, a Rust-side dialog); in the browser there is none, so the
 * path is typed (or clicked from the recent list) into a plain text field
 * instead.
 */

import { useEffect, useId, useRef, useState } from 'react'
import { errorDetail, errorSlug } from '../api/client'
import {
  useLeaveDemo,
  useLoadDemo,
  usePickCsvFile,
  useProbeTradeSource,
  useSetTradeSource,
  useTradeSource,
} from '../api/queries'
import type { TradeSourceRequest } from '../api/types'
import { usePickedFile } from '../lib/pickedFile'
import { isTauri } from '../lib/tauri'
import { Modal } from './ui'

/** How long the load confirmation stays up before the dialog closes. */
export const CONFIRM_MS = 900

/** The load confirmation is longer: it also reports what was loaded. */
export const LOAD_CONFIRM_MS = 1800

/** One extra hint line under the server's own sentence, keyed by error slug. */
const SLUG_HINT: Record<string, string> = {
  'sheet-not-shared':
    'In Google Sheets: Share → General access → Anyone with the link → Viewer. Wingspan never signs in and never asks for a password.',
  'sheet-tab-not-found':
    'Check the tab name on the strip at the bottom of the spreadsheet — it is case-sensitive, and the default is “Legs”.',
}

export function TradeSourceDialog({ onClose }: { onClose: () => void }) {
  const titleId = useId()
  const descId = useId()
  const tauri = isTauri()

  const source = useTradeSource()
  const probe = useProbeTradeSource()
  const setSource = useSetTradeSource()
  const pickCsv = usePickCsvFile()
  const loadDemo = useLoadDemo()
  const leaveDemo = useLeaveDemo()

  const [url, setUrl] = useState('')
  const [tab, setTab] = useState(() => source.data?.tab ?? 'Legs')
  const [csvPath, setCsvPath] = useState('')
  const csvInputRef = useRef<HTMLInputElement>(null)
  const [lastRequest, setLastRequest] = useState<TradeSourceRequest | null>(null)
  const [confirmation, setConfirmation] = useState<string | null>(null)
  const [pickFailure, setPickFailure] = useState<string | null>(null)

  const timer = useRef<ReturnType<typeof setTimeout> | null>(null)
  useEffect(() => () => {
    if (timer.current !== null) clearTimeout(timer.current)
  }, [])

  const runProbe = (request: TradeSourceRequest): void => {
    setSource.reset()
    setLastRequest(request)
    probe.mutate(request)
  }

  const checkSheet = (): void => {
    const trimmed = url.trim()
    if (trimmed === '') return
    const trimmedTab = tab.trim()
    runProbe({ kind: 'sheet', url: trimmed, tab: trimmedTab === '' ? undefined : trimmedTab })
  }

  const checkCsvPath = (): void => {
    const trimmed = csvPath.trim()
    if (trimmed === '') return
    runProbe({ kind: 'csv', path: trimmed })
  }

  // The chooser's answer is claimed from the store, not from `mutate`'s own
  // callbacks: it is parked there precisely because this component may have
  // unmounted and come back (`TradeSourceControl` reopens the dialog for a
  // pick that outlived it) while the chooser was up. See `lib/pickedFile`.
  const pending = usePickedFile((state) => state.pending)
  useEffect(() => {
    if (pending === null) return
    const outcome = usePickedFile.getState().claim()
    // Already claimed — a second effect run (StrictMode) has nothing to do.
    if (outcome === null) return
    if (outcome.kind === 'file') {
      setPickFailure(null)
      runProbe({ kind: 'csv', path: outcome.path })
    } else {
      setPickFailure(outcome.detail)
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pending])

  const chooseCsvFile = (): void => {
    setPickFailure(null)
    // No `onSuccess` here on purpose: the path arrives through the effect
    // above, which survives this dialog being closed and reopened. A cancel
    // parks nothing — nothing was picked, so there is nothing to say.
    pickCsv.mutate()
  }

  const load = (): void => {
    if (!probe.isSuccess || lastRequest === null) return
    setSource.mutate(lastRequest, {
      onSuccess: (data) => {
        setConfirmation(
          `${data.open_trades} open / ${data.closed_trades} closed trades loaded from ${
            data.label ?? probe.data.label
          }`,
        )
        timer.current = setTimeout(onClose, LOAD_CONFIRM_MS)
      },
    })
  }

  const loadDemoBook = (): void => {
    loadDemo.mutate(undefined, {
      onSuccess: (data) => {
        setPickFailure(null)
        setConfirmation(
          `${data.open_trades} open / ${data.closed_trades} closed trades loaded — the demo book`,
        )
        timer.current = setTimeout(onClose, LOAD_CONFIRM_MS)
      },
    })
  }

  const leaveDemoBook = (): void => {
    leaveDemo.mutate(undefined, {
      onSuccess: (data) => {
        setPickFailure(null)
        setConfirmation(
          `back to ${data.open_trades} open / ${data.closed_trades} closed trades${
            data.label ? ` from ${data.label}` : ''
          }`,
        )
        timer.current = setTimeout(onClose, LOAD_CONFIRM_MS)
      },
    })
  }

  const error = probe.error ?? setSource.error ?? loadDemo.error ?? leaveDemo.error
  // `pickCsv.error` is not read here: a chooser that failed while this dialog
  // was unmounted has no mutation left to ask, so the failure comes back
  // through the store like a picked path does.
  const errorText = error ? errorDetail(error) : pickFailure
  const hint = error ? SLUG_HINT[errorSlug(error) ?? ''] : undefined

  const recent = source.data?.recent ?? []
  const columnsMissing = probe.isSuccess ? probe.data.columns_missing.length > 0 : false

  return (
    // Not dismissable while the chooser is up: a click on the backdrop or an
    // Escape aimed at the chooser would otherwise unmount the dialog the
    // pick is coming back to.
    <Modal
      labelledBy={titleId}
      describedBy={descId}
      onClose={onClose}
      className="max-w-[46rem]"
      dismissable={!pickCsv.isPending}
    >
      <header className="flex shrink-0 items-center justify-between gap-2 border-b border-line px-3 py-2">
        <div>
          <h2 id={titleId} className="m-0 text-[13px] font-semibold text-fg-strong">
            Import trades
          </h2>
          <p id={descId} className="m-0 text-[11px] text-muted">
            A Google Sheet “Legs” tab shared anyone-with-the-link → Viewer, or a CSV file with the
            same columns. Wingspan reads them where they are and never copies anything.
          </p>
        </div>
        <button type="button" className="ws-btn" onClick={onClose} aria-label="Close">
          ✕
        </button>
      </header>

      <div className="flex min-h-0 flex-1 flex-col overflow-y-auto px-3 py-2">
        <div className="flex flex-wrap items-center gap-2">
          <input
            aria-label="Sheet link"
            type="text"
            data-autofocus
            className="ws-input min-w-[16rem] flex-1"
            placeholder="Paste a Google Sheet link or id"
            value={url}
            onChange={(event) => setUrl(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === 'Enter') {
                event.preventDefault()
                checkSheet()
              }
            }}
          />
          <input
            aria-label="Tab"
            type="text"
            className="ws-input w-24"
            value={tab}
            onChange={(event) => setTab(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === 'Enter') {
                event.preventDefault()
                checkSheet()
              }
            }}
          />
          <button
            type="button"
            className="ws-btn"
            disabled={url.trim() === '' || probe.isPending}
            onClick={checkSheet}
          >
            {probe.isPending ? 'Checking…' : 'Check'}
          </button>
        </div>

        <div className="mt-2 flex flex-wrap items-center gap-2">
          <button
            type="button"
            className="ws-btn"
            disabled={pickCsv.isPending}
            onClick={tauri ? chooseCsvFile : () => csvInputRef.current?.focus()}
          >
            Pick a CSV file…
          </button>
          {pickCsv.isPending ? (
            <span role="status" className="text-[11px] text-muted">
              Waiting for the file chooser…
            </span>
          ) : null}
        </div>

        {!tauri ? (
          <div className="mt-2 flex items-center gap-2">
            <input
              ref={csvInputRef}
              type="text"
              aria-label="CSV path"
              className="ws-input flex-1"
              value={csvPath}
              onChange={(event) => setCsvPath(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === 'Enter') {
                  event.preventDefault()
                  checkCsvPath()
                }
              }}
            />
            <button
              type="button"
              className="ws-btn"
              disabled={csvPath.trim() === '' || probe.isPending}
              onClick={checkCsvPath}
            >
              {probe.isPending ? 'Checking…' : 'Go'}
            </button>
          </div>
        ) : null}

        {recent.length > 0 ? (
          <div className="mt-2">
            <p className="m-0 text-[11px] text-muted">
              A sheet you loaded before is listed here; paste its link again to re-import it.
            </p>
            <p className="ws-label m-0 mt-1 mb-1">Recent</p>
            <ul className="m-0 flex list-none flex-col gap-1 p-0">
              {recent.map((entry, index) => (
                <li key={`${entry.kind}-${entry.label}-${index}`}>
                  <button
                    type="button"
                    className="ws-btn num w-full justify-start truncate text-left"
                    onClick={() => {
                      if (entry.kind !== 'csv') return
                      setCsvPath(entry.label)
                      runProbe({ kind: 'csv', path: entry.label })
                    }}
                  >
                    {entry.label}
                    {entry.tab !== null ? ` · ${entry.tab}` : ''}
                  </button>
                </li>
              ))}
            </ul>
          </div>
        ) : null}

        <div className="mt-2 min-h-[6rem] flex-1">
          {probe.isSuccess ? (
            <div className="space-y-1.5">
              <dl
                className="m-0 grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-[12px]"
                role="group"
                aria-label="Source summary"
              >
                <dt className="ws-label">Source</dt>
                <dd className="num m-0 break-all text-fg-strong">{probe.data.label}</dd>
                <dt className="ws-label">Tab</dt>
                <dd className="num m-0 text-fg">{probe.data.tab ?? '—'}</dd>
                <dt className="ws-label">Rows</dt>
                <dd className="num m-0 text-fg">{probe.data.rows}</dd>
                <dt className="ws-label">Legs</dt>
                <dd className="num m-0 text-fg">{probe.data.legs}</dd>
                <dt className="ws-label">Skipped</dt>
                <dd className="num m-0 text-fg">{probe.data.rows_skipped}</dd>
                <dt className="ws-label">Trades</dt>
                <dd className="num m-0 text-fg">
                  {probe.data.trades.open} open / {probe.data.trades.closed} closed
                </dd>
                <dt className="ws-label">Found</dt>
                <dd className="num m-0 text-fg">{probe.data.columns_found.join(', ') || '—'}</dd>
                <dt className="ws-label">Absent</dt>
                <dd className="num m-0 text-fg">
                  {probe.data.columns_absent.join(', ') || 'none'}
                </dd>
                <dt className="ws-label">Missing</dt>
                <dd className="num m-0 text-fg">
                  {probe.data.columns_missing.join(', ') || 'none'}
                </dd>
                <dt className="ws-label">Ignored</dt>
                <dd className="num m-0 text-fg">
                  {probe.data.columns_ignored.join(', ') || 'none'}
                </dd>
              </dl>
              {probe.data.columns_missing.length > 0 ? (
                <p role="alert" className="m-0 text-[11px] text-neg">
                  Wingspan cannot import this: the header is missing{' '}
                  {probe.data.columns_missing.join(', ')}.
                </p>
              ) : null}
              {probe.data.columns_absent.length > 0 ? (
                <p className="m-0 text-[11px] text-warn">
                  {probe.data.columns_absent.join(', ')} is missing or blank — the trades load
                  without a cost basis or days-in-trade.
                </p>
              ) : null}
              {probe.data.note ? (
                <p className="m-0 text-[11px] text-muted">{probe.data.note}</p>
              ) : null}
            </div>
          ) : probe.isPending ? (
            <p className="m-0 py-6 text-center text-muted">Checking that source…</p>
          ) : (
            <p className="m-0 py-6 text-center text-muted">
              Paste the link to a Google Sheet “Legs” tab, or pick a CSV file with the same
              columns, to see what Wingspan finds.
            </p>
          )}
        </div>
      </div>

      <footer className="shrink-0 border-t border-line px-3 py-2">
        {confirmation !== null ? (
          <p role="status" className="m-0 mb-1.5 text-[12px] text-pos">
            ✓ {confirmation}
          </p>
        ) : null}
        {errorText !== null ? (
          <>
            <p role="alert" className="m-0 mb-1 text-[12px] text-neg">
              {errorText}
            </p>
            {hint ? <p className="m-0 mb-1.5 text-[11px] text-muted">{hint}</p> : null}
          </>
        ) : null}
        <div className="flex flex-wrap items-center justify-end gap-2">
          <button
            type="button"
            className="ws-btn"
            disabled={!tauri || loadDemo.isPending}
            title={tauri ? undefined : 'Available in the Wingspan desktop app'}
            onClick={loadDemoBook}
          >
            {loadDemo.isPending ? 'Loading…' : 'Load demo book'}
          </button>
          {source.data?.kind === 'demo' ? (
            <button
              type="button"
              className="ws-btn"
              disabled={leaveDemo.isPending}
              onClick={leaveDemoBook}
            >
              {leaveDemo.isPending ? 'Leaving…' : 'Leave demo'}
            </button>
          ) : null}
          <button
            type="button"
            className="ws-btn"
            disabled={!probe.isSuccess || lastRequest === null || columnsMissing || setSource.isPending}
            onClick={load}
          >
            {setSource.isPending ? 'Loading…' : 'Load & analyze'}
          </button>
        </div>
      </footer>
    </Modal>
  )
}
