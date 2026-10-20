/**
 * Round-3 audit, SPA side: hiding an expiration by date, the terminal line
 * the header names, the chart's line summary, pruning past date specs, the
 * New York day, later-expiration styles, greek precision and the what-if
 * date clamp.
 */

import { beforeEach, describe, expect, it } from 'vitest'
import type { RiskGraphResponse, RiskLine } from '../api/types'
import { DEFAULT_GRAPH, useAppStore } from '../store/appStore'
import { resetAppStore } from '../test/utils'
import { newYorkDateOf } from './dates'
import { fmtGreek, fmtLegGreek } from './format'
import { buildWhatIfRequest } from './graphRequest'
import {
  describeLines,
  hideKey,
  isLineHidden,
  liveDateSpecs,
  terminalLabel,
  terminalOf,
} from './lines'
import { expiryRanks, expiryStyle } from './palette'

const line = (kind: RiskLine['kind'], key: string, date: string): RiskLine => ({
  kind,
  key,
  date,
  label: '',
  pnl: [0],
})

const OCT = line('expiry', 'expiry', '2026-10-16')
const DEC = line('expiry', 'expiry:2026-12-18', '2026-12-18')
const T0 = line('t0', 't0', '2026-09-15')
const T12 = line('tplus', 't+12', '2026-09-27')

describe('hideKey', () => {
  it('hides an expiration by its date, never by the bare key', () => {
    expect(hideKey(OCT)).toBe('expiry:2026-10-16')
    expect(hideKey(DEC)).toBe('expiry:2026-12-18')
    expect(hideKey(T0)).toBe('t0')
    expect(hideKey(T12)).toBe('t+12')
  })

  it('keeps a hidden Oct expiry off the Dec line once Dec becomes the canonical one', () => {
    const hidden = [hideKey(OCT)]
    // Oct leaves the selection: Dec now owns the bare `expiry` key.
    const decAlone = line('expiry', 'expiry', '2026-12-18')
    expect(isLineHidden(decAlone, hidden)).toBe(false)
    expect(isLineHidden(OCT, hidden)).toBe(true)
    // A legacy blob's bare key hides nothing.
    expect(isLineHidden(OCT, ['expiry'])).toBe(false)
  })
})

describe('terminalOf / terminalLabel', () => {
  const data = (overrides: Partial<RiskGraphResponse>): Pick<RiskGraphResponse, 'lines' | 'terminal'> => ({
    lines: [T0, T12, OCT, DEC],
    ...overrides,
  })

  it('takes the server terminal when there is one', () => {
    const terminal = { key: 'expiry', date: '2026-10-16', kind: 'expiry' as const, drawn: false }
    expect(terminalOf(data({ lines: [T0, T12], terminal }))).toBe(terminal)
    expect(terminalLabel(terminal)).toBe('at EXP Oct 16 2026')
  })

  it('falls back to the canonical expiry line, else the last line drawn', () => {
    expect(terminalOf(data({}))).toEqual({ key: 'expiry', date: '2026-10-16', kind: 'expiry', drawn: true })
    expect(terminalOf(data({ lines: [T0, T12] }))).toEqual({
      key: 't+12',
      date: '2026-09-27',
      kind: 'tplus',
      drawn: true,
    })
    expect(terminalOf(data({ lines: [] }))).toBeNull()
    expect(terminalLabel({ key: 't+12', date: '2026-09-27', kind: 'tplus', drawn: true })).toBe('at T+12')
  })
})

describe('describeLines', () => {
  it('counts expiration lines as such, and T+0 as the first T+x rung', () => {
    expect(describeLines([T0, T12, OCT])).toBe('expiration line plus 2 T+x lines')
    expect(describeLines([T0, T12, OCT, DEC])).toBe('2 expiration lines plus 2 T+x lines')
    expect(describeLines([T0, T12])).toBe('no expiration line, 2 T+x lines')
    expect(describeLines([T0, OCT])).toBe('expiration line plus 1 T+x line')
  })
})

describe('liveDateSpecs', () => {
  it('drops only the expiry: specs before the server date', () => {
    const specs = ['today', '+12d', 'expiry:2026-09-14', 'expiry:2026-09-15', 'expiry:2026-10-16']
    expect(liveDateSpecs(specs, '2026-09-15')).toEqual(['today', '+12d', 'expiry:2026-09-15', 'expiry:2026-10-16'])
  })

  it('returns the same array when nothing is stale', () => {
    const specs = ['today', 'expiry:2026-10-16']
    expect(liveDateSpecs(specs, '2026-09-15')).toBe(specs)
  })
})

