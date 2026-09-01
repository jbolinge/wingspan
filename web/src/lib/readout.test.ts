import { describe, expect, it } from 'vitest'
import type { RiskGraphResponse, RiskLine } from '../api/types'
import { buildSlices, nearestPriceIndex, readoutAt, yExtent } from './readout'

const prices = [6000, 6100, 6200, 6300, 6400]

const lines: RiskLine[] = [
  { date: '2026-08-31', kind: 't0', key: 't0', label: 'T+0', pnl: [-400, -200, 0, 200, 400] },
  { date: '2026-09-30', kind: 'tplus', key: 't+30', label: 'T+30', pnl: [-500, -300, -100, 100, 300] },
  { date: '2026-10-16', kind: 'expiry', key: 'expiry', label: 'Exp 2026-10-16', pnl: [-800, -400, 0, 400, 800] },
]

const data: RiskGraphResponse = {
  as_of: '2026-08-31T14:30:00Z',
  spot: 6200,
  prices,
  lines,
  per_trade: [],
  breakevens: { expiry: [6200] },
  max_profit_on_range: 800,
  max_loss_on_range: -800,
  pop: 0.5,
  sigma_bands: { one: [6100, 6300], two: [6000, 6400] },
  front_atm_iv: 0.16,
  warnings: [],
}

describe('readoutAt', () => {
  it('reads every line at the cursor, oldest date first', () => {
    const readout = readoutAt(data, 2)
    expect(readout?.price).toBe(6200)
    expect(readout?.rows.map((r) => r.pnl)).toEqual([0, -100, 0])
  })

  it('derives delta as the local slope in dollars per index point', () => {
    // t0 rises 400 over 200 points around the cursor -> 2 $/pt.
    expect(readoutAt(data, 2)?.rows[0]?.delta).toBeCloseTo(2, 10)
  })

  it('derives theta from the gap to the next line, per calendar day', () => {
    // t0 -> tplus is 30 days and -100 of P&L.
    const theta = readoutAt(data, 2)?.rows[0]?.theta
    expect(theta).toBeCloseTo(-100 / 30, 6)
  })

  it('has no theta on the last line out', () => {
    expect(readoutAt(data, 2)?.rows[2]?.theta).toBeNull()
  })

  it('clamps an out-of-range index instead of returning holes', () => {
    expect(readoutAt(data, 99)?.index).toBe(4)
    expect(readoutAt(data, -3)?.index).toBe(0)
  })

  it('reads exactly the lines it is handed, and measures theta between them', () => {
    const visible = lines.filter((l) => l.kind !== 'tplus')
    const readout = readoutAt({ prices, lines: visible }, 0)
    expect(readout?.rows.map((r) => r.key)).toEqual(['t0', 'expiry'])
    // Θ is the drop to the next *visible* line: t0 -> expiry is 46 days and
    // -400 of P&L, not the -100 over 30 days of the hidden T+30 line.
    expect(readout?.rows[0]?.theta).toBeCloseTo(-400 / 46, 6)
    expect(readoutAt(data, 0)?.rows[0]?.theta).toBeCloseTo(-100 / 30, 6)
  })

  it('carries the line key so callers can label rows where dates repeat', () => {
    expect(readoutAt(data, 2)?.rows.map((r) => r.key)).toEqual(['t0', 't+30', 'expiry'])
  })

  it('returns null when there is no grid', () => {
    expect(readoutAt({ prices: [], lines: [] }, 0)).toBeNull()
  })
})

describe('nearestPriceIndex', () => {
  it('snaps to the closest grid point', () => {
    expect(nearestPriceIndex(prices, 6249)).toBe(2)
    expect(nearestPriceIndex(prices, 6251)).toBe(3)
    expect(nearestPriceIndex(prices, 1)).toBe(0)
    expect(nearestPriceIndex([], 100)).toBe(0)
  })
})

describe('buildSlices', () => {
  it('includes spot, the offsets inside the range and the breakevens', () => {
    const rows = buildSlices(data, lines, { offsets: [-0.02, 0, 0.02] })
    expect(rows.map((r) => r.label)).toContain('spot')
    expect(rows.every((r) => r.values).valueOf()).toBe(true)
  })

  it('drops offsets that fall outside the plotted range', () => {
    const rows = buildSlices(data, lines, { offsets: [-0.9, 0, 0.9], includeBreakevens: false })
    expect(rows).toHaveLength(1)
    expect(rows[0]?.label).toBe('spot')
  })

  it('never emits two rows for the same grid point', () => {
    const rows = buildSlices(data, lines, { offsets: [0, 0.0001] })
    const indices = rows.map((r) => r.price)
    expect(new Set(indices).size).toBe(indices.length)
  })
})

describe('yExtent', () => {
  it('pads the extent so a curve never touches the frame', () => {
    const extent = yExtent(lines)
    expect(extent?.min).toBeLessThan(-800)
    expect(extent?.max).toBeGreaterThan(800)
  })

  it('is null when there is nothing finite to measure', () => {
    expect(yExtent([])).toBeNull()
  })
})
