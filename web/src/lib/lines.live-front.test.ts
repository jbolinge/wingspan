/**
 * The SPA's live-expiry rule follows the engine's live front: an expiry
 * dated today is live until its settlement instant, which only the server
 * knows. Once the response's `front_expiry` is later than today (17:00 ET on
 * a PM expiry day, or after 09:30 ET on an AM-only date), today's expiry has
 * settled and is no front for the presets, the "all expiries" list or the
 * per-expiry vol sliders.
 */

import { afterEach, describe, expect, it, vi } from 'vitest'
import { mockRequest, mockRiskGraph } from '../api/mock'
import type { TradesResponse } from '../api/types'
import { todayIso } from './dates'
import { frontForSelection, liveExpiries, selectionKey } from './lines'

const expiries = ['2026-10-16', '2026-10-19', '2026-11-20']

afterEach(() => {
  vi.useRealTimers()
})

describe('liveExpiries against the server front', () => {
  it('drops today once the server front has moved past it (after the settlement)', () => {
    // 17:00 ET on 2026-10-19: its PM legs settled at 16:00, the engine's
    // live front is the November expiry.
    expect(liveExpiries(expiries, '2026-10-19', '2026-11-20')).toEqual(['2026-11-20'])
  })

  it('keeps today while the server still calls it the front (before the settlement)', () => {
    expect(liveExpiries(expiries, '2026-10-19', '2026-10-19')).toEqual(['2026-10-19', '2026-11-20'])
  })

  it('keeps today before the first answer, and when every leg has settled', () => {
    expect(liveExpiries(expiries, '2026-10-19')).toEqual(['2026-10-19', '2026-11-20'])
    expect(liveExpiries(expiries, '2026-10-19', null)).toEqual(['2026-10-19', '2026-11-20'])
    // All settled: the engine falls back to the earliest expiry, which is
    // not later than today.
    expect(liveExpiries(['2026-10-19'], '2026-10-19', '2026-10-19')).toEqual(['2026-10-19'])
  })

  it('never drops a later expiry, whatever the server front', () => {
    // A stale answer for another selection must not hide a later expiry.
    expect(liveExpiries(expiries, '2026-10-16', '2026-11-20')).toEqual(['2026-10-19', '2026-11-20'])
  })

  it('the mock states the earliest live expiry as front_expiry, the way the server does', () => {
    // A pinned demo day, so the fixed demo expiries stay in the future.
    vi.useFakeTimers({ toFake: ['Date'] })
    vi.setSystemTime(new Date(2026, 7, 31, 11, 0, 0))
    const trades = (mockRequest('/trades') as TradesResponse).trades
    const trade = trades.find((t) => t.name === 'Dec Fly')!
    const graph = mockRiskGraph({
      trade_ids: [trade.id],
      price_range: null,
      points: 5,
      dates: null,
      vol_shift: 0,
      vol_shift_by_expiry: {},
      spot_override: null,
      include_realized: true,
      model_legs: [],
    })
    const live = trade.legs
      .filter((leg) => leg.qty !== 0 && leg.contract.expiry >= todayIso())
      .map((leg) => leg.contract.expiry)
      .sort()
    expect(live.length).toBeGreaterThan(0)
    expect(graph.front_expiry).toBe(live[0])
  })
})

describe('frontForSelection', () => {
  const nov = { front_expiry: '2026-11-20' }
  const oneTrade = selectionKey({ trade_ids: ['a'], model_legs: [] })
  const twoTrades = selectionKey({ trade_ids: ['b', 'a'], model_legs: [] })

  it('passes the front of an answer for the selection on screen', () => {
    expect(frontForSelection(nov, oneTrade, oneTrade)).toBe('2026-11-20')
    // Order does not matter, only which trades.
    expect(selectionKey({ trade_ids: ['a', 'b'], model_legs: [] })).toBe(twoTrades)
  })

  it("ignores another selection's answer, so today's live expiry is not dropped", () => {
    // The previous selection (no leg today) answered November; the new one
    // holds a PM leg expiring today that is still live.
    const front = frontForSelection(nov, oneTrade, twoTrades)
    expect(front).toBeUndefined()
    expect(liveExpiries(expiries, '2026-10-19', front)).toEqual(['2026-10-19', '2026-11-20'])
  })

  it('knows nothing before the first answer', () => {
    expect(frontForSelection(undefined, oneTrade, oneTrade)).toBeUndefined()
    expect(frontForSelection(nov, undefined, oneTrade)).toBeUndefined()
    expect(frontForSelection(nov, null, null)).toBeUndefined()
  })
})
