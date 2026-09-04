/**
 * Loading a book: a folder on *this* computer, read in place and watched.
 *
 * Trade files never leave the machine and are never copied anywhere. Picking
 * a folder is a two-step flow — probe, then commit — so a wrong pick is cheap
 * to back out of: `POST /api/v1/trades/root/probe` validates and summarises a
 * candidate folder without touching anything that is loaded; only
 * `PUT /api/v1/trades/root` (behind "Load & analyze") actually adopts it.
 *
 * Phase 3 has no OS folder picker yet — that arrives with the desktop build's
 * `pick_folder` command — so outside Tauri the folder is typed (or clicked
 * from the recent list) into a plain path field.
 */

import { useEffect, useId, useRef, useState } from 'react'
import { ApiError } from '../api/client'
import { useProbeFolder, useSetTradesRoot, useTradesRoot } from '../api/queries'
import { isTauri } from '../lib/tauri'
import { Modal } from './ui'

/** How long the load confirmation stays up before the dialog closes. */
export const CONFIRM_MS = 900

/** The load confirmation is longer: it also reports what was loaded. */
export const LOAD_CONFIRM_MS = 1800

const LAYOUT_HINT =
  'Open Trades / Closed Trades / Daily Trade Statement live here, or one level down.'

function errorDetail(error: unknown): string {
  if (error instanceof ApiError) return error.problem?.detail ?? error.message
  return error instanceof Error ? error.message : String(error)
}

export function TradesFolderDialog({ onClose }: { onClose: () => void }) {
  const titleId = useId()
  const descId = useId()
  const tauri = isTauri()

  const root = useTradesRoot()
  const probe = useProbeFolder()
  const setRoot = useSetTradesRoot()

  const [path, setPath] = useState('')
  const inputRef = useRef<HTMLInputElement>(null)
  const [confirmation, setConfirmation] = useState<string | null>(null)

  const timer = useRef<ReturnType<typeof setTimeout> | null>(null)
  useEffect(() => () => {
    if (timer.current !== null) clearTimeout(timer.current)
  }, [])

  const runProbe = (candidate: string): void => {
    const trimmed = candidate.trim()
    if (trimmed === '') return
    setRoot.reset()
    probe.mutate(trimmed)
  }

  const load = (): void => {
    if (!probe.isSuccess) return
    setRoot.mutate(probe.data.root, {
      onSuccess: (data) => {
        setConfirmation(
          `${data.open_trades} open / ${data.closed_trades} closed trades loaded from ${data.root ?? probe.data.root}`,
        )
        timer.current = setTimeout(onClose, LOAD_CONFIRM_MS)
      },
    })
  }

  const error = probe.error ?? setRoot.error

  return (
    <Modal labelledBy={titleId} describedBy={descId} onClose={onClose} className="max-w-[46rem]">
      <header className="flex shrink-0 items-center justify-between gap-2 border-b border-line px-3 py-2">
        <div>
          <h2 id={titleId} className="m-0 text-[13px] font-semibold text-fg-strong">
            Choose your trades folder
          </h2>
          <p id={descId} className="m-0 text-[11px] text-muted">
            {`Reading a folder on this computer, in place. ${LAYOUT_HINT}`}
          </p>
        </div>
        <button type="button" className="ws-btn" onClick={onClose} aria-label="Close">
          ✕
        </button>
      </header>

      <div className="flex min-h-0 flex-1 flex-col overflow-y-auto px-3 py-2">
        <div className="flex flex-wrap items-center gap-2">
          <button
            type="button"
            className="ws-btn"
            data-autofocus
            disabled={tauri}
            title={tauri ? 'Coming with the desktop build' : undefined}
            onClick={tauri ? undefined : () => inputRef.current?.focus()}
          >
            Choose folder…
          </button>
        </div>

        {!tauri ? (
          <div className="mt-2 flex items-center gap-2">
            <input
              ref={inputRef}
              type="text"
              aria-label="Folder path"
              className="ws-input flex-1"
              value={path}
              onChange={(event) => setPath(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === 'Enter') {
                  event.preventDefault()
                  runProbe(path)
                }
              }}
            />
            <button
              type="button"
              className="ws-btn"
              disabled={path.trim() === '' || probe.isPending}
              onClick={() => runProbe(path)}
            >
              {probe.isPending ? 'Checking…' : 'Go'}
            </button>
          </div>
        ) : null}

        {root.data && root.data.recent.length > 0 ? (
          <div className="mt-2">
            <p className="ws-label m-0 mb-1">Recent</p>
            <ul className="m-0 flex list-none flex-col gap-1 p-0">
              {root.data.recent.map((recentPath) => (
                <li key={recentPath}>
                  <button
                    type="button"
                    className="ws-btn num w-full justify-start truncate text-left"
                    onClick={() => {
                      setPath(recentPath)
                      runProbe(recentPath)
                    }}
                  >
                    {recentPath}
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
                aria-label="Folder summary"
              >
                <dt className="ws-label">Folder</dt>
                <dd className="num m-0 break-all text-fg-strong">{probe.data.root}</dd>
                <dt className="ws-label">Trades</dt>
                <dd className="num m-0 text-fg">
                  {probe.data.open_trades} open / {probe.data.closed_trades} closed
                </dd>
                <dt className="ws-label">Statements</dt>
                <dd className="num m-0 text-fg">{probe.data.statements}</dd>
                <dt className="ws-label">Watching</dt>
                <dd className="num m-0 text-fg">{root.data?.watching ? 'yes' : 'no'}</dd>
              </dl>
              {probe.data.note ? <p className="m-0 text-[11px] text-muted">{probe.data.note}</p> : null}
            </div>
          ) : !probe.isPending ? (
            <p className="m-0 py-6 text-center text-muted">
              Pick the folder holding <span className="num">Open Trades</span>,{' '}
              <span className="num">Closed Trades</span> and{' '}
              <span className="num">Daily Trade Statement</span> — they may also sit one level
              down.
            </p>
          ) : null}
        </div>
      </div>

      <footer className="shrink-0 border-t border-line px-3 py-2">
        {confirmation !== null ? (
          <p role="status" className="m-0 mb-1.5 text-[12px] text-pos">
            ✓ {confirmation}
          </p>
        ) : null}
        {error ? (
          <p role="alert" className="m-0 mb-1.5 text-[12px] text-neg">
            {errorDetail(error)}
          </p>
        ) : null}
        <div className="flex flex-wrap items-center justify-end gap-2">
          <button
            type="button"
            className="ws-btn"
            disabled
            title="Coming with the desktop build"
          >
            Load demo book
          </button>
          <button
            type="button"
            className="ws-btn"
            disabled={!probe.isSuccess || setRoot.isPending}
            onClick={load}
          >
            {setRoot.isPending ? 'Loading…' : 'Load & analyze'}
          </button>
        </div>
      </footer>
    </Modal>
  )
}
