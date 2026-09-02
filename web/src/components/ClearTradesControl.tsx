/**
 * Status-bar button that empties the analysis set in one stroke, behind a
 * confirmation dialog: the selected trades are deselected, and — when the book
 * was uploaded from this device — every byte of it is deleted from the server
 * (`DELETE /trades/root`).
 *
 * That deletion is the point of the button, not a side effect. Uploaded trade
 * files live on the server only for as long as they are being analysed: the
 * clear removes the whole copy, the record pointing at it and the cached
 * market snapshot derived from it, and the book falls back to whatever the
 * deployer configured — or to nothing at all.
 *
 * A book the *deployer* configured (`[trades] root`, or `--demo`) is not ours
 * to delete, so with one of those the clear is only the analysis set.
 */

import { useEffect, useId, useRef, useState } from 'react'
import { ApiError } from '../api/client'
import { useClearTrades, useTradesRoot } from '../api/queries'
import { useAppStore } from '../store/appStore'
import { CONFIRM_MS } from './TradesUploadDialog'
import { Modal } from './ui'

function problemDetail(error: unknown): string {
  if (error instanceof ApiError) return error.problem?.detail ?? error.message
  return error instanceof Error ? error.message : String(error)
}

export function ClearTradesControl() {
  const root = useTradesRoot()
  const clearTrades = useClearTrades()
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
  const busy = clearTrades.isPending || confirmation !== null

  const close = (): void => {
    if (timer.current !== null) clearTimeout(timer.current)
    setOpen(false)
    setConfirmation(null)
    clearTrades.reset()
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
    clearTrades.mutate(undefined, {
      onSuccess: (data) =>
        succeed(
          data.source === 'empty'
            ? 'upload deleted from the server — no trades loaded'
            : `upload deleted from the server — back to ${data.open_trades} open / ${data.closed_trades} closed trades from ${data.root ?? 'the configured folder'}`,
        ),
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
              ? 'Deselect every analyzed trade and delete the uploaded trade files from the server'
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
                ? ', and the trade files uploaded from this device will be deleted from the server.'
                : '. Nothing was uploaded, so there is nothing to delete from the server.'}
            </p>
            {confirmation !== null ? (
              <p role="status" className="m-0 mt-1.5 text-[12px] text-pos">
                ✓ {confirmation}
              </p>
            ) : null}
            {clearTrades.error !== null ? (
              <p role="alert" className="m-0 mt-1.5 text-[12px] text-neg">
                Could not delete the upload: {problemDetail(clearTrades.error)}
              </p>
            ) : null}
            <div className="mt-2.5 flex justify-end gap-2">
              <button type="button" className="ws-btn" onClick={close} disabled={confirmation !== null}>
                Cancel
              </button>
              <button type="button" className="ws-btn" data-autofocus disabled={busy} onClick={clear}>
                {clearTrades.isPending ? 'Clearing…' : 'Clear trades'}
              </button>
            </div>
          </div>
        </Modal>
      ) : null}
    </>
  )
}