describe('newYorkDateOf', () => {
  it('is the New York day, not the UTC one', () => {
    // 21:30 ET on the 15th is already the 16th in UTC.
    expect(newYorkDateOf('2026-09-16T01:30:00Z')).toBe('2026-09-15')
    expect(newYorkDateOf('2026-09-15T16:00:00Z')).toBe('2026-09-15')
    expect(newYorkDateOf('2026-12-16T04:59:00Z')).toBe('2026-12-15')
    expect(newYorkDateOf(null)).toBeNull()
    expect(newYorkDateOf('not a date')).toBeNull()
  })
})

describe('store pruning', () => {
  beforeEach(() => resetAppStore())
  const graph = () => useAppStore.getState().graph

  it('drops past expiry: specs and a past what-if date once the server date is known', () => {
    useAppStore.getState().patchGraph({
      dates: ['today', 'expiry:2026-09-11', 'expiry:2026-10-16'],
      whatIf: { ...DEFAULT_GRAPH.whatIf, date: '2026-09-14' },
    })
    useAppStore.getState().pruneExpiredGraphDates('2026-09-15')
    expect(graph().dates).toEqual(['today', 'expiry:2026-10-16'])
    expect(graph().whatIf.date).toBeNull()
  })

  it('falls back to the preset when every spec was past', () => {
    useAppStore.getState().patchGraph({ dates: ['expiry:2026-09-11'] })
    useAppStore.getState().pruneExpiredGraphDates('2026-09-15')
    expect(graph().dates).toBeNull()
  })

  it('changes nothing when nothing is stale', () => {
    useAppStore.getState().patchGraph({ dates: ['today', 'expiry:2026-10-16'] })
    const before = graph()
    useAppStore.getState().pruneExpiredGraphDates('2026-09-15')
    expect(graph()).toBe(before)
  })

  it('keeps only hidden keys a drawn line still has', () => {
    useAppStore.getState().patchGraph({ hiddenLines: ['expiry', 't+12', 'expiry:2026-10-16', 't+40'] })
    useAppStore.getState().pruneHiddenLines([T0, T12, OCT, DEC].map(hideKey))
    expect(graph().hiddenLines).toEqual(['t+12', 'expiry:2026-10-16'])
    const before = graph()
    useAppStore.getState().pruneHiddenLines([T0, T12, OCT, DEC].map(hideKey))
    expect(graph()).toBe(before)
  })
})

describe('later expiration lines', () => {
  it('draw thinner, each with a dash of its own; the earliest stays solid 3 px', () => {
    expect(expiryStyle(0)).toEqual({ width: 3, dash: null })
    const later = [1, 2, 3, 4].map(expiryStyle)
    for (const style of later) {
      expect(style.dash).not.toBeNull()
      expect(style.width).toBeLessThan(3)
    }
    expect(new Set(later.map((style) => style.dash!.join(','))).size).toBe(4)
  })

  it('are ranked by date over every line, whatever the request order', () => {
    const ranks = expiryRanks([DEC, T0, OCT])
    expect(ranks.get('expiry')).toBe(0)
    expect(ranks.get('expiry:2026-12-18')).toBe(1)
    expect(ranks.has('t0')).toBe(false)
  })
})

describe('fmtGreek precision', () => {
  it('never prints a negative zero', () => {
    expect(fmtGreek('theta', -0.04)).toBe('0.0')
    expect(fmtGreek('vega', -0.0001)).toBe('0.0')
    expect(fmtGreek('gamma', -0.00002)).toBe('0.0000')
    expect(fmtGreek('delta', -0.001)).toBe('0.00')
    expect(fmtLegGreek('gamma', -0.000001)).toBe('0.00000')
  })

  it('shows one decimal of theta and vega below 10, whole dollars from 10', () => {
    expect(fmtGreek('theta', -3.46)).toBe('-3.5')
    expect(fmtGreek('vega', 0.84)).toBe('0.8')
    expect(fmtGreek('vega', -9.94)).toBe('-9.9')
    expect(fmtGreek('theta', 9.96)).toBe('10')
    expect(fmtGreek('theta', -55.4)).toBe('-55')
    expect(fmtGreek('vega', 1234.4)).toBe('1,234')
  })
})

describe('buildWhatIfRequest date', () => {
  it('falls back to the server day for a stored date it has passed', () => {
    const graph = { ...DEFAULT_GRAPH, whatIf: { ...DEFAULT_GRAPH.whatIf, date: '2026-09-14' } }
    expect(buildWhatIfRequest(['a'], graph, 7750, [], '2026-09-15')?.date).toBe('2026-09-15')
    const later = { ...DEFAULT_GRAPH, whatIf: { ...DEFAULT_GRAPH.whatIf, date: '2026-10-16' } }
    expect(buildWhatIfRequest(['a'], later, 7750, [], '2026-09-15')?.date).toBe('2026-10-16')
  })
})
