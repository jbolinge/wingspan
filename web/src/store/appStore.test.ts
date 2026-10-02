import { beforeEach, describe, expect, it } from 'vitest'
import type { OptionContract } from '../api/types'
import {
  AUTO_EXPIRY,
  DEFAULT_GRAPH,
  DEFAULT_MODEL,
  MAX_MODEL_LEGS,
  POSITIONS_MIN_HEIGHT,
  STORE_KEY,
  useAppStore,
} from './appStore'
import { resetAppStore } from '../test/utils'

beforeEach(() => resetAppStore())

const state = () => useAppStore.getState()

describe('selection', () => {
  it('toggles ids and keeps insertion order', () => {
    state().toggleSelected('a')
    state().toggleSelected('b')
    expect(state().selected).toEqual(['a', 'b'])
    state().toggleSelected('a')
    expect(state().selected).toEqual(['b'])
  })

  it('de-duplicates a bulk set', () => {
    state().setSelected(['a', 'a', 'b'])
    expect(state().selected).toEqual(['a', 'b'])
  })

  it('clears', () => {
    state().setSelected(['a'])
    state().clearSelection()
    expect(state().selected).toEqual([])
  })

  it('drops ids the new trades root does not have', () => {
    state().setSelected(['a', 'gone', 'b'])
    state().pruneSelection(['a', 'b', 'c'])
    expect(state().selected).toEqual(['a', 'b'])
  })

  it('leaves the selection array untouched when every id survives', () => {
    state().setSelected(['a', 'b'])
    const before = state().selected
    state().pruneSelection(['a', 'b', 'c'])
    expect(state().selected).toBe(before)
  })

  it('prunes expanded rows the same way', () => {
    state().toggleExpanded('a')
    state().toggleExpanded('gone')
    state().pruneExpanded(['a'])
    expect(state().expanded).toEqual(['a'])
  })
})

describe('sorting', () => {
  it('flips direction when the same column is clicked twice', () => {
    state().setSort('name')
    expect(state().sortKey).toBe('name')
    expect(state().sortDir).toBe('asc')
    state().setSort('name')
    expect(state().sortDir).toBe('desc')
  })

  it('starts numeric columns descending, because bigger is more interesting', () => {
    state().setSort('pnl_total')
    expect(state().sortDir).toBe('desc')
  })

  it('starts text columns ascending', () => {
    state().setSort('structure')
    expect(state().sortDir).toBe('asc')
  })
})

describe('expansion', () => {
  it('toggles', () => {
    state().toggleExpanded('a')
    state().toggleExpanded('b')
    expect(state().expanded).toEqual(['a', 'b'])
    state().toggleExpanded('a')
    expect(state().expanded).toEqual(['b'])
  })
})

describe('graph settings', () => {
  it('patches without dropping the rest', () => {
    state().patchGraph({ points: 481 })
    expect(state().graph.points).toBe(481)
    expect(state().graph.rangeMode).toBe(DEFAULT_GRAPH.rangeMode)
  })

  it('stores per-expiry vol shifts and removes them when zeroed', () => {
    state().setExpiryVolShift('2027-02-19', 0.02)
    expect(state().graph.volShiftByExpiry).toEqual({ '2027-02-19': 0.02 })
    state().setExpiryVolShift('2027-02-19', 0)
    expect(state().graph.volShiftByExpiry).toEqual({})
  })

  it('clears every per-expiry shift at once', () => {
    state().setExpiryVolShift('2027-02-19', 0.02)
    state().setExpiryVolShift('2027-03-19', -0.01)
    state().clearExpiryVolShifts()
    expect(state().graph.volShiftByExpiry).toEqual({})
  })

  /** By line key, not date: a 0-DTE position puts T+0 and the expiration line
   *  on the same day, and one checkbox must not hide both. */
  it('toggles line visibility by line key', () => {
    state().toggleLine('t0')
    expect(state().graph.hiddenLines).toEqual(['t0'])
    state().toggleLine('expiry')
    expect(state().graph.hiddenLines).toEqual(['t0', 'expiry'])
    state().toggleLine('t0')
    expect(state().graph.hiddenLines).toEqual(['expiry'])
  })

  it('patches the what-if sub-object independently', () => {
    state().patchWhatIf({ enabled: true, price: 6800 })
    expect(state().graph.whatIf).toEqual({ enabled: true, price: 6800, date: null, volShift: 0 })
    expect(state().graph.points).toBe(DEFAULT_GRAPH.points)
  })

  it('restores every default at once', () => {
    // The zoom/pan view is part of the graph settings, so 'restore graph
    // defaults' has to put the whole computed range back too.
    state().patchGraph({ points: 961, volShift: 0.05, yLock: true, xZoom: 8, xCenter: 7100 })
    state().resetGraph()
    expect(state().graph).toEqual(DEFAULT_GRAPH)
    expect(state().graph).toMatchObject({ xZoom: 1, xCenter: null })
  })
})

