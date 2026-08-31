/**
 * Server-side folder picker for the trades root.
 *
 * The SPA never sees the user's disk — every listing comes from
 * `GET /api/v1/fs/dirs` on the machine running `wingspan serve`, which returns
 * directories only. Picking a folder is `PUT /api/v1/trades/root`; the server
 * runs the same discovery probe as startup, so any directory may be offered
 * and a 422 comes back with the discovery sentence when it does not qualify.
 */

import { useEffect, useId, useRef, useState } from 'react'
import { ApiError } from '../api/client'
import { useFsDirs, useResetTradesRoot, useSetTradesRoot } from '../api/queries'
import type { FsDirEntry, TradesRootResponse } from '../api/types'
import { Modal, Pill, Spinner, ToggleButton } from './ui'

/** How long the "loaded" confirmation stays up before the dialog closes. */
export const CONFIRM_MS = 900

const LAYOUT_HINT =
  'Open Trades / Closed Trades / Daily Trade Statement live here, or one level down.'

function problemDetail(error: unknown): string {
  if (error instanceof ApiError) return error.problem?.detail ?? error.message
  return error instanceof Error ? error.message : String(error)
}

function basename(path: string): string {
  const trimmed = path.replace(/\/+$/, '')
  const cut = trimmed.lastIndexOf('/')
  return cut < 0 ? trimmed : (trimmed.slice(cut + 1) || '/')
}

