/**
 * What the OS folder chooser last answered, parked outside React's tree.
 *
 * The native chooser is not modal to the Wingspan window and can be up for as
 * long as the user takes to find a folder. Anything that unmounts
 * `TradesFolderDialog` in the meantime — Escape, a click on the modal
 * backdrop, the ✕ — also unsubscribes the `usePickFolder` observer, and
 * TanStack Query runs a per-call `mutate(vars, { onSuccess })` callback only
 * while that observer still has listeners. The pick would then complete in
 * Rust, return the path, and land nowhere: no folder, no error, nothing in
 * the log. That is the reported "I chose a folder and nothing came back".
 *
 * So the answer is parked here, by `usePickFolder`'s *mutation-level*
 * callbacks — those the mutation itself runs, whether or not anything is
 * listening. `TradesFolderDialog` claims the outcome and probes it (or shows
 * the failure); `TradesRootControl` reopens the dialog if the pick outlived
 * it, so a chosen folder is acted on even when the dialog is long gone.
 *
 * Not app state: nothing here is persisted, and an outcome is claimed once.
 */

import { create } from 'zustand'

/** A folder to probe, or a chooser that failed with something to say. */
export type PickOutcome = { kind: 'folder'; path: string } | { kind: 'error'; detail: string }

interface PickedFolderState {
  /** `at` only exists to make two identical picks in a row distinguishable. */
  pending: { outcome: PickOutcome; at: number } | null
  receive: (outcome: PickOutcome) => void
  /** Take the outcome, leaving nothing behind. `null` once it is claimed. */
  claim: () => PickOutcome | null
}

export const usePickedFolder = create<PickedFolderState>((set, get) => ({
  pending: null,
  receive: (outcome) => set({ pending: { outcome, at: Date.now() } }),
  claim: () => {
    const pending = get().pending
    if (pending === null) return null
    set({ pending: null })
    return pending.outcome
  },
}))

/** Tests and the store's own owner: forget an outcome nobody claimed. */
export function resetPickedFolder(): void {
  usePickedFolder.setState({ pending: null })
}