describe('theme', () => {
  it('toggles between dark and light', () => {
    expect(state().theme).toBe('dark')
    state().toggleTheme()
    expect(state().theme).toBe('light')
    state().toggleTheme()
    expect(state().theme).toBe('dark')
  })
})

describe('persistence', () => {
  it('writes the selection and graph settings under the documented key', () => {
    state().setSelected(['oct-strangle-2026-08-24'])
    state().patchGraph({ points: 481 })
    const raw = localStorage.getItem(STORE_KEY)
    expect(raw).toBeTruthy()
    const parsed = JSON.parse(raw ?? '{}')
    expect(parsed.state.selected).toEqual(['oct-strangle-2026-08-24'])
    expect(parsed.state.graph.points).toBe(481)
    expect(parsed.version).toBe(4)
  })

  it('backfills graph settings added after an old blob was written', () => {
    const merged = useAppStore.persist.getOptions().merge?.(
      { theme: 'light', graph: { points: 121 } },
      useAppStore.getState(),
    ) as { theme: string; graph: typeof DEFAULT_GRAPH }
    expect(merged.theme).toBe('light')
    expect(merged.graph.points).toBe(121)
    expect(merged.graph.includeRealized).toBe(DEFAULT_GRAPH.includeRealized)
    // A blob written before the viewport existed must not leave xZoom
    // undefined: every read of it would be NaN, and the chart blank.
    expect(merged.graph.xZoom).toBe(DEFAULT_GRAPH.xZoom)
    expect(merged.graph.xCenter).toBe(DEFAULT_GRAPH.xCenter)
  })
})

describe('layout', () => {
  it('clamps a dragged positions height to the readable minimum', () => {
    state().setPositionsHeight(20)
    expect(state().positionsHeight).toBe(POSITIONS_MIN_HEIGHT)
    state().setPositionsHeight(333.6)
    expect(state().positionsHeight).toBe(334)
    state().setPositionsHeight(null)
    expect(state().positionsHeight).toBeNull()
    state().setPositionsHeight(Number.NaN)
    expect(state().positionsHeight).toBeNull()
  })

  it('persists the collapse and height preferences but not the maximized graph', () => {
    state().setPositionsCollapsed(true)
    state().setWarningsCollapsed(true)
    state().setPositionsHeight(200)
    state().setGraphMaximized(true)
    const saved = JSON.parse(localStorage.getItem(STORE_KEY) ?? '{}') as {
      state: Record<string, unknown>
    }
    expect(saved.state.positionsCollapsed).toBe(true)
    expect(saved.state.warningsCollapsed).toBe(true)
    expect(saved.state.positionsHeight).toBe(200)
    expect(saved.state).not.toHaveProperty('graphMaximized')
  })
})

describe('cleared warnings', () => {
  it('accumulates fingerprints without duplicates and restores all at once', () => {
    state().dismissWarnings(['a', 'b'])
    state().dismissWarnings(['b', 'c'])
    expect(state().dismissedWarnings).toEqual(['a', 'b', 'c'])
    const saved = JSON.parse(localStorage.getItem(STORE_KEY) ?? '{}') as {
      state: { dismissedWarnings: string[] }
    }
    expect(saved.state.dismissedWarnings).toEqual(['a', 'b', 'c'])
    state().restoreWarnings()
    expect(state().dismissedWarnings).toEqual([])
  })
})

function contract(strike: number, right: 'call' | 'put' = 'put'): OptionContract {
  const cp = right === 'call' ? 'C' : 'P'
  return {
    underlying: 'SPX',
    root: 'SPXW',
    expiry: '2026-10-16',
    settlement: 'pm',
    strike,
    right,
    multiplier: 100,
    osi: `SPXW  261016${cp}0${strike}000`,
    tos_code: `SPXW261016${cp}${strike}`,
  }
}

