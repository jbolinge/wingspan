import { describe, expect, it } from 'vitest'
import type { RiskGraphResponse, RiskLine } from '../api/types'
import { buildRiskChartOption, lineLabel, perTradeOverlayCurves } from './chartOption'
import { chartPalette, mixHex, tplusColor, tplusDash } from './palette'

const palette = chartPalette('dark')

const lines: RiskLine[] = [
  { date: '2026-08-31', kind: 't0', key: 't0', label: 'T+0', pnl: [-100, 0, 100] },
  { date: '2026-09-15', kind: 'tplus', key: 't+15', label: 'T+15', pnl: [-120, -10, 90] },
  { date: '2026-10-01', kind: 'tplus', key: 't+31', label: 'T+31', pnl: [-140, -20, 80] },
  { date: '2027-02-19', kind: 'expiry', key: 'expiry', label: 'Exp 2027-02-19', pnl: [-200, 0, 200] },
]

const data: RiskGraphResponse = {
  as_of: '2026-08-31T14:30:00Z',
  spot: 6200,
  prices: [6000, 6200, 6400],
  lines,
  per_trade: [{ trade_id: 'm1', date: '2026-08-31', key: 't0', pnl: [-50, 0, 50] }],
  breakevens: { expiry: [6200] },
  max_profit_on_range: 200,
  max_loss_on_range: -200,
  pop: 0.5,
  sigma_bands: { one: [6100, 6300], two: [6050, 6350] },
  front_atm_iv: 0.16,
}

function build(overrides: Partial<Parameters<typeof buildRiskChartOption>[0]> = {}) {
  return buildRiskChartOption({
    data,
    visibleLines: lines,
    perTrade: [],
    palette,
    showSigmaBands: true,
    showBreakevens: true,
    yLock: null,
    ...overrides,
  })
}

function seriesOf(option: ReturnType<typeof build>): Record<string, unknown>[] {
  return (option.series ?? []) as unknown as Record<string, unknown>[]
}

function lineStyleOf(
  series: Record<string, unknown> | undefined,
): { width?: number; type?: unknown } {
  return (series?.lineStyle ?? {}) as { width?: number; type?: unknown }
}

describe('buildRiskChartOption', () => {
  it('draws the expiration line thickest and the T+0 line next', () => {
    const series = seriesOf(build())
    const expiry = series.find((s) => s.name === 'EXP Feb 19 2027')
    const t0 = series.find((s) => s.name === 'T+0')
    expect(lineStyleOf(expiry).width).toBe(3)
    expect(lineStyleOf(t0).width).toBe(2.2)
  })

  it('dashes the T+x ladder and leaves T+0 and expiry solid', () => {
    const series = seriesOf(build())
    const tplus = series.filter((s) => Array.isArray(lineStyleOf(s).type))
    expect(tplus).toHaveLength(2)
    expect(lineStyleOf(series.find((s) => s.name === 'T+0')).type).toBeUndefined()
  })

  it('layers the expiration line above the ladder', () => {
    const series = seriesOf(build())
    const expiry = series.find((s) => s.name === 'EXP Feb 19 2027')
    const tplus = series.find((s) => s.name === 'T+15')
    expect(Number(expiry?.z)).toBeGreaterThan(Number(tplus?.z))
  })

  it('always carries the zero line and the spot marker', () => {
    const markers = seriesOf(build()).find((s) => s.name === 'markers')
    const markLine = (markers?.markLine ?? { data: [] }) as { data: { name: string }[] }
    expect(markLine.data.map((d) => d.name)).toEqual(['0', 'spot 6200'])
  })

  it('emits the sigma bands only when asked', () => {
    expect(seriesOf(build()).some((s) => s.name === 'sigma')).toBe(true)
    expect(seriesOf(build({ showSigmaBands: false })).some((s) => s.name === 'sigma')).toBe(false)
  })

  it('plots breakevens on the zero line and can be switched off', () => {
    const scatter = seriesOf(build()).find((s) => s.name === 'breakeven')
    expect(scatter?.data).toEqual([[6200, 0]])
    expect(seriesOf(build({ showBreakevens: false })).some((s) => s.name === 'breakeven')).toBe(false)
  })

  it('skips breakevens that fall outside the plotted range', () => {
    const option = build({
      data: { ...data, breakevens: { expiry: [100] } },
    })
    expect(seriesOf(option).some((s) => s.name === 'breakeven')).toBe(false)
  })

  it('adds one thin series per trade when the overlay is on', () => {
    const option = build({ perTrade: [{ tradeId: 'm1', name: 'M1', pnl: [-50, 0, 50] }] })
    const overlay = seriesOf(option).find((s) => s.name === 'M1 (T+0)')
    expect(lineStyleOf(overlay).width).toBe(1)
  })

  it('honours a locked y-axis and auto-scales otherwise', () => {
    expect(build({ yLock: { min: -500, max: 500 } }).yAxis).toMatchObject({ min: -500, max: 500 })
    expect(build().yAxis).not.toHaveProperty('min')
  })

  it('drops hidden lines entirely', () => {
    const option = build({ visibleLines: lines.filter((l) => l.kind !== 'tplus') })
    expect(seriesOf(option).filter((s) => Array.isArray(lineStyleOf(s).type))).toHaveLength(0)
  })

  it('pins the x axis to the returned price grid', () => {
    expect(build().xAxis).toMatchObject({ min: 6000, max: 6400 })
  })
})

