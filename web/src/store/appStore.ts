/**
 * UI state: which trades are selected, how the table is sorted, every
 * risk-graph control, and the modeled legs. Persisted to `localStorage` under
 * `wingspan.ui` (the key `index.html` reads before first paint to avoid a
 * theme flash).
 *
 * Server data never lives here — that is TanStack Query's job. This store only
 * holds what the trader chose. Modeled legs count as a choice: they are UI
 * scratch state, never part of the book, and travel to the server only as the
 * additive `model_legs` field of the read requests.
 */

import { create } from 'zustand'
import { persist } from 'zustand/middleware'
import type { OptionContract } from '../api/types'
import type { SortDir, SortKey } from '../lib/aggregate'
import { isLiveModelLeg } from '../lib/model'

export type Theme = 'dark' | 'light'
export type Tab = 'positions' | 'model' | 'closed' | 'health'
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
  /** Horizontal zoom over the computed price range; 1 = the whole range. */
  xZoom: number
  /** Price the zoom window is centred on; null = centre on spot. */
  xCenter: number | null
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
  xZoom: 1,
  xCenter: null,
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

/** One modeled leg: the frozen leg contract and a non-zero integer quantity. */
export interface ModelLeg {
  contract: OptionContract
  qty: number
}

/** Strikes shown around ATM per chain section; `null` = every listed strike. */
export type StrikeCount = 16 | 32 | 64 | null

export interface ModelSettings {
  /** Keyed by `contract.tos_code` (one entry per contract); qty never 0. */
  legs: ModelLeg[]
  /** Draw the model in the risk graph. */
  include: boolean
  /** Also draw the position-only T+0 and expiry lines, dashed. */
  compare: boolean
  /** Chain sections shown, as `${expiry}|${root}`, in the order they were
   *  opened (the oldest is the one a seventh replaces); `[]` = automatic. */
  expiries: string[]
  strikeCount: StrikeCount
}

export const DEFAULT_MODEL: ModelSettings = {
  legs: [],
  include: true,
  compare: false,
  expiries: [],
  strikeCount: 32,
}

/** The server refuses a modeled |qty| above this (422 `out-of-range`). */
export const MAX_MODEL_QTY = 999
/** … and more legs than this. The store never builds a request it would refuse. */
export const MAX_MODEL_LEGS = 64
/** Chain sections stacked at once; a seventh replaces the oldest. */
export const MAX_MODEL_SECTIONS = 6

const STRIKE_COUNTS: readonly StrikeCount[] = [16, 32, 64, null]

/** Round to an integer and clamp to ±`MAX_MODEL_QTY`; anything unreadable is 0. */
export function normalizeModelQty(qty: number): number {
  if (!Number.isFinite(qty)) return 0
  const rounded = Math.round(qty)
  // `Math.round(-0.4)` is `-0`, which would print as "-0" and compare unequal
  // to 0 under Object.is — collapse it.
  if (rounded === 0) return 0
  return Math.max(-MAX_MODEL_QTY, Math.min(MAX_MODEL_QTY, rounded))
}

/**
 * Re-validates a persisted model slice: an old or hand-edited blob must not
 * put a zero, fractional or duplicated leg into a request the server would
 * refuse with a 422 on every graph redraw.
 */
function sanitizeModel(saved: Partial<ModelSettings> | undefined): ModelSettings {
  const merged = { ...DEFAULT_MODEL, ...(saved ?? {}) }
  const seen = new Set<string>()
  const legs: ModelLeg[] = []
  for (const leg of Array.isArray(merged.legs) ? merged.legs : []) {
    const code = leg?.contract?.tos_code
    if (typeof code !== 'string' || seen.has(code)) continue
    const qty = normalizeModelQty(Number(leg.qty))
    if (qty === 0) continue
    seen.add(code)
    legs.push({ contract: leg.contract, qty })
    if (legs.length === MAX_MODEL_LEGS) break
  }
  return {
    legs,
    include: merged.include !== false,
    compare: merged.compare === true,
    expiries: Array.isArray(merged.expiries)
      ? merged.expiries.filter((key): key is string => typeof key === 'string').slice(-MAX_MODEL_SECTIONS)
      : [],
    strikeCount: STRIKE_COUNTS.includes(merged.strikeCount) ? merged.strikeCount : DEFAULT_MODEL.strikeCount,
  }
}

