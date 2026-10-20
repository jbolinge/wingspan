/**
 * The greek strip over the server's non-uniform grid (D6): `points` uniform
 * samples with spot and every in-range strike inserted in order. Columns are
 * sampled evenly in price, tile the axis with no gap or overlap, and are as
 * wide as the local spacing.
 */

import { describe, expect, it } from 'vitest'
import type { RiskGraphResponse, RiskGreeks, RiskLine } from '../api/types'
import { buildCursorOverlay, buildStrip, columnPitch, sampleColumns, stripColumns } from './greekStrip'
import { chartPalette } from './palette'

const palette = chartPalette('dark')

/** 121 uniform samples over [6000, 7200], plus a dense cluster of strikes
 *  and a spot that is no uniform node. */
function nonUniformGrid(): number[] {
  const uniform = Array.from({ length: 121 }, (_, i) => 6000 + i * 10)
  const inserted = [6601, 6602, 6603, 6604, 6605, 6606, 6607, 6608, 6609, 6612.34]
  return [...new Set([...uniform, ...inserted])].sort((a, b) => a - b)
}

describe('sampleColumns on a non-uniform grid', () => {
  it('spreads the columns evenly in price, not in index', () => {
    const grid = nonUniformGrid()
    const picked = sampleColumns(grid, null, 13).map((i) => grid[i]!)
    // Targets every 100 points: a strike cluster must not pull them in.
    expect(picked).toEqual([6000, 6100, 6200, 6300, 6400, 6500, 6600, 6700, 6800, 6900, 7000, 7100, 7200])
  })

  it('samples a uniform grid exactly as index rounding did', () => {
    const grid = Array.from({ length: 6 }, (_, i) => 6000 + i * 100)
    expect(sampleColumns(grid, null, 4)).toEqual([0, 2, 3, 5])
    expect(sampleColumns(grid, null, 3)).toEqual([0, 3, 5])
  })

  it('keeps every node when there are no more of them than columns', () => {
    const grid = [6000, 6001, 6002, 6300]
    expect(sampleColumns(grid, null, 20)).toEqual([0, 1, 2, 3])
  })
})

describe('stripColumns on a non-uniform grid', () => {
  it('tiles the axis with no gap and no overlap, every sample inside its column', () => {
    const grid = nonUniformGrid()
    for (const window of [null, { min: 6550, max: 6680 }, { min: 6590, max: 6620 }]) {
      const cols = stripColumns(grid, window, 20)
      expect(cols.length).toBeGreaterThan(1)
      for (let k = 0; k + 1 < cols.length; k += 1) {
        expect(cols[k]!.to).toBe(cols[k + 1]!.from)
        expect(cols[k]!.to).toBeGreaterThan(cols[k]!.from)
      }
      for (const col of cols) {
        expect(col.price).toBe(grid[col.index])
        expect(col.from).toBeLessThan(col.price)
        expect(col.to).toBeGreaterThan(col.price)
      }
    }
  })

  it('makes a column as wide as the local spacing inside a dense cluster', () => {
    const grid = nonUniformGrid()
    // Zoomed onto the cluster: every node is a column, 1 point apart.
    const cols = stripColumns(grid, { min: 6602.5, max: 6607.5 }, 20)
    expect(cols.map((c) => c.price)).toEqual([6603, 6604, 6605, 6606, 6607])
    for (const col of cols.slice(1, -1)) expect(col.to - col.from).toBeCloseTo(1, 9)
  })

  it('falls back to the local step, not the mean step, for a lone column', () => {
    const grid = [6000, 6100, 6200, 6201, 6202, 6300]
    expect(columnPitch(grid, { min: 6200.8, max: 6201.2 }, 5)).toBeCloseTo(1, 9)
  })
})

describe('buildStrip on a non-uniform grid', () => {
  const grid = nonUniformGrid()
  const ramp = grid.map((p) => p - 6600)
  const t0: RiskLine = { date: '2026-08-31', kind: 't0', key: 't0', label: 'T+0', pnl: ramp }
  const greeks: RiskGreeks = {
    key: 't0',
    delta: ramp,
    gamma: ramp.map((v) => v / 1000),
    theta: ramp,
    vega: ramp,
    weighted_vega: ramp,
    weighted_vega_reference: '30d',
  }
  const data: Pick<RiskGraphResponse, 'prices' | 'lines' | 't0_greeks'> = {
    prices: grid,
    lines: [t0],
    t0_greeks: greeks,
  }

  it('draws every row as contiguous bars, without gaps or overlaps', () => {
    const strip = buildStrip({ data, window: null, palette, left: 68, right: 20, xMin: 6000, xMax: 7200 })
    const custom = strip.series.filter((s) => s.type === 'custom')
    expect(custom).toHaveLength(6)
    for (const series of custom) {
      const xs = (series.data as { value: [number, number] }[]).map((d) => d.value[0])
      // Evenly spread in price: the gaps between sampled prices never differ
      // by more than one local grid step (10 points).
      const gaps = xs.slice(1).map((x, i) => x - xs[i]!)
      expect(Math.max(...gaps) - Math.min(...gaps)).toBeLessThanOrEqual(10)
    }
  })

  it('reads the cursor at the exact spot node', () => {
    const spotIndex = grid.indexOf(6612.34)
    const overlay = buildCursorOverlay({ data, index: spotIndex, window: null, palette })
    const pnl = (overlay.series as { id: string; data: [number, number][] }[]).find((s) => s.id === 'cursor-pnl')!
    expect(pnl.data).toEqual([[6612.34, 0]])
  })
})
