import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { RiskGraphResponse, RiskGreeks, RiskLine } from '../api/types'
import { buildRiskChartOption, lineLabel, perTradeOverlayCurves } from './chartOption'
import { cursorSeriesId, stripHeight, STRIP_ROWS } from './greekStrip'
import { chartPalette, mixHex, tplusColor, tplusDash } from './palette'

const palette = chartPalette('dark')

const lines: RiskLine[] = [
  { date: '2026-08-31', kind: 't0', key: 't0', label: 'T+0', pnl: [-100, 0, 100] },
  { date: '2026-09-15', kind: 'tplus', key: 't+15', label: 'T+15', pnl: [-120, -10, 90] },
  { date: '2026-10-01', kind: 'tplus', key: 't+31', label: 'T+31', pnl: [-140, -20, 80] },
  { date: '2027-02-19', kind: 'expiry', key: 'expiry', label: 'Exp 2027-02-19', pnl: [-200, 0, 200] },
]

// Three prices, mixed signs in every row, so the strip's symmetric-extent and
// sign-based bar styling both have something to chew on.
const t0Greeks: RiskGreeks = {
  key: 't0',
  delta: [-30, 5, 40],
  gamma: [-2, 3, -1],
  theta: [-15, -5, 10],
  vega: [-8, 12, -20],
  weighted_vega: [-5, 9, -14],
  weighted_vega_reference: 'front_expiry (2026-10-16)',
}

const data: RiskGraphResponse = {
  as_of: '2026-08-31T14:30:00Z',
  spot: 6200,
  prices: [6000, 6200, 6400],
  lines,
  t0_greeks: t0Greeks,
  per_trade: [{ trade_id: 'm1', date: '2026-08-31', key: 't0', pnl: [-50, 0, 50] }],
  breakevens: { expiry: [6200] },
  max_profit_on_range: 200,
  max_loss_on_range: -200,
  pop: 0.5,
  sigma_bands: { one: [6100, 6300], two: [6050, 6350] },
  front_atm_iv: 0.16,
  warnings: [],
}

const nullGreeks: RiskGraphResponse = { ...data, t0_greeks: null }

