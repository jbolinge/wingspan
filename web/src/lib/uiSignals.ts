/**
 * Cross-component "do a thing" signals, for callers with no other channel to
 * the component that owns the UI — the native menu bridge is the only one
 * today. Not app state: nothing here is persisted or read by a query.
 *
 * `TradesRootControl` and `ClearTradesControl` subscribe (via the hook's own
 * `.subscribe`, so an unrelated signal triggers no re-render) and open their
 * dialog when `last.signal` matches; `menu.ts` is the only emitter.
 */

import { create } from 'zustand'

export type UiSignal = 'open-trades-dialog' | 'open-clear-dialog'

interface UiSignalsState {
  last: { signal: UiSignal; at: number } | null
  emit: (signal: UiSignal) => void
}

export const useUiSignals = create<UiSignalsState>((set) => ({
  last: null,
  emit: (signal) => set({ last: { signal, at: Date.now() } }),
}))
