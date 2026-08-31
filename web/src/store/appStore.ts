/**
 * UI state: which trades are selected, how the table is sorted, and every
 * risk-graph control. Persisted to `localStorage` under `wingspan.ui` (the key
 * `index.html` reads before first paint to avoid a theme flash).
 *
 * Server data never lives here — that is TanStack Query's job. This store only
 * holds what the trader chose.
 */

import { create } from 'zustand'
import { persist } from 'zustand/middleware'
import type { SortDir, SortKey } from '../lib/aggregate'

export type Theme = 'dark' | 'light'
export type Tab = 'positions' | 'closed' | 'health'
export type RangeMode = 'auto' | 'pct' | 'abs'

export interface WhatIfSettings {
  enabled: boolean
  /** null = follow spot. */
  price: number | null
  /** null = today. */
  date: string | null
  volShift: number
}

export interface GraphSettings {
  rangeMode: RangeMode
  /** Used when `rangeMode === 'pct'`; 0.12 = +/-12% around spot. */
  rangePct: number
  rangeMin: number | null
  rangeMax: number | null
  points: number
  /** Freeze the y-axis so curves stop rescaling as the tape moves. */
  yLock: boolean
  yMin: number | null
  yMax: number | null
  /** Global vol shift in decimal vol (0.01 = 1 vol point). */
  volShift: number
  volShiftByExpiry: Record<string, number>
  /** null = server preset; otherwise `today` | `+45d` | `expiry:YYYY-MM-DD`. */
  dates: string[] | null
  includeRealized: boolean
  /** `RiskLine.key`s the trader has switched off — not dates, which repeat. */
  hiddenLines: string[]
  perTradeOverlay: boolean
  showSigmaBands: boolean
  showBreakevens: boolean
  showSliceTable: boolean
  spotOverride: number | null
  whatIf: WhatIfSettings
}

export const DEFAULT_GRAPH: GraphSettings = {
  rangeMode: 'auto',
  rangePct: 0.12,
  rangeMin: null,
  rangeMax: null,
  points: 241,
  yLock: false,
  yMin: null,
  yMax: null,
  volShift: 0,
  volShiftByExpiry: {},
  dates: null,
  includeRealized: true,
  hiddenLines: [],
  perTradeOverlay: false,
  showSigmaBands: true,
  showBreakevens: true,
  showSliceTable: false,
  spotOverride: null,
  whatIf: { enabled: false, price: null, date: null, volShift: 0 },
}

export interface AppState {
  theme: Theme
  tab: Tab
  selected: string[]
  sortKey: SortKey
  sortDir: SortDir
  expanded: string[]
  graph: GraphSettings

  setTheme: (theme: Theme) => void
  toggleTheme: () => void
  setTab: (tab: Tab) => void

  setSelected: (ids: string[]) => void
  toggleSelected: (id: string) => void
  clearSelection: () => void

  setSort: (key: SortKey) => void
  toggleExpanded: (id: string) => void
  collapseAll: () => void

  patchGraph: (patch: Partial<GraphSettings>) => void
  setExpiryVolShift: (expiry: string, shift: number) => void
  clearExpiryVolShifts: () => void
  toggleLine: (key: string) => void
  patchWhatIf: (patch: Partial<WhatIfSettings>) => void
  resetGraph: () => void
}

/** Numeric columns read better descending on first click. */
const DESC_FIRST: ReadonlySet<SortKey> = new Set<SortKey>([
  'dit',
  'dte',
  'cost',
  'value',
  'pnl_open',
  'pnl_total',
  'pnl_pct',
  'delta',
  'gamma',
  'theta',
  'vega',
  'weighted_vega',
  'staleness_secs',
  'warnings',
])

export const STORE_KEY = 'wingspan.ui'

/** The slice of the store that is persisted — see `partialize` below. */
type PersistedState = Pick<
  AppState,
  'theme' | 'tab' | 'selected' | 'sortKey' | 'sortDir' | 'expanded' | 'graph'
>

export const useAppStore = create<AppState>()(
  persist(
    (set) => ({
      theme: 'dark',
      tab: 'positions',
      selected: [],
      sortKey: 'opened_on',
      sortDir: 'asc',
      expanded: [],
      graph: DEFAULT_GRAPH,

      setTheme: (theme) => set({ theme }),
      toggleTheme: () => set((s) => ({ theme: s.theme === 'dark' ? 'light' : 'dark' })),
      setTab: (tab) => set({ tab }),

      setSelected: (ids) => set({ selected: [...new Set(ids)] }),
      toggleSelected: (id) =>
        set((s) => ({
          selected: s.selected.includes(id)
            ? s.selected.filter((x) => x !== id)
            : [...s.selected, id],
        })),
      clearSelection: () => set({ selected: [] }),

      setSort: (key) =>
        set((s) =>
          s.sortKey === key
            ? { sortDir: s.sortDir === 'asc' ? 'desc' : 'asc' }
            : { sortKey: key, sortDir: DESC_FIRST.has(key) ? 'desc' : 'asc' },
        ),
      toggleExpanded: (id) =>
        set((s) => ({
          expanded: s.expanded.includes(id)
            ? s.expanded.filter((x) => x !== id)
            : [...s.expanded, id],
        })),
      collapseAll: () => set({ expanded: [] }),

      patchGraph: (patch) => set((s) => ({ graph: { ...s.graph, ...patch } })),
      setExpiryVolShift: (expiry, shift) =>
        set((s) => {
          const next = { ...s.graph.volShiftByExpiry }
          if (shift === 0) delete next[expiry]
          else next[expiry] = shift
          return { graph: { ...s.graph, volShiftByExpiry: next } }
        }),
      clearExpiryVolShifts: () => set((s) => ({ graph: { ...s.graph, volShiftByExpiry: {} } })),
      toggleLine: (key) =>
        set((s) => ({
          graph: {
            ...s.graph,
            hiddenLines: s.graph.hiddenLines.includes(key)
              ? s.graph.hiddenLines.filter((x) => x !== key)
              : [...s.graph.hiddenLines, key],
          },
        })),
      patchWhatIf: (patch) =>
        set((s) => ({ graph: { ...s.graph, whatIf: { ...s.graph.whatIf, ...patch } } })),
      resetGraph: () => set({ graph: DEFAULT_GRAPH }),
    }),
    {
      name: STORE_KEY,
      version: 2,
      partialize: (state) => ({
        theme: state.theme,
        tab: state.tab,
        selected: state.selected,
        sortKey: state.sortKey,
        sortDir: state.sortDir,
        expanded: state.expanded,
        graph: state.graph,
      }),
      // v2 keys `hiddenLines` by `RiskLine.key`; a v1 blob holds dates, which
      // would silently stop matching and resurrect curves the trader hid.
      // `merge` backfills whatever this leaves out, so a partial blob is fine.
      migrate: (persisted, version) => {
        const saved = (persisted ?? {}) as Partial<AppState>
        if (version < 2 && saved.graph) {
          return { ...saved, graph: { ...saved.graph, hiddenLines: [] } } as PersistedState
        }
        return saved as PersistedState
      },
      merge: (persisted, current) => {
        const saved = (persisted ?? {}) as Partial<AppState>
        return {
          ...current,
          ...saved,
          // A settings shape added after the user's last visit must not be
          // missing when an old blob is rehydrated.
          graph: { ...DEFAULT_GRAPH, ...(saved.graph ?? {}) },
        }
      },
    },
  ),
)

/** Applies the theme to `<html data-theme>`; called once from `App`. */
export function applyTheme(theme: Theme): void {
  if (typeof document !== 'undefined') document.documentElement.dataset.theme = theme
}