describe('perTradeOverlayCurves', () => {
  /** The API sends one curve per (trade, date); the overlay draws one per
   *  trade, or the legend and the chart repeat every trade once per date. */
  it('keeps one curve per trade — the T+0 one', () => {
    const perTrade = [
      { trade_id: 'm1', date: '2026-08-31', key: 't0', pnl: [1] },
      { trade_id: 'm2', date: '2026-08-31', key: 't0', pnl: [2] },
      { trade_id: 'm1', date: '2026-09-15', key: 't+15', pnl: [3] },
      { trade_id: 'm2', date: '2026-09-15', key: 't+15', pnl: [4] },
      { trade_id: 'm1', date: '2027-02-19', key: 'expiry', pnl: [5] },
      { trade_id: 'm2', date: '2027-02-19', key: 'expiry', pnl: [6] },
    ]
    expect(perTradeOverlayCurves({ ...data, per_trade: perTrade })).toEqual([
      { trade_id: 'm1', date: '2026-08-31', key: 't0', pnl: [1] },
      { trade_id: 'm2', date: '2026-08-31', key: 't0', pnl: [2] },
    ])
  })

  /** On expiration day the T+0 and expiry lines share a date, so matching the
   *  overlay on the date alone picks up both curves. */
  it('matches on the line key, which the date does not disambiguate', () => {
    const sameDay: RiskLine[] = [
      { date: '2026-09-18', kind: 't0', key: 't0', label: 'T+0', pnl: [0] },
      { date: '2026-09-18', kind: 'expiry', key: 'expiry', label: 'Exp 2026-09-18', pnl: [0] },
    ]
    const perTrade = [
      { trade_id: 'm1', date: '2026-09-18', key: 't0', pnl: [1] },
      { trade_id: 'm1', date: '2026-09-18', key: 'expiry', pnl: [2] },
    ]
    expect(perTradeOverlayCurves({ ...data, lines: sameDay, per_trade: perTrade })).toEqual([
      { trade_id: 'm1', date: '2026-09-18', key: 't0', pnl: [1] },
    ])
  })

  it('falls back to the first line when the date list has no T+0', () => {
    const noT0: RiskLine[] = [
      { date: '2026-10-01', kind: 'tplus', key: 't+31', label: 'T+31', pnl: [0] },
      { date: '2027-02-19', kind: 'expiry', key: 'expiry', label: 'Exp 2027-02-19', pnl: [0] },
    ]
    const perTrade = [
      { trade_id: 'm1', date: '2026-10-01', key: 't+31', pnl: [1] },
      { trade_id: 'm1', date: '2027-02-19', key: 'expiry', pnl: [2] },
    ]
    expect(perTradeOverlayCurves({ ...data, lines: noT0, per_trade: perTrade })).toEqual([
      { trade_id: 'm1', date: '2026-10-01', key: 't+31', pnl: [1] },
    ])
  })

  it('is empty without a response', () => {
    expect(perTradeOverlayCurves(undefined)).toEqual([])
    expect(perTradeOverlayCurves({ ...data, lines: [], per_trade: [] })).toEqual([])
  })
})

describe('lineLabel', () => {
  it('names the three line kinds', () => {
    expect(lineLabel(lines[0]!, '2026-08-31')).toBe('T+0')
    expect(lineLabel(lines[1]!, '2026-08-31')).toBe('T+15')
    expect(lineLabel(lines[3]!, '2026-08-31')).toBe('EXP Feb 19 2027')
  })
})

describe('palette', () => {
  it('grades the T+x ramp from the near colour to the far one', () => {
    expect(tplusColor(palette, 0, 3)).toBe(palette.tplusFrom)
    expect(tplusColor(palette, 2, 3)).toBe(palette.tplusTo)
    expect(tplusColor(palette, 0, 1)).toBe(palette.tplusFrom)
  })

  it('lengthens the dash gap for later dates', () => {
    const near = tplusDash(0, 3)
    const far = tplusDash(2, 3)
    expect(far[1]).toBeGreaterThan(near[1]!)
  })

  it('interpolates and clamps hex mixes', () => {
    expect(mixHex('#000000', '#ffffff', 0.5)).toBe('#808080')
    expect(mixHex('#000000', '#ffffff', 5)).toBe('#ffffff')
    expect(mixHex('#000', '#fff', 0)).toBe('#000000')
  })
})