export function TradesRootDialog({
  root,
  onClose,
}: {
  root: TradesRootResponse | undefined
  onClose: () => void
}) {
  const titleId = useId()
  const descId = useId()

  // `null` lets the server choose the opening directory (the active root's
  // parent, else the home dir) — the contract's default for `path`.
  const [path, setPath] = useState<string | null>(null)
  const [draft, setDraft] = useState<string | null>(null)
  const [hidden, setHidden] = useState(false)
  const [chosen, setChosen] = useState<string | null>(null)
  const [confirmation, setConfirmation] = useState<string | null>(null)

  const dirs = useFsDirs(path, hidden, true)
  const setRoot = useSetTradesRoot()
  const resetRoot = useResetTradesRoot()

  const listing = dirs.data
  // `placeholderData` keeps the folder you were in on screen while the next
  // one loads, so the list does not flash empty. It must not *decide*
  // anything though: until the listing is the one we asked for, the path
  // field and Select go by the request, never by the folder left behind.
  const stale = dirs.isPlaceholderData
  const listedPath = stale ? null : (listing?.path ?? null)

  // The last `home` the server sent, kept because a failed listing has none of
  // its own and Home is the way back out of a path that does not exist.
  // Adjusted during render rather than in an effect: it is derived from the
  // listing directly above it, and there is no event to hang it off.
  const [home, setHome] = useState<string | null>(null)
  if (listing?.home !== undefined && listing.home !== home) setHome(listing.home)

  const timer = useRef<ReturnType<typeof setTimeout> | null>(null)
  useEffect(() => () => {
    if (timer.current !== null) clearTimeout(timer.current)
  }, [])

  const navigate = (next: string): void => {
    setDraft(null)
    setChosen(null)
    setRoot.reset()
    resetRoot.reset()
    setPath(next)
  }

  const target = chosen ?? listedPath ?? path
  const busy = setRoot.isPending || resetRoot.isPending || confirmation !== null

  const succeed = (data: TradesRootResponse): void => {
    setConfirmation(
      `${data.open_trades} open / ${data.closed_trades} closed trades loaded from ${data.root}`,
    )
    timer.current = setTimeout(onClose, CONFIRM_MS)
  }

  const select = (): void => {
    if (target === null || busy) return
    // Only one of the two can be showing a message at a time.
    resetRoot.reset()
    setRoot.mutate(target, { onSuccess: succeed })
  }

  const reset = (): void => {
    if (busy) return
    setRoot.reset()
    resetRoot.mutate(undefined, { onSuccess: succeed })
  }

  const failure = setRoot.error ?? resetRoot.error
  const fieldValue = draft ?? listedPath ?? path ?? ''

  return (
    <Modal labelledBy={titleId} describedBy={descId} onClose={onClose} className="max-w-[46rem]">
      <header className="flex shrink-0 items-center justify-between gap-2 border-b border-line px-3 py-2">
        <div>
          <h2 id={titleId} className="m-0 text-[13px] font-semibold text-fg-strong">
            Trades folder
          </h2>
          <p id={descId} className="m-0 text-[11px] text-muted">
            Browsing the folders on the machine running Wingspan. {LAYOUT_HINT}
          </p>
        </div>
        <button type="button" className="ws-btn" onClick={onClose} aria-label="Close">
          ✕
        </button>
      </header>

      <div className="flex shrink-0 flex-wrap items-center gap-2 border-b border-line px-3 py-2">
        <button
          type="button"
          className="ws-btn"
          // A headless server process may have no home directory at all.
          onClick={() => home !== null && navigate(home)}
          disabled={busy || home === null}
          title="Go to the home directory"
        >
          ⌂ Home
        </button>
        <button
          type="button"
          className="ws-btn"
          onClick={() => listing?.parent !== null && listing !== undefined && navigate(listing.parent)}
          disabled={busy || listing?.parent === null || listing === undefined}
          title="Go to the parent folder"
        >
          ↑ Up
        </button>
        <form
          className="flex min-w-[16rem] flex-1 items-center gap-2"
          onSubmit={(event) => {
            event.preventDefault()
            const next = fieldValue.trim()
            if (next !== '') navigate(next)
          }}
        >
          <input
            data-autofocus
            className="ws-input min-w-0 flex-1"
            aria-label="Folder path"
            spellCheck={false}
            autoComplete="off"
            value={fieldValue}
            onChange={(event) => setDraft(event.target.value)}
          />
          <button type="submit" className="ws-btn" disabled={busy}>
            Go
          </button>
        </form>
        <ToggleButton
          pressed={hidden}
          onClick={() => setHidden((on) => !on)}
          title="Include dot-directories"
          disabled={busy}
        >
          Hidden
        </ToggleButton>
      </div>

      <div className="min-h-[12rem] flex-1 overflow-y-auto">
        {dirs.isError ? (
          <div role="alert" className="m-3 rounded border border-neg/50 bg-neg/10 px-3 py-2 text-neg">
            <strong className="font-semibold">Cannot list that folder.</strong>{' '}
            <span>{problemDetail(dirs.error)}</span>
            <button type="button" className="ws-btn ml-2" onClick={() => void dirs.refetch()}>
              Retry
            </button>
          </div>
        ) : dirs.isPending ? (
          <div className="p-4">
            <Spinner label="Listing folders…" />
          </div>
        ) : (
          <ul
            className={`m-0 list-none p-0 ${stale ? 'opacity-50' : ''}`}
            aria-label="Folders"
            aria-busy={stale}
          >
            {listing?.parent === null || listing === undefined ? null : (
              <li>
                <button
                  type="button"
                  aria-label="Up to parent folder"
                  className="flex w-full items-center gap-2 border-0 border-b border-line/60 bg-transparent px-3 py-1.5 text-left text-fg hover:bg-surface-2"
                  onClick={() => navigate(listing.parent as string)}
                >
                  <span aria-hidden="true" className="text-faint">
                    ↰
                  </span>
                  <span className="num text-[12px]">.. ({basename(listing.parent)})</span>
                </button>
              </li>
            )}
            {listing !== undefined && listing.entries.length === 0 ? (
              <li className="px-3 py-6 text-center text-muted">No sub-folders here.</li>
            ) : null}
            {(listing?.entries ?? []).map((entry: FsDirEntry) => {
              const active = chosen === entry.path
              return (
                <li key={entry.path}>
                  <button
                    type="button"
                    // The glyph and the badge are decorative; the name is the
                    // folder, with the layout hint spelled out for AT.
                    aria-label={
                      entry.has_trades_layout
                        ? `${entry.name} — contains trades layout`
                        : entry.name
                    }
                    aria-current={active ? 'true' : undefined}
                    className={`flex w-full items-center gap-2 border-0 border-b border-line/60 px-3 py-1.5 text-left ${
                      active ? 'bg-accent-soft text-fg-strong' : 'bg-transparent text-fg hover:bg-surface-2'
                    }`}
                    onClick={() => setChosen(entry.path)}
                    onDoubleClick={() => navigate(entry.path)}
                    onKeyDown={(event) => {
                      if (event.key !== 'Enter') return
                      event.preventDefault()
                      navigate(entry.path)
                    }}
                  >
                    <span aria-hidden="true" className="text-faint">
                      ▸
                    </span>
                    <span className="num flex-1 truncate text-[12px]">{entry.name}</span>
                    {entry.has_trades_layout ? (
                      <Pill tone="pos" title={LAYOUT_HINT}>
                        contains trades layout
                      </Pill>
                    ) : null}
                  </button>
                </li>
              )
            })}
          </ul>
        )}
      </div>

      <footer className="shrink-0 border-t border-line px-3 py-2">
        {confirmation !== null ? (
          <p role="status" className="m-0 mb-1.5 text-[12px] text-pos">
            ✓ {confirmation}
          </p>
        ) : null}
        {failure !== null && failure !== undefined ? (
          <p role="alert" className="m-0 mb-1.5 text-[12px] text-neg">
            {problemDetail(failure)}
          </p>
        ) : null}
        <div className="flex flex-wrap items-center gap-2">
          <span className="ws-label shrink-0">Selected</span>
          <span className="num min-w-0 flex-1 truncate text-[11px] text-fg" title={target ?? ''}>
            {target ?? '—'}
          </span>
          {root?.source === 'override' ? (
            <button
              type="button"
              className="ws-btn"
              disabled={busy}
              title={
                root.config_root === null
                  ? 'Drop the folder chosen here and go back to the configured one'
                  : `Back to ${root.config_root}`
              }
              onClick={reset}
            >
              Reset to config
            </button>
          ) : null}
          <button type="button" className="ws-btn" onClick={onClose}>
            Cancel
          </button>
          <button
            type="button"
            className="ws-btn"
            disabled={target === null || busy}
            onClick={select}
          >
            {setRoot.isPending ? 'Loading…' : 'Select'}
          </button>
        </div>
      </footer>
    </Modal>
  )
}
