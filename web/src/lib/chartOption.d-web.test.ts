/**
 * The chart's names come from the server keys (D1), its breakeven markers
 * are the canonical expiration line's only (D2), and its x axis is a value
 * axis over a non-uniform grid (D6).
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { RiskGraphResponse, RiskLine } from '../api/types'
import { buildRiskChartOption, canonicalBreakevens } from './chartOption'
import { chartPalette } from './palette'

const palette = chartPalette('dark')
// A non-uniform grid: uniform samples plus spot (6237.5) and a strike (6250).
const prices = [6000, 6100, 6200, 6237.5, 6250, 6300, 6400]

const lines: RiskLine[] = [
  { date: '2026-08-31', kind: 't0', key: 't0', label: 'T+0', pnl: [-100, -50, -10, 0, 5, 30, 100] },
  { date: '2026-09-15', kind: 'tplus', key: 't+15', label: 'T+15', pnl: [-120, -60, -20, -5, 0, 20, 90] },
  { date: '2026-12-18', kind: 'expiry', key: 'expiry', label: 'Exp 2026-12-18', pnl: [-200, -100, -30, 0, 10, 50, 200] },
  {
    date: '2027-01-15',
    kind: 'expiry',
    key: 'expiry:2027-01-15',
    label: 'Exp 2027-01-15',
    pnl: [-210, -110, -40, -10, 0, 40, 190],
  },
]

const data: RiskGraphResponse = {
  as_of: '2026-08-31T20:30:00Z',
  spot: 6237.5,
  prices,
  lines,
  t0_greeks: null,
  per_trade: [],
  breakevens: { t0: [6237.5], 't+15': [6250], expiry: [6237.5], 'expiry:2027-01-15': [6250] },
  max_profit_on_range: 200,
  max_loss_on_range: -200,
  pop: 0.5,
  sigma_bands: { one: [6100, 6300], two: [6050, 6350] },
  front_atm_iv: 0.16,
  warnings: [],
}

function build(visibleLines: RiskLine[] = lines) {
  return buildRiskChartOption({
    data,
    visibleLines,
    perTrade: [],
    palette,
    showSigmaBands: false,
    showBreakevens: true,
    yLock: null,
    xWindow: null,
  })
}

type Series = { name?: string; type?: string; data?: unknown[] }
const seriesOf = (option: ReturnType<typeof build>): Series[] => option.series as Series[]

describe('chart line names', () => {
  // Local clock already on the next day (New York is still on Aug 31).
  beforeEach(() => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date(2026, 8, 1, 0, 30))
  })
  afterEach(() => vi.useRealTimers())

  it('are the server T+N, not one recomputed from the browser day', () => {
    const names = seriesOf(build()).map((s) => s.name)
    expect(names).toContain('T+15')
    expect(names).not.toContain('T+14')
    expect(names).toContain('T+0')
  })
})

describe('breakeven markers', () => {
  it('are the canonical expiration line breakevens only', () => {
    const scatter = seriesOf(build()).find((s) => s.name === 'breakeven')
    expect(scatter?.data).toEqual([[6237.5, 0]])
  })

  it('are gone while the canonical expiration line is hidden', () => {
    const visible = lines.filter((line) => line.key !== 'expiry')
    expect(seriesOf(build(visible)).some((s) => s.name === 'breakeven')).toBe(false)
    expect(canonicalBreakevens(data, visible)).toEqual([])
  })

  it('never carry a hidden line or a T+x crossing', () => {
    expect(canonicalBreakevens(data, lines)).toEqual([6237.5])
  })
})

describe('x axis', () => {
  it('is a value axis plotting every node at its own price', () => {
    const option = build()
    const axis = (option.xAxis as { type: string }[])[0]!
    expect(axis.type).toBe('value')
    const t0 = seriesOf(option).find((s) => s.name === 'T+0')!
    expect((t0.data as [number, number][]).map(([x]) => x)).toEqual(prices)
  })
})
