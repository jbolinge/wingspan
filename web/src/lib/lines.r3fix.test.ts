/** Round-3 review fixes in lines.ts: the header's breakevens fall back to the
 *  analysis axis when the drawn window misses a crossing, and hidden-line
 *  keys survive a selection that does not draw them. */

import { beforeEach, describe, expect, it } from 'vitest'
import { useAppStore } from '../store/appStore'
import { resetAppStore } from '../test/utils'
import { headerBreakevens, liveHiddenLines } from './lines'

const terminal = (analysis: number[]) => ({
  key: 'expiry',
  date: '2026-10-16',
  kind: 'expiry' as const,
  drawn: true,
  analysis_breakevens: analysis,
})

describe('headerBreakevens', () => {
  it('reads the drawn range when it holds every crossing', () => {
    const data = { prices: [6200, 6500, 6800], breakevens: { expiry: [6270, 6730] } }
    expect(headerBreakevens(data, terminal([6270, 6730]))).toEqual([
      { value: 6270, offRange: false },
      { value: 6730, offRange: false },
    ])
  })

  it('falls back to the analysis axis, flagging crossings outside a narrow window', () => {
    const data = { prices: [6450, 6500, 6550], breakevens: { expiry: [] } }
    expect(headerBreakevens(data, terminal([6270, 6730]))).toEqual([
      { value: 6270, offRange: true },
      { value: 6730, offRange: true },
    ])
    const half = { prices: [6250, 6500, 6550], breakevens: { expiry: [6270] } }
    expect(headerBreakevens(half, terminal([6270, 6730]))).toEqual([
      { value: 6270, offRange: false },
      { value: 6730, offRange: true },
    ])
  })

  it('is empty with no crossings anywhere', () => {
    expect(headerBreakevens({ prices: [6400, 6600], breakevens: {} }, null)).toEqual([])
  })
})

describe('hidden-line pruning', () => {
  beforeEach(() => resetAppStore())

  it('keeps a hide the current selection does not draw', () => {
    expect(liveHiddenLines(['expiry:2026-10-16', 't+12@2026-09-27'], '2026-09-15')).toEqual([
      'expiry:2026-10-16',
      't+12@2026-09-27',
    ])
  })

  it('survives switching to a trade without that line and back', () => {
    useAppStore.getState().patchGraph({ hiddenLines: ['expiry:2026-10-16'] })
    // Every answer prunes against the server date only: a Dec-only selection
    // answering without the Oct line does not retire the Oct hide.
    useAppStore.getState().pruneExpiredGraphDates('2026-09-15')
    useAppStore.getState().pruneExpiredGraphDates('2026-09-15')
    expect(useAppStore.getState().graph.hiddenLines).toEqual(['expiry:2026-10-16'])
    // Once Oct 16 has passed, the key can never match again.
    useAppStore.getState().pruneExpiredGraphDates('2026-10-17')
    expect(useAppStore.getState().graph.hiddenLines).toEqual([])
  })
})
