/** Round-4 review fix in lines.ts: a hidden T+x line is hidden by its key
 *  and its date, so the default ladder moving a day does not carry the hide
 *  onto whichever line takes that offset next. */

import { beforeEach, describe, expect, it } from 'vitest'
import type { RiskLine } from '../api/types'
import { useAppStore } from '../store/appStore'
import { resetAppStore } from '../test/utils'
import { hideKey, isLineHidden, liveHiddenLines } from './lines'

const tplus = (offset: number, date: string): RiskLine => ({
  key: `t+${offset}`,
  label: `T+${offset}`,
  date,
  kind: 'tplus',
  pnl: [0],
})

describe('hidden T+x lines across a day roll', () => {
  beforeEach(() => resetAppStore())

  it('names one day’s line, and stops matching once the ladder moves', () => {
    // Monday: the front's ladder has T+38 on Nov 27.
    const monday = tplus(38, '2026-11-27')
    const hidden = [hideKey(monday)]
    expect(hidden).toEqual(['t+38@2026-11-27'])
    expect(isLineHidden(monday, hidden)).toBe(true)
    // Tuesday: a day less to the front, the ladder lays T+38 on Nov 28 —
    // another line, which the Monday hide must not touch.
    expect(isLineHidden(tplus(38, '2026-11-28'), hidden)).toBe(false)
    // Nor the line on Nov 27, which Tuesday calls T+37.
    expect(isLineHidden(tplus(37, '2026-11-27'), hidden)).toBe(false)
  })

  it('keeps the hide all day, and retires it when the server day rolls', () => {
    useAppStore.getState().patchGraph({ hiddenLines: ['t0', 't+38@2026-11-27'] })
    useAppStore.getState().pruneExpiredGraphDates('2026-10-20')
    expect(useAppStore.getState().graph.hiddenLines).toEqual(['t0', 't+38@2026-11-27'])
    useAppStore.getState().pruneExpiredGraphDates('2026-10-21')
    expect(useAppStore.getState().graph.hiddenLines).toEqual(['t0'])
  })

  it('drops a legacy bare T+N key, which no line matches any more', () => {
    expect(liveHiddenLines(['t+38', 't0'], '2026-10-20')).toEqual(['t0'])
    expect(isLineHidden(tplus(38, '2026-11-27'), ['t+38'])).toBe(false)
  })
})
