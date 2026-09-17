/**
 * Cross-component "do a thing" signals, for callers with no other channel to
 * the component that owns the UI — the native menu bridge is the only one
 * today. Not app state: nothing here is persisted or read by a query.
 *
 * `TradesRootControl` and `ClearTradesControl` subscribe (via the hook's own
 * `.subscribe`, so an unrelated signal triggers no re-render) and open their
 * dialog when `last.signal` matches; `menu.ts` is the only emitter.
 *
 * `'open-trades-dialog'` is the retired folder dialog's signal — kept only
 * until step 9 of the trade-source migration deletes `TradesRootControl`.
 * `'open-source-dialog'` is what `TradeSourceControl` and the menu's
 * `Trades → Import trades…` item use now.
 */

import { create } from 'zustand'

export type UiSignal = 'open-trades-dialog' | 'open-source-dialog' | 'open-clear-dialog'

interface UiSignalsState {
  last: { signal: UiSignal; at: number } | null
  emit: (signal: UiSignal) => void
}

export const useUiSignals = create<UiSignalsState>((set) => ({
  last: null,
  emit: (signal) => set({ last: { signal, at: Date.now() } }),
}))