/** The open-positions table can never be dragged shorter than this (px):
 *  its header row plus one trade stay readable. */
export const POSITIONS_MIN_HEIGHT = 96

export interface AppState {
  theme: Theme
  tab: Tab
  selected: string[]
  sortKey: SortKey
  sortDir: SortDir
  expanded: string[]
  graph: GraphSettings
  /** The open-positions card folded down to its header. */
  positionsCollapsed: boolean
  /** Max height (px) the trader dragged the open-positions card to; null =
   *  the default cap. */
  positionsHeight: number | null
  /** The risk graph alone on the positions tab. Deliberately not persisted:
   *  a relaunch should show the whole dashboard again. */
  graphMaximized: boolean
  /** The warnings card on Data health folded down to its header. */
  warningsCollapsed: boolean
  /** Fingerprints (`warningKey`) of warnings the trader cleared. Kept until
   *  restored, so a reload of the same files does not bring them back. */
  dismissedWarnings: string[]
  /** The Model tab: modeled legs and how the chain is laid out. */
  model: ModelSettings

  setTheme: (theme: Theme) => void
  toggleTheme: () => void
  setTab: (tab: Tab) => void

  setSelected: (ids: string[]) => void
  toggleSelected: (id: string) => void
  clearSelection: () => void
  /** Drop selected ids the server no longer knows about (root changed). */
  pruneSelection: (liveIds: readonly string[]) => void
  /** Same, for the expanded rows — pass open *and* closed ids. */
  pruneExpanded: (liveIds: readonly string[]) => void

  setSort: (key: SortKey) => void
  toggleExpanded: (id: string) => void

  patchGraph: (patch: Partial<GraphSettings>) => void
  setExpiryVolShift: (expiry: string, shift: number) => void
  clearExpiryVolShifts: () => void
  toggleLine: (key: string) => void
  patchWhatIf: (patch: Partial<WhatIfSettings>) => void
  resetGraph: () => void

  setPositionsCollapsed: (collapsed: boolean) => void
  setPositionsHeight: (height: number | null) => void
  setGraphMaximized: (maximized: boolean) => void
  setWarningsCollapsed: (collapsed: boolean) => void
  dismissWarnings: (keys: readonly string[]) => void
  restoreWarnings: () => void

  /** 0 removes the leg; non-integers are rounded and |qty| clamped to 999. A
   *  new contract past the 64-leg limit is ignored. */
  setModelQty: (contract: OptionContract, qty: number) => void
  removeModelLeg: (tosCode: string) => void
  /** Drops every leg that expired before `valuationDate` (the server's date,
   *  `valuationDateOf(snapshot.as_of)`): a persisted 0DTE leg must not make
   *  every graph request a 422 the next day. A no-op when nothing expired. */
  pruneExpiredModel: (valuationDate: string) => void
  /** Empties the legs only — the chain layout and the toggles stay. */
  clearModel: () => void
  /** Show or hide one chain section; a seventh replaces the oldest. */
  toggleModelExpiry: (key: string) => void
  setModelExpiries: (keys: string[]) => void
  setModelStrikeCount: (count: StrikeCount) => void
  setModelInclude: (include: boolean) => void
  setModelCompare: (compare: boolean) => void
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
      positionsCollapsed: false,
      positionsHeight: null,
      graphMaximized: false,
      warningsCollapsed: false,
      dismissedWarnings: [],
      model: DEFAULT_MODEL,

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
      pruneSelection: (liveIds) =>
        set((s) => {
          const live = new Set(liveIds)
          const next = s.selected.filter((id) => live.has(id))
          return next.length === s.selected.length ? {} : { selected: next }
        }),
      pruneExpanded: (liveIds) =>
        set((s) => {
          const live = new Set(liveIds)
          const next = s.expanded.filter((id) => live.has(id))
          return next.length === s.expanded.length ? {} : { expanded: next }
        }),

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