function build(overrides: Partial<Parameters<typeof buildRiskChartOption>[0]> = {}) {
  return buildRiskChartOption({
    data,
    visibleLines: lines,
    perTrade: [],
    palette,
    showSigmaBands: true,
    showBreakevens: true,
    yLock: null,
    xWindow: null,
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

function xAxis0(o: ReturnType<typeof build>) {
  return (o.xAxis as unknown[])[0] as { min: number; max: number }
}

function yAxis0(o: ReturnType<typeof build>) {
  return (o.yAxis as unknown[])[0] as {
    min?: number
    max?: number
    axisLabel: Record<string, unknown>
  }
}

describe('buildRiskChartOption', () => {
  // Line labels are keyed off the local calendar day (todayIso()), not the
  // fixture's `as_of`; pin the clock to what `as_of` used to stand in for so
  // the T+n labels below stay deterministic regardless of the real date.
  beforeEach(() => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date(2026, 7, 31, 12, 0))
  })

  afterEach(() => {
    vi.useRealTimers()
  })

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
    const option = build({ perTrade: [{ tradeId: 'm1', name: 'Alpha', pnl: [-50, 0, 50] }] })
    const overlay = seriesOf(option).find((s) => s.name === 'Alpha (T+0)')
    expect(lineStyleOf(overlay).width).toBe(1)
  })

  it('honours a locked y-axis and auto-scales otherwise', () => {
    expect(yAxis0(build({ yLock: { min: -500, max: 500 } }))).toMatchObject({ min: -500, max: 500 })
    expect(yAxis0(build())).not.toHaveProperty('min')
  })

  it('drops hidden lines entirely', () => {
    const option = build({ visibleLines: lines.filter((l) => l.kind !== 'tplus') })
    expect(seriesOf(option).filter((s) => Array.isArray(lineStyleOf(s).type))).toHaveLength(0)
  })

  it('pins the x axis to the returned price grid', () => {
    expect(xAxis0(build())).toMatchObject({ min: 6000, max: 6400 })
  })

  it('pins the x axis to the zoom window when one is set', () => {
    expect(xAxis0(build({ xWindow: { min: 6150, max: 6250 } }))).toMatchObject({
      min: 6150,
      max: 6250,
    })
  })

  /** Zoomed in, the vertical detail of the visible slice is the whole point:
   *  the y axis follows the curves on screen (plus the clipped edge
   *  segments), instead of staying scaled to the whole computed range. */
  it('rescales the y axis to the curves inside the window', () => {
    const option = build({ xWindow: { min: 6190, max: 6250 } })
    const yAxis = yAxis0(option)
    expect(yAxis.max).toBeGreaterThan(200)
    expect(yAxis.max).toBeLessThan(200 * 1.5)
    expect(yAxis.min).toBeLessThan(-100)
  })

  it('measures the per-trade overlay too, when it is drawn', () => {
    const wide = build({
      xWindow: { min: 6190, max: 6210 },
      visibleLines: lines.filter((l) => l.kind === 't0'),
      perTrade: [{ tradeId: 'm1', name: 'Alpha', pnl: [-9000, 0, 9000] }],
    })
    expect(yAxis0(wide).min).toBeLessThan(-1000)
  })

  it('keeps a locked y-axis frozen while zoomed', () => {
    const option = build({ xWindow: { min: 6190, max: 6210 }, yLock: { min: -500, max: 500 } })
    expect(yAxis0(option)).toMatchObject({ min: -500, max: 500 })
  })

  /** Without a window the option must be exactly what it was before zooming
   *  existed — the screenshot baseline and every test above ride on it. */
  it('leaves the y axis auto-scaled when there is no window', () => {
    expect(yAxis0(build())).not.toHaveProperty('min')
  })

  /** A window-derived bound is the padded end of whatever curve happened to
   *  be on screen, so ECharts draws it as an extra boundary tick right on top
   *  of the round one beside it. */
  it('drops the boundary labels when the y bounds come from the window', () => {
    const axisLabel = (option: ReturnType<typeof build>) => yAxis0(option).axisLabel
    expect(axisLabel(build({ xWindow: { min: 6190, max: 6210 } }))).toMatchObject({
      showMinLabel: false,
      showMaxLabel: false,
    })
    // A locked extent is a number the trader froze: it keeps its labels, and
    // so does the un-windowed default, which ECharts rounds itself.
    expect(axisLabel(build({ yLock: { min: -500, max: 500 } }))).not.toHaveProperty('showMinLabel')
    expect(axisLabel(build())).not.toHaveProperty('showMinLabel')
  })

  /** Panned clean past what has been fetched there is nothing on screen to
   *  measure: the axis goes back to ECharts' own scaling rather than being
   *  pinned to a stale extent — and the build must not throw on the way. */
  it('leaves the y axis auto-scaled when the window is off the data entirely', () => {
    const option = build({ xWindow: { min: 9000, max: 9400 } })
    expect(yAxis0(option)).not.toHaveProperty('min')
    expect(xAxis0(option)).toMatchObject({ min: 9000, max: 9400 })
    expect(yAxis0(option).axisLabel).not.toHaveProperty('showMinLabel')
  })

  it('keeps breakeven markers filtered to the full range, not the window', () => {
    const option = build({ xWindow: { min: 6000, max: 6100 } })
    expect(seriesOf(option).find((s) => s.name === 'breakeven')?.data).toEqual([[6200, 0]])
  })

  /** In the evening the server's UTC `as_of` has already rolled to tomorrow
   *  while the local trading day has not; labelling off that UTC date used
   *  to make a real T+1 line collide with "T+0". */
  it('labels lines off the local calendar day, not the UTC as_of date', () => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date(2026, 7, 31, 20, 0))
    const eveningLines: RiskLine[] = [
      { date: '2026-08-31', kind: 't0', key: 't0', label: 'T+0', pnl: [-100, 0, 100] },
      { date: '2026-09-01', kind: 'tplus', key: 't+1', label: 'T+1', pnl: [-90, 10, 110] },
    ]
    const series = seriesOf(
      build({
        data: { ...data, as_of: '2026-09-01T02:00:00Z' },
        visibleLines: eveningLines,
      }),
    )
    expect(series.some((s) => s.name === 'T+1')).toBe(true)
    expect(series.filter((s) => s.name === 'T+0')).toHaveLength(1)
  })
})