describe('the model slice', () => {
  it('starts empty, drawn, not comparing, on 32 automatic strikes, showing the chain', () => {
    expect(state().model).toEqual(DEFAULT_MODEL)
    expect(DEFAULT_MODEL).toEqual({
      legs: [],
      include: true,
      compare: false,
      expiry: AUTO_EXPIRY,
      strikeCount: 32,
      view: 'chain',
    })
  })

  it('keys legs by contract: a second quantity replaces the first, in place', () => {
    state().setModelQty(contract(7500), -1)
    state().setModelQty(contract(7400), 1)
    state().setModelQty(contract(7500), -3)
    expect(state().model.legs.map((leg) => [leg.contract.tos_code, leg.qty])).toEqual([
      ['SPXW261016P7500', -3],
      ['SPXW261016P7400', 1],
    ])
  })

  it('removes a leg set to zero, and never adds one at zero', () => {
    state().setModelQty(contract(7500), 2)
    state().setModelQty(contract(7500), 0)
    state().setModelQty(contract(7400), 0)
    expect(state().model.legs).toEqual([])
  })

  it('rounds non-integers and clamps to ±999', () => {
    state().setModelQty(contract(7500), 2.6)
    state().setModelQty(contract(7400), -5000)
    state().setModelQty(contract(7300), 0.4)
    state().setModelQty(contract(7200), Number.NaN)
    expect(state().model.legs.map((leg) => leg.qty)).toEqual([3, -999])
  })

  it('refuses a new contract past the 64-leg limit but still edits existing ones', () => {
    for (let i = 0; i < MAX_MODEL_LEGS; i += 1) state().setModelQty(contract(6000 + i * 5), 1)
    state().setModelQty(contract(9000), 1)
    expect(state().model.legs).toHaveLength(MAX_MODEL_LEGS)
    state().setModelQty(contract(6000), 4)
    expect(state().model.legs[0]?.qty).toBe(4)
  })

  it('removes one leg by TOS code, and clears only the legs', () => {
    state().setModelQty(contract(7500), -1)
    state().setModelQty(contract(7400), 1)
    state().setModelStrikeCount(64)
    state().setModelExpiry('2026-10-16|SPXW')
    state().setModelCompare(true)
    state().removeModelLeg('SPXW261016P7500')
    expect(state().model.legs.map((leg) => leg.contract.tos_code)).toEqual(['SPXW261016P7400'])
    state().clearModel()
    expect(state().model).toEqual({
      ...DEFAULT_MODEL,
      strikeCount: 64,
      expiry: '2026-10-16|SPXW',
      compare: true,
    })
  })

  it('moves a dragged leg to another contract in place, replacing what was there', () => {
    state().setModelQty(contract(7500), -1)
    state().setModelQty(contract(7400), 1)
    state().setModelQty(contract(7300), 2)
    // Sliding the short put down the chain onto a strike already modeled:
    // the −1 lands on 7400, the +1 that was there is gone, and the ticket
    // order is the source's, not "appended at the end".
    state().moveModelLeg('SPXW261016P7500', contract(7400))
    expect(state().model.legs.map((leg) => [leg.contract.tos_code, leg.qty])).toEqual([
      ['SPXW261016P7400', -1],
      ['SPXW261016P7300', 2],
    ])
    // Onto an empty strike, and across rights.
    state().moveModelLeg('SPXW261016P7300', contract(7800, 'call'))
    expect(state().model.legs.map((leg) => [leg.contract.tos_code, leg.qty])).toEqual([
      ['SPXW261016P7400', -1],
      ['SPXW261016C7800', 2],
    ])
  })

  it('ignores a drop on itself or from a contract that is not modeled', () => {
    state().setModelQty(contract(7500), -1)
    const before = state().model
    state().moveModelLeg('SPXW261016P7500', contract(7500))
    state().moveModelLeg('SPXW261016P7200', contract(7400))
    expect(state().model).toBe(before)
  })

  // A persisted 0DTE leg from yesterday would make the server refuse every
  // graph request carrying it; the date is the server's, so the boundary day
  // itself is still live.
  it('prunes legs that expired before the valuation date, and only those', () => {
    const daily = { ...contract(7600), expiry: '2026-09-30', tos_code: 'SPXW260930P7600' }
    const today = { ...contract(7650), expiry: '2026-10-01', tos_code: 'SPXW261001P7650' }
    state().setModelQty(daily, -1)
    state().setModelQty(today, 2)
    state().setModelQty(contract(7500), 1)
    state().setModelStrikeCount(64)
    state().pruneExpiredModel('2026-10-01')
    expect(state().model.legs.map((leg) => leg.contract.tos_code)).toEqual([
      'SPXW261001P7650',
      'SPXW261016P7500',
    ])
    expect(state().model.strikeCount).toBe(64)
    // Nothing expired: the same slice object, so nothing re-renders.
    const before = state().model
    state().pruneExpiredModel('2026-10-01')
    expect(state().model).toBe(before)
  })

  it('opens one section at a time, and lets it be closed', () => {
    state().setModelExpiry('2026-10-16|SPXW')
    state().setModelExpiry('2026-11-20|SPX')
    expect(state().model.expiry).toBe('2026-11-20|SPX')
    state().setModelExpiry(null)
    expect(state().model.expiry).toBeNull()
    // Closing is not the same as never having chosen.
    expect(state().model.expiry).not.toBe(AUTO_EXPIRY)
  })

  it('leaves the slice untouched when the section does not change', () => {
    state().setModelExpiry('2026-10-16|SPXW')
    const before = state().model
    state().setModelExpiry('2026-10-16|SPXW')
    expect(state().model).toBe(before)
  })

  it('switches the tab between the chain and the risk graph, keeping the legs', () => {
    state().setModelQty(contract(7500), -1)
    state().setModelView('graph')
    expect(state().model.view).toBe('graph')
    expect(state().model.legs).toHaveLength(1)
    const before = state().model
    state().setModelView('graph')
    expect(state().model).toBe(before)
    state().setModelView('chain')
    expect(state().model.view).toBe('chain')
  })

  it('flips the include, compare and strike-count settings', () => {
    state().setModelInclude(false)
    state().setModelCompare(true)
    state().setModelStrikeCount(null)
    expect(state().model).toMatchObject({ include: false, compare: true, strikeCount: null })
  })
})

