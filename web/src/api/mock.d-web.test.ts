/**
 * Mock parity with the server (D3, D6, D7): the XSP leg is quoted as the
 * proxy's quote ÷ 10 (the leg row and the chain agree exactly), breakevens
 * come back for every line key with the bare `expiry` the earliest
 * expiration line, the price grid carries spot and every in-range strike,
 * and the what-if honours per-expiry vol shifts.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { mockOptionChain, mockRequest, mockRiskGraph, mockWhatIf } from './mock'
import type { ModelLeg, RiskGraphRequest, TradesResponse, WhatIfRequest } from './types'

beforeEach(() => {
  vi.useFakeTimers({ toFake: ['Date'] })
  vi.setSystemTime(new Date(2026, 7, 31, 11, 0, 0))
})
afterEach(() => vi.useRealTimers())

const DEC = '2026-12-18'
const JAN = '2027-01-15'

function request(overrides: Partial<RiskGraphRequest> = {}): RiskGraphRequest {
  return {
    trade_ids: [],
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

const spxLeg = (strike: number, expiry: string, qty = 1): ModelLeg => ({
  contract: { underlying: 'SPX', root: 'SPXW', expiry, settlement: 'pm', strike, right: 'put', multiplier: 100 },
  qty,
})

const trades = () => (mockRequest('/trades') as TradesResponse).trades

describe('the XSP Hedge leg quote', () => {
  it('is exactly the XSP chain quote: the proxy SPXW quote ÷ 10', () => {
    const leg = trades().find((t) => t.name === 'XSP Hedge')!.legs[0]!
    const quote = mockOptionChain({ expiry: DEC, root: 'SPXW', underlying: 'XSP' }).rows.find(
      (row) => row.strike === 650,
    )!.put!
    expect(leg.mark).toBe(quote.mark)
    expect(leg.bid).toBe(quote.bid)
    expect(leg.ask).toBe(quote.ask)
    const spx = mockOptionChain({ expiry: DEC, root: 'SPXW' }).rows.find((row) => row.strike === 6500)!.put!
    expect(leg.mark).toBe(spx.mark! / 10)
    expect(leg.ask! - leg.bid!).toBeCloseTo((spx.ask! - spx.bid!) / 10, 9)
  })
})

describe('mockRiskGraph', () => {
  it('answers breakevens for every line key', () => {
    const graph = mockRiskGraph(request({ trade_ids: trades().map((t) => t.id) }))
    expect(Object.keys(graph.breakevens).sort()).toEqual(graph.lines.map((line) => line.key).sort())
  })

  it('keys the earliest expiration line `expiry` whatever the request order', () => {
    const graph = mockRiskGraph(
      request({ model_legs: [spxLeg(6500, DEC), spxLeg(6400, JAN)], dates: [`expiry:${JAN}`, 'today', `expiry:${DEC}`] }),
    )
    const expiry = graph.lines.find((line) => line.key === 'expiry')!
    expect(expiry.date).toBe(DEC)
    expect(graph.lines.find((line) => line.key === `expiry:${JAN}`)?.date).toBe(JAN)
    expect(graph.max_loss_on_range).toBe(Math.min(...expiry.pnl))
  })

  it('inserts spot and every in-range proxy strike into a sorted grid', () => {
    const hedge = trades().find((t) => t.name === 'XSP Hedge')!
    const graph = mockRiskGraph(
      request({ trade_ids: [hedge.id], model_legs: [spxLeg(6512.5, DEC)], price_range: { min: 6000, max: 8000 }, points: 21 }),
    )
    expect(graph.prices).toContain(graph.spot)
    expect(graph.prices).toContain(6500) // the XSP 650 put's proxy strike
    expect(graph.prices).toContain(6512.5)
    expect(graph.prices.every((p, i) => i === 0 || p > graph.prices[i - 1]!)).toBe(true)
    expect(graph.prices.length).toBeGreaterThan(21)
    for (const line of graph.lines) expect(line.pnl).toHaveLength(graph.prices.length)
    for (const curve of graph.per_trade) expect(curve.pnl).toHaveLength(graph.prices.length)
    expect(graph.t0_greeks?.delta).toHaveLength(graph.prices.length)
  })

  it('leaves out-of-range strikes off the grid', () => {
    const graph = mockRiskGraph(
      request({ model_legs: [spxLeg(6512.5, DEC)], price_range: { min: 7000, max: 8000 }, points: 11 }),
    )
    expect(graph.prices).not.toContain(6512.5)
  })
})

describe('mockWhatIf', () => {
  it('honours vol_shift_by_expiry on top of the global shift', () => {
    const base: WhatIfRequest = {
      trade_ids: [],
      price: 6800,
      date: '2026-09-15',
      vol_shift: 0.01,
      include_realized: true,
      model_legs: [spxLeg(6500, DEC)],
    }
    const flat = mockWhatIf({ ...base, vol_shift: 0.03 })
    const split = mockWhatIf({ ...base, vol_shift_by_expiry: { [DEC]: 0.02 } })
    expect(split.pnl).toBeCloseTo(flat.pnl, 9)
    expect(split.greeks.vega).toBeCloseTo(flat.greeks.vega, 9)
    // A shift on another expiry leaves this leg alone.
    expect(mockWhatIf({ ...base, vol_shift_by_expiry: { [JAN]: 0.02 } }).pnl).toBeCloseTo(mockWhatIf(base).pnl, 9)
    expect(mockWhatIf({ ...base, vol_shift_by_expiry: null }).pnl).toBeCloseTo(mockWhatIf(base).pnl, 9)
  })
})
