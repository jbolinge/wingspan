/**
 * Loading a book: the folder on *this* device, uploaded as a snapshot.
 *
 * Trade files are never kept on the server. The browser reads the folder the
 * user picks (`webkitdirectory` / File System Access API) and posts a snapshot
 * (`POST /api/v1/trades/upload`); the analysis runs against that copy until
 * the user clears it, which deletes it (`DELETE /api/v1/trades/root`, the
 * status bar's Clear).
 *
 * There used to be a second tab here that browsed the server's own
 * filesystem and pointed the app at a folder on it. That is gone — with it,
 * the route it used — because trades are not to live on the server at all.
 */

import { useEffect, useId, useRef } from 'react'
import { TradesDeviceTab } from './TradesDeviceTab'
import { Modal } from './ui'

/** How long a confirmation stays up before the dialog closes. */
export const CONFIRM_MS = 900

/** The upload confirmation is longer: it also reports what was sent. */
export const UPLOAD_CONFIRM_MS = 1800

const LAYOUT_HINT =
  'Open Trades / Closed Trades / Daily Trade Statement live here, or one level down.'

export function TradesUploadDialog({ onClose }: { onClose: () => void }) {
  const titleId = useId()
  const descId = useId()

  const timer = useRef<ReturnType<typeof setTimeout> | null>(null)
  useEffect(() => () => {
    if (timer.current !== null) clearTimeout(timer.current)
  }, [])

  // The panel shows its own confirmation (it knows what it sent); the
  // dialog's part is only to get out of the way afterwards.
  const succeedUpload = (): void => {
    timer.current = setTimeout(onClose, UPLOAD_CONFIRM_MS)
  }

  return (
    <Modal labelledBy={titleId} describedBy={descId} onClose={onClose} className="max-w-[46rem]">
      <header className="flex shrink-0 items-center justify-between gap-2 border-b border-line px-3 py-2">
        <div>
          <h2 id={titleId} className="m-0 text-[13px] font-semibold text-fg-strong">
            Load trades from this device
          </h2>
          <p id={descId} className="m-0 text-[11px] text-muted">
            {`Reading a folder on this device and uploading a snapshot of it. ${LAYOUT_HINT}`}
          </p>
        </div>
        <button type="button" className="ws-btn" onClick={onClose} aria-label="Close">
          ✕
        </button>
      </header>

      <div className="flex min-h-0 flex-1 flex-col">
        <TradesDeviceTab autoFocus onUploaded={succeedUpload} />
      </div>
    </Modal>
  )
}
