/** Grid helpers and slices over the server's non-uniform grid (D2, D6). */

import { describe, expect, it } from 'vitest'
import type { RiskGraphResponse, RiskLine } from '../api/types'
import { buildSlices, indexAt, nearestPriceIndex } from './readout'

// Uniform 100-point samples with a dense strike cluster and spot inserted.
const prices = [6000, 6100, 6200, 6245, 6250, 6252.37, 6255, 6260, 6300, 6400]

describe('nearestPriceIndex on a non-uniform grid', () => {
  it('lands on the exact node inside a dense cluster', () => {
    expect(nearestPriceIndex(prices, 6252.37)).toBe(5)
    expect(nearestPriceIndex(prices, 6253)).toBe(5)
    expect(nearestPriceIndex(prices, 6254)).toBe(6)
    expect(nearestPriceIndex(prices, 6248)).toBe(4)
  })

  it('still picks the nearest node in the sparse stretch beside the cluster', () => {
    expect(nearestPriceIndex(prices, 6279)).toBe(7)
    expect(nearestPriceIndex(prices, 6281)).toBe(8)
  })
})

describe('indexAt on a non-uniform grid', () => {
  it('hits the exact spot node', () => {
    expect(indexAt(prices, 6252.37)).toBe(5)
    expect(prices[indexAt(prices, 6252.37)!]).toBe(6252.37)
  })

  it('allows half the *local* end step of slack, not the mean step', () => {
    // The end step is 100 on both sides here; the mean step is ~44.
    expect(indexAt(prices, 5951)).toBe(0)
    expect(indexAt(prices, 6449)).toBe(9)
    expect(indexAt(prices, 5949)).toBeNull()
    expect(indexAt(prices, 6451)).toBeNull()
  })
})

describe('buildSlices breakevens', () => {
  const lines: RiskLine[] = [
    { date: '2026-08-31', kind: 't0', key: 't0', label: 'T+0', pnl: prices.map((p) => p - 6100) },
    { date: '2026-12-18', kind: 'expiry', key: 'expiry', label: 'Exp', pnl: prices.map((p) => p - 6300) },
  ]
  const data: Pick<RiskGraphResponse, 'prices' | 'lines' | 'spot' | 'breakevens'> = {
    prices,
    lines,
    spot: 6252.37,
    breakevens: { t0: [6100], expiry: [6300], 't+30': [6200] },
  }

  it('labels each breakeven with its line, and skips lines not given', () => {
    const rows = buildSlices(data, lines, { offsets: [] })
    expect(rows.map((row) => row.label)).toEqual(['BE T+0', 'BE EXP Dec 18 2026'])
    expect(buildSlices(data, [lines[1]!], { offsets: [] }).map((row) => row.label)).toEqual([
      'BE EXP Dec 18 2026',
    ])
  })
})