describe('persistence, version 4', () => {
  it('persists the model slice', () => {
    state().setModelQty(contract(7500), -2)
    const saved = JSON.parse(localStorage.getItem(STORE_KEY) ?? '{}') as {
      state: { model: { legs: { qty: number }[] } }
      version: number
    }
    expect(saved.version).toBe(4)
    expect(saved.state.model.legs[0]?.qty).toBe(-2)
  })

  it('carries a version-2 blob over rather than discarding it', async () => {
    localStorage.setItem(
      STORE_KEY,
      JSON.stringify({ state: { theme: 'light', selected: ['a'], graph: { points: 121 } }, version: 2 }),
    )
    await useAppStore.persist.rehydrate()
    expect(state().theme).toBe('light')
    expect(state().selected).toEqual(['a'])
    expect(state().graph.points).toBe(121)
    expect(state().model).toEqual(DEFAULT_MODEL)
  })

  it('keeps a stored section, and an explicit "no chain open"', () => {
    const merge = useAppStore.persist.getOptions().merge
    if (!merge) throw new Error('the store has no merge')
    const of = (expiry: unknown) =>
      (merge({ model: { expiry } }, useAppStore.getState()) as { model: typeof DEFAULT_MODEL }).model.expiry
    expect(of('2026-10-16|SPXW')).toBe('2026-10-16|SPXW')
    expect(of(null)).toBeNull()
    expect(of(undefined)).toBe(AUTO_EXPIRY)
  })

  it('keeps a stored view, and reads anything else as the chain', () => {
    const merge = useAppStore.persist.getOptions().merge
    if (!merge) throw new Error('the store has no merge')
    const of = (view: unknown) =>
      (merge({ model: { view } }, useAppStore.getState()) as { model: typeof DEFAULT_MODEL }).model.view
    expect(of('graph')).toBe('graph')
    expect(of('chain')).toBe('chain')
    expect(of(undefined)).toBe('chain')
    expect(of('table')).toBe('chain')
  })

  it('migrates a version-3 expiries stack to its last-opened section', async () => {
    const blob = (expiries: unknown[]) =>
      JSON.stringify({ state: { theme: 'light', model: { legs: [], strikeCount: 64, expiries } }, version: 3 })
    localStorage.setItem(STORE_KEY, blob(['2026-10-16|SPXW', '2026-11-20|SPX']))
    await useAppStore.persist.rehydrate()
    expect(state().theme).toBe('light')
    expect(state().model.expiry).toBe('2026-11-20|SPX')
    expect(state().model.strikeCount).toBe(64)
    expect(state().model).not.toHaveProperty('expiries')
    // A v3 blob had no view: it opens on the chain, as before.
    expect(state().model.view).toBe('chain')

    // An empty stack was the automatic set.
    localStorage.setItem(STORE_KEY, blob([]))
    await useAppStore.persist.rehydrate()
    expect(state().model.expiry).toBe(AUTO_EXPIRY)
  })

  it('backfills and re-validates a stored model slice', () => {
    const merged = useAppStore.persist.getOptions().merge?.(
      {
        model: {
          legs: [
            { contract: contract(7500), qty: 2.4 },
            { contract: contract(7500), qty: 5 },
            { contract: contract(7400), qty: 0 },
            { contract: { strike: 1 }, qty: 1 },
            { contract: contract(7300), qty: 5000 },
          ],
          strikeCount: 17,
          expiry: 7,
        },
      },
      useAppStore.getState(),
    ) as { model: typeof DEFAULT_MODEL }
    expect(merged.model.legs.map((leg) => [leg.contract.tos_code, leg.qty])).toEqual([
      ['SPXW261016P7500', 2],
      ['SPXW261016P7300', 999],
    ])
    expect(merged.model.strikeCount).toBe(DEFAULT_MODEL.strikeCount)
    expect(merged.model.expiry).toBe(AUTO_EXPIRY)
    expect(merged.model.include).toBe(true)
    expect(merged.model.compare).toBe(false)
  })
})
