/**
 * Round-3 audit, SPA side: the mock values on its own pinned instant (not the
 * browser clock), the vitest run pins `Date` for every test, PoP and ATM IV
 * are nullable, the response names its terminal line, a past `expiry:` spec
 * is dropped with a warning, and a what-if before the valuation date is
 * refused.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { MockProblem, MOCK_NOW, MOCK_TODAY, mockRequest, mockRiskGraph, mockSnapshot, mockWhatIf } from './mock'
import type { RiskGraphRequest, TradesResponse, WhatIfRequest } from './types'

const OCT = '2026-10-16'
const NOV = '2026-11-20'

function request(overrides: Partial<RiskGraphRequest> = {}): RiskGraphRequest {
  return {
    trade_ids: ['oct-strangle-2026-08-24'],
    price_range: null,
    points: 241,
    dates: null,
    vol_shift: 0,
    vol_shift_by_expiry: {},
    spot_override: null,
    include_realized: true,
    model_legs: [],
    ...overrides,
  }
}

const trades = () => (mockRequest('/trades') as TradesResponse).trades
const strangle = () => trades().find((trade) => trade.name === 'Oct Strangle')!

describe('the vitest clock', () => {
  it('starts every test on 2026-09-15 12:00 New York', () => {
    expect(new Date().toISOString()).toBe('2026-09-15T16:00:00.000Z')
    expect(Date.now()).toBe(Date.parse('2026-09-15T16:00:00Z'))
  })

  describe('a test that fakes timers itself', () => {
    beforeEach(() => {
      vi.useFakeTimers({ toFake: ['Date'] })
      vi.setSystemTime(new Date('2027-06-01T15:00:00Z'))
    })
    afterEach(() => vi.useRealTimers())

    it('overrides the pin', () => {
      expect(new Date().toISOString()).toBe('2027-06-01T15:00:00.000Z')
    })
  })

  it('is pinned again after a test that restored real timers', () => {
    expect(new Date().toISOString()).toBe('2026-09-15T16:00:00.000Z')
  })
})

describe('the mock clock (MOCK_NOW)', () => {
  it('is the Rust demo instant: Monday 2026-08-31, noon in New York', () => {
    expect(MOCK_NOW).toBe('2026-08-31T16:00:00.000Z')
    expect(MOCK_TODAY).toBe('2026-08-31')
    expect(mockSnapshot().as_of).toBe(MOCK_NOW)
  })

  describe('whatever the wall clock says', () => {
    beforeEach(() => {
      vi.useFakeTimers({ toFake: ['Date'] })
      // Long after every expiry the book holds.
      vi.setSystemTime(new Date('2027-06-01T15:00:00Z'))
    })
    afterEach(() => vi.useRealTimers())

    it('keeps the Oct Strangle a live two-legged strangle, 46 days out', () => {
      const trade = strangle()
      expect(trade.legs).toHaveLength(2)
      expect(trade.dte).toBe(46)
      expect(trade.legs.every((leg) => leg.quote_as_of === MOCK_NOW)).toBe(true)
    })

    it('draws its graph on the mock day, to the Oct expiry', () => {
      const graph = mockRiskGraph(request())
      expect(graph.as_of).toBe(MOCK_NOW)
      expect(graph.lines[0]).toMatchObject({ kind: 't0', date: MOCK_TODAY })
      expect(graph.front_expiry).toBe(OCT)
      expect(graph.terminal).toEqual({ key: 'expiry', date: OCT, kind: 'expiry', drawn: true })
    })
  })
})

describe('nullable PoP and ATM IV', () => {
  it('reports both for a live book', () => {
    const graph = mockRiskGraph(request())
    expect(graph.pop).not.toBeNull()
    expect(graph.pop!).toBeGreaterThan(0)
    expect(graph.pop!).toBeLessThan(1)
    expect(graph.front_atm_iv).toBeGreaterThan(0)
  })

  it('answers null, not 0, with no legs to measure', () => {
    // An empty selection with no model is nothing; the whole-book rule is
    // the server's, so ask for a trade id the book does not hold.
    const graph = mockRiskGraph(request({ trade_ids: ['no-such-trade'] }))
    expect(graph.pop).toBeNull()
    expect(graph.front_atm_iv).toBeNull()
    expect(graph.terminal).toMatchObject({ kind: 't0', drawn: true })
  })

  it('answers a null PoP when the terminal line is T+0 (no time left to measure)', () => {
    const graph = mockRiskGraph(request({ trade_ids: ['no-such-trade'], dates: ['today'] }))
    expect(graph.pop).toBeNull()
  })
})

describe('the terminal line', () => {
  it('is the canonical (earliest) expiration line when several are drawn', () => {
    const graph = mockRiskGraph(
      request({
        trade_ids: ['oct-strangle-2026-08-24', 'nov-condor-2026-08-26'],
        dates: ['today', `expiry:${NOV}`, `expiry:${OCT}`],
      }),
    )
    expect(graph.terminal).toEqual({ key: 'expiry', date: OCT, kind: 'expiry', drawn: true })
    const canonical = graph.lines.find((line) => line.key === 'expiry')!
    expect(graph.max_loss_on_range).toBe(Math.round(Math.min(...canonical.pnl) * 100) / 100)
  })

  it('is the front expiration, measured but not drawn, when the dates hold none', () => {
    const graph = mockRiskGraph(request({ dates: ['today', '+10d'] }))
    expect(graph.lines.some((line) => line.kind === 'expiry')).toBe(false)
    expect(graph.terminal).toEqual({ key: 'expiry', date: OCT, kind: 'expiry', drawn: false })
    // The readouts describe that expiration, not the T+10 line.
    const withExpiry = mockRiskGraph(request({ dates: ['today', '+10d', `expiry:${OCT}`] }))
    expect(graph.max_profit_on_range).toBe(withExpiry.max_profit_on_range)
    expect(graph.max_loss_on_range).toBe(withExpiry.max_loss_on_range)
    expect(graph.pop).toBe(withExpiry.pop)
    expect(graph.breakevens.expiry).toEqual(withExpiry.breakevens.expiry)
  })
})

describe('past expiry: specs', () => {
  it('are dropped with a graph_date_dropped warning', () => {
    const graph = mockRiskGraph(request({ dates: ['today', 'expiry:2026-08-21', `expiry:${OCT}`] }))
    expect(graph.lines.map((line) => line.date)).toEqual([MOCK_TODAY, OCT])
    const dropped = graph.warnings.filter((w) => w.code === 'graph_date_dropped')
    expect(dropped).toHaveLength(1)
    expect(dropped[0]!.message).toMatch(/expiry:2026-08-21/)
  })

  it('leave the preset when nothing else is left', () => {
    const graph = mockRiskGraph(request({ dates: ['expiry:2026-08-21'] }))
    expect(graph.lines.map((line) => line.date)).toEqual(mockRiskGraph(request()).lines.map((line) => line.date))
  })
})

describe('the what-if date', () => {
  const whatIf = (date: string): WhatIfRequest => ({
    trade_ids: ['oct-strangle-2026-08-24'],
    price: 7750,
    date,
    vol_shift: 0,
    include_realized: true,
    model_legs: [],
  })

  it('is refused before the valuation date (422 out-of-range)', () => {
    expect(() => mockWhatIf(whatIf('2026-08-30'))).toThrow(MockProblem)
    try {
      mockWhatIf(whatIf('2026-08-30'))
    } catch (error) {
      expect((error as MockProblem).problem.status).toBe(422)
    }
    expect(() => mockWhatIf(whatIf(MOCK_TODAY))).not.toThrow()
  })

  it('on an expiry prices that expiration: the expiry line value at that price', () => {
    const graph = mockRiskGraph(request({ dates: [`expiry:${OCT}`], price_range: { min: 7000, max: 8500 }, points: 7 }))
    const at = graph.prices.indexOf(7750)
    expect(at).toBeGreaterThanOrEqual(0)
    expect(mockWhatIf(whatIf(OCT)).pnl).toBeCloseTo(graph.lines[0]!.pnl[at]!, 0)
  })
})
