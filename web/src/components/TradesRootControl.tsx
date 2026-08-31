/**
 * Status-bar control naming the folder the trade files come from, and the
 * doorway to the two ways of changing it (the server-side picker and the
 * device upload).
 *
 * The button shows only the folder's basename — the full path and *how* that
 * path was decided (config, chosen here, the demo fixtures, or uploaded from
 * the device showing this page) live in the tooltip, which is mirrored into
 * `title` so it survives touch and screen readers.
 *
 * An uploaded root is a snapshot, so it also gets a Re-sync: where the browser
 * kept a handle on the picked folder that is a one-click re-read and re-upload,
 * and everywhere else it reopens the dialog on the device tab to re-pick.
 */

import { useId, useState } from 'react'
import { ApiError } from '../api/client'
import { useTradesRoot, useUploadTradesRoot } from '../api/queries'
import type { TradesRootSource } from '../api/types'
import { rereadRememberedFolder, supportsDirectoryPicker } from '../lib/deviceFolder'
import { buildSelection, buildUploadForm } from '../lib/deviceUpload'
import { fmtAge } from '../lib/format'
import { useNow } from '../lib/useNow'
import { TradesRootDialog, type TradesRootTab } from './TradesRootDialog'

const SOURCE_LABEL: Record<TradesRootSource, string> = {
  config: 'from config',
  override: 'chosen in app',
  demo: 'demo fixtures',
  uploaded: 'uploaded from this device',
}

function basename(path: string): string {
  const trimmed = path.replace(/\/+$/, '')
  const cut = trimmed.lastIndexOf('/')
  return cut < 0 ? trimmed : (trimmed.slice(cut + 1) || '/')
}

export function TradesRootControl() {
  const root = useTradesRoot()
  const [open, setOpen] = useState(false)
  const [tab, setTab] = useState<TradesRootTab>('server')
  const [tip, setTip] = useState(false)
  const [resyncError, setResyncError] = useState<string | null>(null)
  const tipId = useId()
  const now = useNow(30_000)
  const upload = useUploadTradesRoot()

  const data = root.data
  const uploaded = data?.source === 'uploaded'
  // `<state_dir>/uploads/current` is the server's business, not a place the
  // user recognises; for an uploaded root the honest name is where it came
  // from.
  const label = uploaded ? 'this device' : data ? basename(data.root) : root.isError ? 'unknown' : '…'
  // `uploaded_at` is absent for every other source, and the contract lets it
  // be missing even here — no instant, no age, rather than an em dash. The
  // clamp covers the ordinary case of an upload that just happened (and a
  // server clock a shade ahead of this one), which is otherwise negative.
  const uploadedMs = uploaded && data.uploaded_at ? Date.parse(data.uploaded_at) : Number.NaN
  const age = Number.isFinite(uploadedMs)
    ? fmtAge(Math.max(0, (now.getTime() - uploadedMs) / 1000))
    : null

  const showDialog = (which: TradesRootTab): void => {
    setTab(which)
    setOpen(true)
  }

  /**
   * One click when the folder is still remembered; otherwise the dialog opens
   * on the device tab so the user can point at it again — which is every
   * re-sync in a browser without the File System Access API, and any re-sync
   * after a reload.
   */
  const resync = (): void => {
    setResyncError(null)
    upload.reset()
    void rereadRememberedFolder()
      .then((pick) => {
        if (pick === null) {
          showDialog('device')
          return
        }
        const selection = buildSelection(pick.entries)
        if (selection.kept.length === 0) {
          setResyncError('that folder no longer holds any trade files')
          showDialog('device')
          return
        }
        return upload.mutateAsync(buildUploadForm(selection)).then(() => undefined)
      })
      .catch((error: unknown) => {
        setResyncError(
          error instanceof ApiError
            ? (error.problem?.detail ?? error.message)
            : error instanceof Error
              ? error.message
              : String(error),
        )
      })
  }

  const summary = data
    ? `${uploaded ? 'Uploaded from this device' : data.root} — ${SOURCE_LABEL[data.source]}${
        age === null ? '' : `, ${age} ago`
      }. ${data.open_trades} open / ${data.closed_trades} closed trades. Click to pick another folder.`
    : root.isError
      ? `Could not read the trades root: ${
          root.error instanceof ApiError
            ? (root.error.problem?.detail ?? root.error.message)
            : String(root.error)
        }`
      : 'Asking the server where the trade files are…'

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
        onClick={() => showDialog('server')}
        onMouseEnter={() => setTip(true)}
        onMouseLeave={() => setTip(false)}
        onFocus={() => setTip(true)}
        onBlur={() => setTip(false)}
      >
        <span className="ws-label">trades</span>
        <span className="num max-w-[12rem] truncate text-fg-strong">{label}</span>
        {uploaded && age !== null ? <span className="num text-faint">({age})</span> : null}
      </button>
      {uploaded ? (
        <button
          type="button"
          className="ws-btn"
          disabled={upload.isPending}
          onClick={resync}
          title={
            supportsDirectoryPicker()
              ? 'Re-read the folder on this device and upload it again'
              : 'Pick the folder on this device again and upload it — this browser cannot remember it'
          }
        >
          {upload.isPending ? '↻ re-syncing…' : '↻ re-sync'}
        </button>
      ) : null}
      {resyncError !== null ? (
        <span role="alert" className="max-w-[14rem] truncate text-[11px] text-neg">
          Re-sync failed: {resyncError}
        </span>
      ) : null}
      {tip ? (
        <span
          id={tipId}
          role="tooltip"
          className="pointer-events-none absolute top-full left-0 z-50 mt-1 w-max max-w-[26rem] rounded border border-line bg-surface-3 px-2 py-1.5 text-[11px] leading-snug font-normal text-fg shadow-lg"
          style={{ boxShadow: 'var(--ws-shadow)' }}
        >
          {data ? (
            <span className="block space-y-0.5">
              <span className="num block break-all text-fg-strong">
                {uploaded ? 'Uploaded from this device' : data.root}
              </span>
              <span className="block text-muted">
                {/* The first line already says where it came from. */}
                {uploaded ? (age === null ? 'uploaded' : `uploaded ${age} ago`) : SOURCE_LABEL[data.source]}{' '}
                · {data.open_trades} open / {data.closed_trades} closed
              </span>
              <span className="block text-muted">
                {uploaded
                  ? 'A snapshot: edits on this device arrive on the next re-sync. Reset in the dialog goes back to the configured folder.'
                  : 'Click to pick another folder.'}
              </span>
            </span>
          ) : (
            summary
          )}
        </span>
      ) : null}
      {open ? (
        <TradesRootDialog
          root={data}
          initialTab={tab}
          onClose={() => {
            setOpen(false)
            setTab('server')
          }}
        />
      ) : null}
    </span>
  )
}
