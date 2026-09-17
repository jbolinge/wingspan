/**
 * What the OS file chooser last answered, parked outside React's tree.
 *
 * The native chooser is not modal to the Wingspan window and can be up for as
 * long as the user takes to find a file. Anything that unmounts
 * `TradeSourceDialog` in the meantime — Escape, a click on the modal
 * backdrop, the ✕ — also unsubscribes the `usePickCsvFile` observer, and
 * TanStack Query runs a per-call `mutate(vars, { onSuccess })` callback only
 * while that observer still has listeners. The pick would then complete in
 * Rust, return the path, and land nowhere: no CSV, no error, nothing in the
 * log. That is the CSV sibling of the folder chooser's reported "I chose a
 * folder and nothing came back".
 *
 * So the answer is parked here, by `usePickCsvFile`'s *mutation-level*
 * callbacks — those the mutation itself runs, whether or not anything is
 * listening. `TradeSourceControl` reopens the dialog for a pick that outlived
 * it, and the reopened `TradeSourceDialog` claims the outcome from here.
 *
 * Not app state: nothing here is persisted, and an outcome is claimed once.
 * A copy of `lib/pickedFolder.ts`'s zustand-park pattern for the CSV chooser.
 */

import { create } from 'zustand'

/** A CSV file to probe, or a chooser that failed with something to say. */
export type PickFileOutcome = { kind: 'file'; path: string } | { kind: 'error'; detail: string }

interface PickedFileState {
  /** A fresh object per pick, so two identical picks in a row are two changes. */
  pending: { outcome: PickFileOutcome } | null
  receive: (outcome: PickFileOutcome) => void
  /** Take the outcome, leaving nothing behind. `null` once it is claimed. */
  claim: () => PickFileOutcome | null
}

export const usePickedFile = create<PickedFileState>((set, get) => ({
  pending: null,
  receive: (outcome) => set({ pending: { outcome } }),
  claim: () => {
    const pending = get().pending
    if (pending === null) return null
    set({ pending: null })
    return pending.outcome
  },
}))

/** Tests and the store's own owner: forget an outcome nobody claimed. */
export function resetPickedFile(): void {
  usePickedFile.setState({ pending: null })
}
