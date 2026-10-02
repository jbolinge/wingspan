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

/** What the Model tab shows beside its ticket: the chain, or the risk graph. */
export type ModelView = 'chain' | 'graph'

export interface ModelSettings {
  /** Keyed by `contract.tos_code` (one entry per contract); qty never 0. */
  legs: ModelLeg[]
  /** Draw the model in the risk graph. */
  include: boolean
  /** With a model drawn over selected trades, also draw the position
   *  without it — every visible line again, dashed and grey — so the change
   *  the model brings is on the one graph. On by default; the key is new in
   *  0.4.4 because the old `compare` (off by default) is what every
   *  persisted blob before it holds. */
  comparePosition: boolean
  /** The one chain section on screen, as `${expiry}|${root}`: `null` = none
   *  open (a legal, explicit choice), `AUTO_EXPIRY` = the trader has not
   *  chosen, so the automatic pick opens. */
  expiry: string | null
  strikeCount: StrikeCount
  /** The chain to build the model in, or the risk graph to see what it
   *  does — the same panel Positions mounts, so the trader need not leave
   *  the tab to look. */
  view: ModelView
}

/**
 * "Not chosen yet — open the automatic pick". A real section key always
 * contains `|`, so this cannot collide with one.
 */
export const AUTO_EXPIRY = 'auto'

export const DEFAULT_MODEL: ModelSettings = {
  legs: [],
  include: true,
  comparePosition: true,
  expiry: AUTO_EXPIRY,
  strikeCount: 32,
  view: 'chain',
}

/** The server refuses a modeled |qty| above this (422 `out-of-range`). */
export const MAX_MODEL_QTY = 999
/** … and more legs than this. The store never builds a request it would refuse. */
export const MAX_MODEL_LEGS = 64

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
    comparePosition: merged.comparePosition !== false,
    expiry: typeof merged.expiry === 'string' || merged.expiry === null ? merged.expiry : AUTO_EXPIRY,
    strikeCount: STRIKE_COUNTS.includes(merged.strikeCount) ? merged.strikeCount : DEFAULT_MODEL.strikeCount,
    view: merged.view === 'graph' ? 'graph' : 'chain',
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
  /** Drag-and-drop between Model cells: the leg at `fromTosCode` moves to
   *  `to`, taking its quantity with it and replacing whatever `to` held. It
   *  stays in its place in the ticket. A drop on itself, or from a contract
   *  that is not modeled, changes nothing. */
  moveModelLeg: (fromTosCode: string, to: OptionContract) => void
  /** Drops every leg that expired before `valuationDate` (the server's date,
   *  `valuationDateOf(snapshot.as_of)`): a persisted 0DTE leg must not make
   *  every graph request a 422 the next day. A no-op when nothing expired. */
  pruneExpiredModel: (valuationDate: string) => void
  /** Empties the legs only — the chain layout and the toggles stay. */
  clearModel: () => void
  /** Opens one chain section, replacing the one on screen; `null` closes it. */
  setModelExpiry: (key: string | null) => void
  setModelStrikeCount: (count: StrikeCount) => void
  setModelView: (view: ModelView) => void
  setModelInclude: (include: boolean) => void
  setModelComparePosition: (comparePosition: boolean) => void
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
      moveModelLeg: (fromTosCode, to) =>
        set((s) => {
          if (fromTosCode === to.tos_code) return {}
          const source = s.model.legs.find((leg) => leg.contract.tos_code === fromTosCode)
          if (!source) return {}
          // One pass, so the moved leg keeps the source's slot in the ticket
          // and a leg already on the target contract is replaced, not doubled.
          const legs = s.model.legs
            .filter((leg) => leg.contract.tos_code !== to.tos_code)
            .map((leg) => (leg === source ? { contract: to, qty: source.qty } : leg))
          return { model: { ...s.model, legs } }
        }),
      pruneExpiredModel: (valuationDate) =>
        set((s) => {
          const legs = s.model.legs.filter((leg) => isLiveModelLeg(leg, valuationDate))
          return legs.length === s.model.legs.length ? {} : { model: { ...s.model, legs } }
        }),
      clearModel: () =>
        set((s) => (s.model.legs.length === 0 ? {} : { model: { ...s.model, legs: [] } })),
      setModelExpiry: (expiry) =>
        set((s) => (s.model.expiry === expiry ? {} : { model: { ...s.model, expiry } })),
      setModelStrikeCount: (strikeCount) => set((s) => ({ model: { ...s.model, strikeCount } })),
      setModelView: (view) => set((s) => (s.model.view === view ? {} : { model: { ...s.model, view } })),
      setModelInclude: (include) => set((s) => ({ model: { ...s.model, include } })),
      setModelComparePosition: (comparePosition) =>
        set((s) => ({ model: { ...s.model, comparePosition } })),
    }),
    {
      name: STORE_KEY,
      // 3 added `model`; 4 turned its `expiries` list into the single
      // `expiry`. Every earlier blob is a strict subset of this shape, so
      // `migrate` passes it through and `merge` backfills the defaults —
      // without a `migrate`, zustand would discard an old blob outright, and
      // with it the trader's selection, theme and graph settings.
      version: 4,
      migrate: (persisted) => {
        const saved = persisted as { model?: { expiries?: unknown; expiry?: unknown } } | null
        const expiries = saved?.model?.expiries
        // A v3 stack of sections keeps its last-opened one.
        if (saved?.model && Array.isArray(expiries)) {
          const { expiries: _dropped, ...rest } = saved.model
          const last = expiries.filter((key): key is string => typeof key === 'string').at(-1)
          saved.model = { ...rest, expiry: last ?? AUTO_EXPIRY }
        }
        return persisted as AppState
      },
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
