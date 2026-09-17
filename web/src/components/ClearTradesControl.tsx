/**
 * Status-bar button that clears the trades in one stroke, behind a
 * confirmation dialog. Per D23, "clear" does both halves of "remove them from
 * displaying in the app": it deselects the analysis set *and* unloads the
 * book (`DELETE /trades/source`) — the imported sheet or CSV is forgotten and
 * the dashboard falls back to whatever the deployer configured, or to
 * nothing.
 *
 * It never deletes anything. The spreadsheet and the CSV file are left
 * exactly as they are — Clear only stops Wingspan from showing them.
 *
 * A book the *deployer* configured (`[trades] sheet_url`/`csv_path`, or
 * `--demo`) is not something we ever imported, so with one of those the
 * clear is only the analysis set.
 */

import { useEffect, useId, useRef, useState } from 'react'
import { ApiError } from '../api/client'
import { useClearTradeSource, useTradeSource } from '../api/queries'
import { useUiSignals } from '../lib/uiSignals'
import { useAppStore } from '../store/appStore'
import { CONFIRM_MS } from './TradeSourceDialog'
import { Modal } from './ui'

function problemDetail(error: unknown): string {
  if (error instanceof ApiError) return error.problem?.detail ?? error.message
  return error instanceof Error ? error.message : String(error)
}

export function ClearTradesControl() {
  const source = useTradeSource()
  const clearTrades = useClearTradeSource()
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

  // The native menu's "Clear trades…" has no component of its own to open
  // this confirmation on, so it emits a signal instead of a prop.
  useEffect(() => useUiSignals.subscribe((state) => {
    if (state.last?.signal === 'open-clear-dialog') setOpen(true)
  }), [])

  const loaded = source.data?.kind === 'sheet' || source.data?.kind === 'csv'
  const nothingToClear = selected.length === 0 && !loaded
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
    if (!loaded) {
      succeed('analysis set cleared')
      return
    }
    clearTrades.mutate(undefined, {
      onSuccess: (data) =>
        succeed(
          `trades cleared — ${
            data.kind === 'empty'
              ? 'no trades loaded'
              : `back to ${data.open_trades} open / ${data.closed_trades} closed trades from ${data.label ?? 'the configured source'}`
          }`,
        ),
    })
  }

  const what = source.data?.label ?? 'the imported source'

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
            ? 'No trades are being analyzed and no source is imported'
            : loaded
              ? 'Deselect every analyzed trade and stop showing the trades from this source'
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
              {loaded
                ? `, and Wingspan will stop showing the trades from ${what}. Nothing on disk or in your spreadsheet is touched.`
                : '. No source is imported, so there is nothing to clear.'}
            </p>
            {confirmation !== null ? (
              <p role="status" className="m-0 mt-1.5 text-[12px] text-pos">
                ✓ {confirmation}
              </p>
            ) : null}
            {clearTrades.error !== null ? (
              <p role="alert" className="m-0 mt-1.5 text-[12px] text-neg">
                Could not clear the trades: {problemDetail(clearTrades.error)}
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