describe('buildRiskChartOption — greek strip', () => {
  beforeEach(() => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date(2026, 7, 31, 12, 0))
  })

  afterEach(() => {
    vi.useRealTimers()
  })

  function grids(option: ReturnType<typeof build>) {
    return option.grid as { left: number; right: number; bottom: number; height?: number }[]
  }

  function xAxes(option: ReturnType<typeof build>) {
    return option.xAxis as {
      min: number
      max: number
      show?: boolean
      gridIndex?: number
      containShape?: boolean
      axisPointer?: { type?: string; snap?: boolean; label?: { show?: boolean } }
    }[]
  }

  function yAxes(option: ReturnType<typeof build>) {
    return option.yAxis as {
      min?: number
      max?: number
      show?: boolean
      gridIndex?: number
      axisPointer?: { show?: boolean }
    }[]
  }

  it('adds one grid per strip row, with grid 0 sized to the strip height', () => {
    const option = build()
    expect(grids(option)).toHaveLength(1 + STRIP_ROWS.length)
    expect(grids(option)[0]?.bottom).toBe(stripHeight())
  })

  it('adds one hidden x/y axis per strip row, pinned to grid 0s window', () => {
    const option = build()
    expect(xAxes(option)).toHaveLength(1 + STRIP_ROWS.length)
    expect(yAxes(option)).toHaveLength(1 + STRIP_ROWS.length)
    const base = xAxis0(option)
    for (const axis of xAxes(option).slice(1)) {
      expect(axis.show).toBe(false)
      expect(axis).toMatchObject({ min: base.min, max: base.max })
    }
  })

  it('binds each strip axis to its own grid, not grid 0', () => {
    // An axis with no gridIndex defaults to grid 0 in ECharts, which would
    // draw every strip row on top of the P&L curves instead of in its own
    // band beneath them.
    const option = build()
    xAxes(option)
      .slice(1)
      .forEach((axis, i) => expect(axis.gridIndex).toBe(i + 1))
    yAxes(option)
      .slice(1)
      .forEach((axis, i) => expect(axis.gridIndex).toBe(i + 1))
  })

  it('disables containShape on the strip x axes so bars share grid 0s pixel columns', () => {
    // ECharts widens a numeric axis' value-to-pixel mapping by half a band
    // width whenever the axis carries a series whose shapes could overflow
    // the plot (bar, candlestick, boxplot). The strip rows are custom series
    // and ask for no widening, so this is a pinned guarantee rather than an
    // active correction: grid 0's axis is never widened, and a widened strip
    // axis would map the same price to a different pixel than the curve above.
    const option = build()
    for (const axis of xAxes(option).slice(1)) {
      expect(axis.containShape).toBe(false)
    }
  })

  it('links every grids crosshair', () => {
    const option = build() as { axisPointer?: { link?: { xAxisIndex: string }[] } }
    expect(option.axisPointer?.link).toEqual([{ xAxisIndex: 'all' }])
  })

  it('stops the strip axes snapping the linked crosshair to a sampled bar', () => {
    const option = build()
    // A strip axis' only series is the ~20 sampled bars, so a snapping axis
    // pointer there would quantise the cursor to bar columns and `link` would
    // hand that price back to the P&L plot, the inset and the cursor boxes.
    for (const axis of xAxes(option).slice(1)) {
      expect(axis.axisPointer?.snap).toBe(false)
    }
    // Grid 0 keeps the tooltip's own snap: its samples are the curves'.
    expect(xAxes(option)[0]?.axisPointer).toBeUndefined()
  })

  it('draws no pointer line inside the strip, so the cursor boxes stay legible', () => {
    // Each grid draws its *own* pointer line across its *own* rect, at the
    // axis-pointer model's z 50 — above the z-20 `cursor-*` boxes and above
    // their 0.92-alpha background — so the six strip segments struck through
    // the very numbers the boxes exist to make readable. `type: 'none'`
    // suppresses only the line element. `show: false` would not do:
    // `saveTooltipAxisInfo` returns early on it, dropping the axis out of
    // `axesInfo` and out of the `axisPointer.link` group, so a hover over a
    // strip row would trigger no axis at all and the cursor column would die.
    const option = build()
    for (const axis of xAxes(option).slice(1)) {
      expect(axis.axisPointer?.type).toBe('none')
    }
    // Grid 0's crosshair is untouched — it is the one the trader reads.
    expect(xAxes(option)[0]?.axisPointer).toBeUndefined()
  })

  it('hides the linked axis pointer label on every strip axis', () => {
    // `axisPointer.link` puts all seven x axes (and, via the cross tooltip,
    // all seven y axes) into one group; without an explicit label:false the
    // cross-type tooltip pointer draws its price/value chip on every axis in
    // the group, so a hover would stack six duplicate price chips (and six
    // meaningless strip-y-value chips) down the strip.
    const option = build()
    for (const axis of xAxes(option).slice(1)) {
      expect(axis.axisPointer?.label?.show).toBe(false)
    }
    for (const axis of yAxes(option).slice(1)) {
      expect(axis.axisPointer?.show).toBe(false)
    }
  })

  it('draws one strip-<row> custom series per row, on the matching grid index', () => {
    const option = build()
    STRIP_ROWS.forEach((row, i) => {
      const series = seriesOf(option).find((s) => s.name === `strip-${row.key}`)
      expect(series).toBeDefined()
      const found = series!
      expect(found.type).toBe('custom')
      expect(found.xAxisIndex).toBe(i + 1)
      expect(found.yAxisIndex).toBe(i + 1)
      expect((found.data as unknown[]).length).toBeGreaterThan(0)
      // Only sampled columns are drawn — never more bars than price points.
      expect((found.data as unknown[]).length).toBeLessThanOrEqual(data.prices.length)
    })
  })

  it('gives every strip row a y axis symmetric about zero', () => {
    for (const axis of yAxes(build()).slice(1)) {
      expect(axis.min).toBe(-(axis.max as number))
    }
  })

  it('carries all six cursor-* placeholder series with empty data', () => {
    const series = seriesOf(build())
    for (const row of STRIP_ROWS) {
      const cursor = series.find((s) => s.id === cursorSeriesId(row.key))
      expect(cursor).toBeDefined()
      expect(cursor?.data).toEqual([])
    }
  })

  it('draws the strip empty with a note when there is no T+0 greek data', () => {
    const option = build({ data: nullGreeks })
    for (const row of STRIP_ROWS) {
      const series = seriesOf(option).find((s) => s.name === `strip-${row.key}`)
      expect(series?.data).toEqual([])
    }
    const titles = option.title as { text?: string }[]
    expect(titles.some((t) => t.text === 'add T+0 to the dates to see greeks')).toBe(true)
  })

  it('leaves a column blank rather than drawing a null greek as a zero bar', () => {
    const holed: RiskGraphResponse = {
      ...data,
      t0_greeks: { ...t0Greeks, vega: [-8, Number.NaN, -20] },
    }
    const option = build({ data: holed })
    const series = (
      option.series as { name?: string; data?: { value: [number, number] }[] }[]
    ).find((s) => s.name === 'strip-vega')
    // Two of the three samples survive; the unknown one is not claimed to be 0.
    expect(series?.data).toHaveLength(2)
    expect(series?.data?.map((d) => d.value[1])).toEqual([-8, -20])
  })

  it('builds without throwing when the window sits entirely off the data', () => {
    expect(() => build({ xWindow: { min: 9000, max: 9400 } })).not.toThrow()
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
