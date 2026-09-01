/**
 * Status-bar button that empties the analysis set in one stroke, behind a
 * confirmation dialog: the selected trades are deselected, and — when the book
 * is a snapshot uploaded from the viewing device — the server's stored copy is
 * removed too (`DELETE /trades/root`), returning to the configured folder.
 *
 * Only an uploaded snapshot is cleared from the server. A root picked on the
 * server or set by config is left alone: clearing the analysis must not change
 * which folder the server watches.
 */

import { useEffect, useId, useRef, useState } from 'react'
import { ApiError } from '../api/client'
import { useResetTradesRoot, useTradesRoot } from '../api/queries'
import { useAppStore } from '../store/appStore'
import { CONFIRM_MS } from './TradesRootDialog'
import { Modal } from './ui'

function problemDetail(error: unknown): string {
  if (error instanceof ApiError) return error.problem?.detail ?? error.message
  return error instanceof Error ? error.message : String(error)
}

export function ClearTradesControl() {
  const root = useTradesRoot()
  const resetRoot = useResetTradesRoot()
  const selected = useAppStore((s) => s.selected)
  const clearSelection = useAppStore((s) => s.clearSelection)
  const [open, setOpen] = useState(false)
  const [confirmation, setConfirmation] = useState<string | null>(null)
  const titleId = useId()
  const descId = useId()

  const timer = useRef<ReturnType<typeof setTimeout> | null>(null)
  useEffect(() => () => {
    if (timer.current !== null) clearTimeout(timer.current)
  }, [])

  const uploaded = root.data?.source === 'uploaded'
  const nothingToClear = selected.length === 0 && !uploaded
  const busy = resetRoot.isPending || confirmation !== null

  const close = (): void => {
    if (timer.current !== null) clearTimeout(timer.current)
    setOpen(false)
    setConfirmation(null)
    resetRoot.reset()
  }

  const succeed = (message: string): void => {
    setConfirmation(message)
    timer.current = setTimeout(close, CONFIRM_MS)
  }

  const clear = (): void => {
    if (busy) return
    clearSelection()
    if (!uploaded) {
      succeed('analysis set cleared')
      return
    }
    resetRoot.mutate(undefined, {
      onSuccess: (data) =>
        succeed(`upload removed — back to ${data.open_trades} open / ${data.closed_trades} closed trades from ${data.root}`),
    })
  }

  return (
    <>
      <button
        type="button"
        className="ws-btn"
        aria-haspopup="dialog"
        aria-expanded={open}
        disabled={nothingToClear}
        title={
          nothingToClear
            ? 'No trades are being analyzed and nothing was uploaded'
            : uploaded
              ? 'Deselect every analyzed trade and remove the uploaded snapshot from the server'
              : 'Deselect every analyzed trade'
        }
        onClick={() => setOpen(true)}
      >
        <span className="ws-label">clear trades</span>
      </button>
      {open ? (
        <Modal labelledBy={titleId} describedBy={descId} onClose={close} className="max-w-[26rem]">
          <div className="px-3 py-2.5">
            <h2 id={titleId} className="m-0 text-[13px] font-semibold text-fg-strong">
              Clear trades
            </h2>
            <p id={descId} className="m-0 mt-1 text-[12px] text-fg">
              Are you sure you want to clear all current trades?
            </p>
            <p className="m-0 mt-1 text-[11px] text-muted">
              {selected.length > 0
                ? `The ${selected.length} trade${selected.length === 1 ? '' : 's'} in the analysis set will be deselected`
                : 'No trades are selected for analysis'}
              {uploaded
                ? ', and the snapshot uploaded from this device will be removed from the server.'
                : '. The trades folder on the server is not touched.'}
            </p>
            {confirmation !== null ? (
              <p role="status" className="m-0 mt-1.5 text-[12px] text-pos">
                ✓ {confirmation}
              </p>
            ) : null}
            {resetRoot.error !== null ? (
              <p role="alert" className="m-0 mt-1.5 text-[12px] text-neg">
                Could not remove the upload: {problemDetail(resetRoot.error)}
              </p>
            ) : null}
            <div className="mt-2.5 flex justify-end gap-2">
              <button type="button" className="ws-btn" onClick={close} disabled={confirmation !== null}>
                Cancel
              </button>
              <button type="button" className="ws-btn" data-autofocus disabled={busy} onClick={clear}>
                {resetRoot.isPending ? 'Clearing…' : 'Clear trades'}
              </button>
            </div>
          </div>
        </Modal>
      ) : null}
    </>
  )
}
