import { beforeEach, describe, expect, it } from 'vitest'
import { DEFAULT_GRAPH, STORE_KEY, useAppStore } from './appStore'
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
    state().patchGraph({ points: 961, volShift: 0.05, yLock: true })
    state().resetGraph()
    expect(state().graph).toEqual(DEFAULT_GRAPH)
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
    expect(parsed.version).toBe(2)
  })

  it('backfills graph settings added after an old blob was written', () => {
    const merged = useAppStore.persist.getOptions().merge?.(
      { theme: 'light', graph: { points: 121 } },
      useAppStore.getState(),
    ) as { theme: string; graph: typeof DEFAULT_GRAPH }
    expect(merged.theme).toBe('light')
    expect(merged.graph.points).toBe(121)
    expect(merged.graph.includeRealized).toBe(DEFAULT_GRAPH.includeRealized)
  })
})
