import { describe, expect, it } from 'vitest'
import type { RiskGraphResponse, RiskLine } from '../api/types'
import { buildSlices, indexAt, nearestPriceIndex, yExtent } from './readout'

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
  t0_greeks: null,
  per_trade: [],
  breakevens: { expiry: [6200] },
  max_profit_on_range: 800,
  max_loss_on_range: -800,
  pop: 0.5,
  sigma_bands: { one: [6100, 6300], two: [6000, 6400] },
  front_atm_iv: 0.16,
  warnings: [],
}

describe('nearestPriceIndex', () => {
  it('snaps to the closest grid point', () => {
    expect(nearestPriceIndex(prices, 6249)).toBe(2)
    expect(nearestPriceIndex(prices, 6251)).toBe(3)
    expect(nearestPriceIndex(prices, 1)).toBe(0)
    expect(nearestPriceIndex([], 100)).toBe(0)
  })
})

describe('indexAt', () => {
  it('snaps to a grid point, within half a step of the ends', () => {
    expect(indexAt(prices, 6249)).toBe(2)
    expect(indexAt(prices, 6000)).toBe(0)
    expect(indexAt(prices, 6400)).toBe(4)
    // ECharts reports the sample it snapped to, so half a step of slack.
    expect(indexAt(prices, 5960)).toBe(0)
    expect(indexAt(prices, 6440)).toBe(4)
  })

  /** The window can now be panned clean off the fetched grid; the readout has
   *  to say so rather than clamp to whichever end sample is nearest. */
  it('is null for a price off the grid entirely', () => {
    expect(indexAt(prices, 1)).toBeNull()
    expect(indexAt(prices, 96_000)).toBeNull()
    expect(indexAt(prices, 5900)).toBeNull()
    expect(indexAt(prices, 6500)).toBeNull()
  })

  it('is null without a grid, or for a nonsense price', () => {
    expect(indexAt([], 6200)).toBeNull()
    expect(indexAt(prices, Number.NaN)).toBeNull()
    expect(indexAt([6200], 6200)).toBe(0)
    expect(indexAt([6200], 6300)).toBeNull()
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