      setPositionsCollapsed: (positionsCollapsed) => set({ positionsCollapsed }),
      setPositionsHeight: (height) =>
        set({
          positionsHeight:
            height === null || !Number.isFinite(height)
              ? null
              : Math.max(POSITIONS_MIN_HEIGHT, Math.round(height)),
        }),
      setGraphMaximized: (graphMaximized) => set({ graphMaximized }),
      setWarningsCollapsed: (warningsCollapsed) => set({ warningsCollapsed }),
      dismissWarnings: (keys) =>
        set((s) => ({ dismissedWarnings: [...new Set([...s.dismissedWarnings, ...keys])] })),
      restoreWarnings: () => set({ dismissedWarnings: [] }),

      setModelQty: (contract, qty) =>
        set((s) => {
          const next = normalizeModelQty(qty)
          const code = contract.tos_code
          const index = s.model.legs.findIndex((leg) => leg.contract.tos_code === code)
          if (index === -1) {
            if (next === 0 || s.model.legs.length >= MAX_MODEL_LEGS) return {}
            return { model: { ...s.model, legs: [...s.model.legs, { contract, qty: next }] } }
          }
          if (s.model.legs[index]?.qty === next) return {}
          const legs =
            next === 0
              ? s.model.legs.filter((_, i) => i !== index)
              : // Edited in place, so the ticket keeps the order legs were added in.
                s.model.legs.map((leg, i) => (i === index ? { contract, qty: next } : leg))
          return { model: { ...s.model, legs } }
        }),
      removeModelLeg: (tosCode) =>
        set((s) => {
          const legs = s.model.legs.filter((leg) => leg.contract.tos_code !== tosCode)
          return legs.length === s.model.legs.length ? {} : { model: { ...s.model, legs } }
        }),
      pruneExpiredModel: (valuationDate) =>
        set((s) => {
          const legs = s.model.legs.filter((leg) => isLiveModelLeg(leg, valuationDate))
          return legs.length === s.model.legs.length ? {} : { model: { ...s.model, legs } }
        }),
      clearModel: () =>
        set((s) => (s.model.legs.length === 0 ? {} : { model: { ...s.model, legs: [] } })),
      toggleModelExpiry: (key) =>
        set((s) => {
          const current = s.model.expiries
          const expiries = current.includes(key)
            ? current.filter((k) => k !== key)
            : [...current, key].slice(-MAX_MODEL_SECTIONS)
          return { model: { ...s.model, expiries } }
        }),
      setModelExpiries: (keys) =>
        set((s) => ({
          model: { ...s.model, expiries: [...new Set(keys)].slice(-MAX_MODEL_SECTIONS) },
        })),
      setModelStrikeCount: (strikeCount) => set((s) => ({ model: { ...s.model, strikeCount } })),
      setModelInclude: (include) => set((s) => ({ model: { ...s.model, include } })),
      setModelCompare: (compare) => set((s) => ({ model: { ...s.model, compare } })),
    }),
    {
      name: STORE_KEY,
      // 3 added `model`. Every earlier blob is a strict subset of this shape,
      // so `migrate` passes it through and `merge` backfills the defaults —
      // without a `migrate`, zustand would discard a version-2 blob outright,
      // and with it the trader's selection, theme and graph settings.
      version: 3,
      migrate: (persisted) => persisted as AppState,
      partialize: (state) => ({
        theme: state.theme,
        tab: state.tab,
        selected: state.selected,
        sortKey: state.sortKey,
        sortDir: state.sortDir,
        expanded: state.expanded,
        graph: state.graph,
        positionsCollapsed: state.positionsCollapsed,
        positionsHeight: state.positionsHeight,
        warningsCollapsed: state.warningsCollapsed,
        dismissedWarnings: state.dismissedWarnings,
        model: state.model,
      }),
      merge: (persisted, current) => {
        const saved = (persisted ?? {}) as Partial<AppState>
        return {
          ...current,
          ...saved,
          // A settings shape added after the user's last visit must not be
          // missing when an old blob is rehydrated.
          graph: { ...DEFAULT_GRAPH, ...(saved.graph ?? {}) },
          model: sanitizeModel(saved.model),
        }
      },
    },
  ),
)

/** Applies the theme to `<html data-theme>`; called once from `App`. */
export function applyTheme(theme: Theme): void {
  if (typeof document !== 'undefined') document.documentElement.dataset.theme = theme
}
