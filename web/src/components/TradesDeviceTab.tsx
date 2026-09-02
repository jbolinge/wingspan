/**
 * "On this device": analyze trade files that live on the computer showing the
 * SPA. It is the only way trades reach the app — the server keeps none of its
 * own, and the copy this uploads is deleted again by Clear.
 *
 * The browser reads the folder the user picks — with the File System Access
 * API where it exists, else a `webkitdirectory` input — and this panel filters
 * it to the trades layout *before* anything is sent, so the user sees exactly
 * what will leave the device. Uploading is `POST /api/v1/trades/upload`, one
 * multipart part per kept file whose filename is the path relative to the
 * picked folder (docs/API.md).
 *
 * What lands on the server is a snapshot: it cannot watch this disk, so later
 * edits here only arrive on the next re-sync. The panel says so out loud.
 *
 * The copy lives under `<state_dir>/uploads/current` until the user clears
 * it, and clearing deletes the whole tree — see `ClearTradesControl`.
 */

import { useEffect, useMemo, useRef, useState } from 'react'
import { ApiError } from '../api/client'
import { useUploadTrades } from '../api/queries'
import type { TradesUploadResponse } from '../api/types'
import { pickDirectory, supportsDirectoryPicker } from '../lib/deviceFolder'
import {
  buildSelection,
  buildUploadForm,
  capMessage,
  entriesFromFileList,
  fmtBytes,
  type DeviceEntry,
  type DeviceSelection,
  type SkipReason,
} from '../lib/deviceUpload'
import { Pill, Spinner } from './ui'

export const SNAPSHOT_NOTE =
  'An upload is a snapshot — edits you make on this device arrive on the next re-sync.'

const SKIP_LABEL: Record<SkipReason, string> = {
  dotfile: 'hidden',
  'outside-layout': 'outside the trades layout',
  'too-large': 'over 4 MiB',
}

/** "2 outside the trades layout, 1 hidden" — counted, in a stable order. */
function skipSummary(selection: DeviceSelection): string {
  const order: SkipReason[] = ['outside-layout', 'dotfile', 'too-large']
  return order
    .map((reason) => ({ reason, n: selection.skipped.filter((f) => f.reason === reason).length }))
    .filter((entry) => entry.n > 0)
    .map((entry) => `${entry.n} ${SKIP_LABEL[entry.reason]}`)
    .join(', ')
}

function failureTitle(error: unknown): string {
  if (error instanceof ApiError && error.status === 413) return 'Too large for one upload.'
  if (error instanceof ApiError && error.status === 422) return 'No trades layout in that folder.'
  return 'Upload failed.'
}

function failureDetail(error: unknown): string {
  if (error instanceof ApiError) return error.problem?.detail ?? error.message
  return error instanceof Error ? error.message : String(error)
}

type Phase = 'idle' | 'reading' | 'uploading'

