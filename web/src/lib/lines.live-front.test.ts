/**
 * The SPA's live-expiry rule follows the engine's live front: an expiry
 * dated today is live until its settlement instant, which only the server
 * knows. Once the response's `front_expiry` is later than today (17:00 ET on
 * a PM expiry day, or after 09:30 ET on an AM-only date), today's expiry has
 * settled and is no front for the presets, the "all expiries" list or the
 * per-expiry vol sliders.
 */

import { describe, expect, it } from 'vitest'
import { mockRiskGraph } from '../api/mock'
import { liveExpiries } from './lines'

const expiries = ['2026-10-16', '2026-10-19', '2026-11-20']

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

  it('the mock states a front_expiry the way the server does', () => {
    const graph = mockRiskGraph({
      trade_ids: [],
      price_range: null,
      points: 5,
      dates: null,
      vol_shift: 0,
      vol_shift_by_expiry: {},
      spot_override: null,
      include_realized: true,
      model_legs: [],
    })
    expect(graph).toHaveProperty('front_expiry')
  })
})
