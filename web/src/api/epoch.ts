/**
 * A counter of "the server's book or runtime was rebuilt" moments —
 * `app_ready` and `trades_changed` from the event tape.
 *
 * It exists for state the server keeps *outside* the book that a rebuild
 * drops: the extra quote universe `set_model_contracts` registers does not
 * survive a runtime swap, so `useModelUniverseSync` re-sends it whenever this
 * moves. A store of its own (rather than living in `events.ts`) keeps
 * `queries.ts` from importing the module that imports it.
 */

import { create } from 'zustand'

interface BookEpoch {
  epoch: number
  bump: () => void
}

export const useBookEpoch = create<BookEpoch>()((set) => ({
  epoch: 0,
  bump: () => set((s) => ({ epoch: s.epoch + 1 })),
}))