export function TradesDeviceTab({
  autoFocus = false,
  onUploaded,
}: {
  autoFocus?: boolean
  onUploaded: (result: TradesUploadResponse) => void
}) {
  const inputRef = useRef<HTMLInputElement>(null)
  const [folder, setFolder] = useState<string | null>(null)
  const [entries, setEntries] = useState<DeviceEntry[] | null>(null)
  const [readError, setReadError] = useState<string | null>(null)
  const [phase, setPhase] = useState<Phase>('idle')
  const upload = useUploadTrades()
  // Guards the async pick/upload callbacks against a closed dialog. Set on
  // mount, not just at init: StrictMode mounts, unmounts and mounts again.
  const alive = useRef(true)
  useEffect(() => {
    alive.current = true
    return () => {
      alive.current = false
    }
  }, [])

  const selection = useMemo(
    () => (entries === null ? null : buildSelection(entries)),
    [entries],
  )
  const cap = selection === null ? null : capMessage(selection)
  const busy = phase !== 'idle' || upload.isPending

  const accept = (folderName: string, picked: DeviceEntry[]): void => {
    setFolder(folderName)
    setEntries(picked)
    upload.reset()
  }

  /** The native picker when the browser has one, else the hidden input. */
  const choose = (): void => {
    setReadError(null)
    if (!supportsDirectoryPicker()) {
      inputRef.current?.click()
      return
    }
    setPhase('reading')
    void pickDirectory()
      .then((pick) => {
        if (!alive.current) return
        if (pick !== null) accept(pick.folder, pick.entries)
      })
      .catch((error: unknown) => {
        if (!alive.current) return
        setReadError(error instanceof Error ? error.message : String(error))
      })
      .finally(() => {
        if (alive.current) setPhase('idle')
      })
  }

  const send = (): void => {
    if (selection === null || selection.kept.length === 0 || cap !== null || busy) return
    setPhase('uploading')
    upload.mutate(buildUploadForm(selection), {
      onSuccess: onUploaded,
      onSettled: () => {
        if (alive.current) setPhase('idle')
      },
    })
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="shrink-0 border-b border-line px-3 py-2">
        <div className="flex flex-wrap items-center gap-2">
          <button
            type="button"
            className="ws-btn"
            data-autofocus={autoFocus ? '' : undefined}
            disabled={busy}
            onClick={choose}
          >
            {folder === null ? 'Choose folder…' : 'Choose another folder…'}
          </button>
          {supportsDirectoryPicker() ? (
            <Pill tone="info" title="This browser can re-read the folder for a one-click re-sync.">
              re-syncable
            </Pill>
          ) : null}
          {phase === 'reading' ? <Spinner label="Reading the folder…" /> : null}
        </div>
        <p className="m-0 mt-1.5 text-[11px] text-muted">{SNAPSHOT_NOTE}</p>
        <input
          ref={inputRef}
          type="file"
          multiple
          className="hidden"
          aria-label="Trades folder on this device"
          // `webkitdirectory` turns the file input into a folder input. It is
          // not in React's typed attribute list, hence the spread.
          {...({ webkitdirectory: '', directory: '' } as Record<string, string>)}
          onChange={(event) => {
            const files = event.target.files
            if (files === null || files.length === 0) return
            const picked = entriesFromFileList(files)
            accept(picked[0]?.path.split('/')[0] ?? '', picked)
            // Let the same folder be re-picked and re-read next time.
            event.target.value = ''
          }}
        />
      </div>

      <div className="min-h-[10rem] flex-1 overflow-y-auto px-3 py-2">
        {selection === null ? (
          <p className="m-0 py-6 text-center text-muted">
            Pick the folder holding <span className="num">Open Trades</span>,{' '}
            <span className="num">Closed Trades</span> and{' '}
            <span className="num">Daily Trade Statement</span> — they may also sit one level down.
            Only the files inside those folders are uploaded.
          </p>
        ) : (
          <div className="space-y-2">
            <dl
              className="m-0 grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-[12px]"
              // A description list has no implicit role to hang the label on.
              role="group"
              aria-label="Upload summary"
            >
              <dt className="ws-label">Folder</dt>
              <dd className="num m-0 break-all text-fg-strong">{folder === '' ? '—' : folder}</dd>
              <dt className="ws-label">Files</dt>
              <dd className="num m-0 text-fg">
                {selection.tradeFiles} trade file{selection.tradeFiles === 1 ? '' : 's'} /{' '}
                {selection.statements} statement{selection.statements === 1 ? '' : 's'}
              </dd>
              <dt className="ws-label">Skipped</dt>
              <dd className="num m-0 text-muted">
                {selection.skipped.length}
                {selection.skipped.length > 0 ? ` (${skipSummary(selection)})` : ''}
              </dd>
              <dt className="ws-label">Size</dt>
              <dd className="num m-0 text-fg">{fmtBytes(selection.bytes)}</dd>
            </dl>
            {selection.kept.length === 0 ? (
              <p className="m-0 text-[12px] text-warn">
                Nothing in that folder belongs to the trades layout, so there is nothing to upload.
              </p>
            ) : (
              <ul className="m-0 max-h-40 list-none overflow-y-auto p-0">
                {selection.kept.slice(0, 12).map((file) => (
                  <li
                    key={file.relativePath}
                    className="num flex justify-between gap-3 border-b border-line/50 py-0.5 text-[11px] text-muted"
                  >
                    <span className="min-w-0 truncate">{file.relativePath}</span>
                    <span className="shrink-0 text-faint">{fmtBytes(file.bytes)}</span>
                  </li>
                ))}
                {selection.kept.length > 12 ? (
                  <li className="py-0.5 text-[11px] text-faint">
                    …and {selection.kept.length - 12} more
                  </li>
                ) : null}
              </ul>
            )}
          </div>
        )}
      </div>

      <footer className="shrink-0 border-t border-line px-3 py-2">
        {upload.isSuccess ? (
          <p role="status" className="m-0 mb-1.5 text-[12px] text-pos">
            ✓ {upload.data.open_trades} open / {upload.data.closed_trades} closed trades loaded
            from this device
          </p>
        ) : null}
        {readError !== null ? (
          <p role="alert" className="m-0 mb-1.5 text-[12px] text-neg">
            Could not read that folder: {readError}
          </p>
        ) : null}
        {cap !== null ? (
          <p role="alert" className="m-0 mb-1.5 text-[12px] text-neg">
            Too large for one upload. {cap}
          </p>
        ) : null}
        {upload.error ? (
          <p role="alert" className="m-0 mb-1.5 text-[12px] text-neg">
            <strong className="font-semibold">{failureTitle(upload.error)}</strong>{' '}
            {failureDetail(upload.error)}
          </p>
        ) : null}
        <div className="flex flex-wrap items-center justify-end gap-2">
          {upload.isPending ? (
            <span className="mr-auto text-[11px] text-muted" role="status">
              Uploading {selection?.kept.length ?? 0} files ({fmtBytes(selection?.bytes ?? 0)})…
            </span>
          ) : null}
          <button
            type="button"
            className="ws-btn"
            disabled={selection === null || selection.kept.length === 0 || cap !== null || busy}
            onClick={send}
          >
            {upload.isPending ? 'Uploading…' : 'Upload & analyze'}
          </button>
        </div>
      </footer>
    </div>
  )
}
