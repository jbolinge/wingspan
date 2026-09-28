import { beforeEach, describe, expect, it } from 'vitest'
import { todayIso } from '../lib/dates'
import { nearestPriceIndex } from '../lib/readout'
import { ApiError, apiFetch, setMockMode } from './client'
import {
  MOCK_CSV_PATH,
  MOCK_SPOT,
  mockRequest,
  mockRiskGraph,
  mockWhatIf,
  resetMockTradeSource,
  resolveGraphDates,
} from './mock'
import type {
  ClosedTradesResponse,
  PortfolioStats,
  ReconciliationResponse,
  RiskGraphRequest,
  Trade,
  TradeSourceRequest,
  TradeSourceResponse,
  TradesResponse,
  WarningsResponse,
  WhatIfRequest,
} from './types'

const trades = () => mockRequest('/trades') as TradesResponse

function baseRequest(overrides: Partial<RiskGraphRequest> = {}): RiskGraphRequest {
  return {
    trade_ids: [],
    price_range: null,
    points: 241,
    dates: null,
    vol_shift: 0,
    vol_shift_by_expiry: {},
    spot_override: null,
    include_realized: true,
    ...overrides,
  }
}

describe('GET /trades', () => {
  it('returns open trades with the documented fields populated', () => {
    const response = trades()
    expect(response.trades.length).toBeGreaterThan(0)
    for (const trade of response.trades) {
      expect(trade.status).toBe('open')
      expect(trade.id).toMatch(/^[a-z0-9-]+$/)
      expect(Number.isFinite(trade.pnl_total)).toBe(true)
      expect(Number.isFinite(trade.greeks.delta)).toBe(true)
      expect(trade.dte).not.toBeNull()
      expect(trade.legs.length).toBeGreaterThan(0)
    }
  })

  it('keeps total P&L equal to open P&L plus realised minus fees', () => {
    for (const trade of trades().trades) {
      expect(trade.pnl_total).toBeCloseTo(trade.pnl_open + trade.realized - trade.fees, 1)
    }
  })

  it('carries the demo book’s Oct Strangle: both short legs at their fills', () => {
    const strangle = trades().trades.find((t) => t.name === 'Oct Strangle')
    expect(strangle?.id).toBe('oct-strangle-2026-08-24')
    expect(strangle?.realized).toBe(0)
    expect(strangle?.fees).toBe(2.52)
    expect(strangle?.legs).toHaveLength(2)
    expect(strangle?.legs.map((l) => l.contract.tos_code)).toEqual([
      'SPX261016C8200',
      'SPX261016P7200',
    ])
    expect(strangle?.legs.map((l) => l.avg_cost)).toEqual([56.45, 57.75])
    expect(strangle?.legs.every((l) => l.qty === -1)).toBe(true)
  })

  it('builds OSI symbols with a six-character root and thousandths strike', () => {
    const leg = trades().trades.flatMap((t) => t.legs)[0]
    expect(leg?.contract.osi).toMatch(/^.{6}\d{6}[PC]\d{8}$/)
  })

  it('aggregates the portfolio and names its weighted-vega reference', () => {
    const stats = trades().portfolio_all
    expect(stats.count).toBe(trades().trades.length)
    expect(stats.weighted_vega_reference).toBe('30d')
  })

  it('measures every row against the book reference, so the rows add up to it', () => {
    const response = trades()
    const rows = response.trades.reduce((acc, t) => acc + t.weighted_vega, 0)
    expect(rows).toBeCloseTo(response.portfolio_all.weighted_vega, 6)
  })
})

describe('GET /trades/closed', () => {
  it('reports totals, a win rate and the Aug Hedge quarantine', () => {
    const response = mockRequest('/trades/closed') as ClosedTradesResponse
    expect(response.totals.count).toBe(response.trades.length)
    expect(response.totals.win_rate).toBeGreaterThanOrEqual(0)
    expect(response.totals.win_rate).toBeLessThanOrEqual(1)
    const hedge = response.trades.find((t) => t.name === 'Aug Hedge')
    expect(hedge?.id).toBe('aug-hedge-2026-07-30')
    expect(hedge?.quarantined).toHaveLength(6)
    expect(hedge?.closed_on).toBe('2026-08-27')
    expect(hedge?.pnl_total).toBeCloseTo(-290.04, 2)
  })

  it('carries the demo book’s closed put spread: +440 on 10.08 of fees', () => {
    const response = mockRequest('/trades/closed') as ClosedTradesResponse
    const spread = response.trades.find((t) => t.name === 'Sep Put Spread')
    expect(spread?.id).toBe('sep-put-spread-2026-08-10')
    expect(spread?.realized).toBe(440)
    expect(spread?.fees).toBe(10.08)
    expect(spread?.pnl_total).toBeCloseTo(429.92, 2)
    expect(spread?.closed_on).toBe('2026-08-21')
  })
})

describe('GET /portfolio', () => {
  it('scopes the stats to the requested ids', () => {
    const all = trades()
    const one = all.trades[0] as Trade
    const stats = mockRequest(`/portfolio?ids=${one.id}`) as PortfolioStats
    expect(stats.count).toBe(1)
    expect(stats.pnl_total).toBeCloseTo(one.pnl_total, 1)
  })

  it('is empty for an empty id list', () => {
    expect((mockRequest('/portfolio?ids=') as PortfolioStats).count).toBe(0)
  })
})

describe('POST /risk-graph', () => {
  it('returns a shared x-axis, the ONE line set and the summary readouts', () => {
    const ids = trades().trades.map((t) => t.id)
    const graph = mockRiskGraph(baseRequest({ trade_ids: ids }))
    expect(graph.prices).toHaveLength(241)
    expect(graph.lines.some((l) => l.kind === 't0')).toBe(true)
    expect(graph.lines.some((l) => l.kind === 'expiry')).toBe(true)
    expect(graph.lines.every((l) => l.pnl.length === graph.prices.length)).toBe(true)
    expect(graph.pop).toBeGreaterThanOrEqual(0)
    expect(graph.pop).toBeLessThanOrEqual(1)
    expect(graph.max_profit_on_range).toBeGreaterThanOrEqual(graph.max_loss_on_range)
    expect(graph.sigma_bands.two[0]).toBeLessThan(graph.sigma_bands.one[0])
    expect(graph.sigma_bands.two[1]).toBeGreaterThan(graph.sigma_bands.one[1])
  })

  it('holds the documented invariant: the T+0 line at spot equals total P&L', () => {
    for (const trade of trades().trades) {
      const graph = mockRiskGraph(
        baseRequest({
          trade_ids: [trade.id],
          price_range: { min: MOCK_SPOT - 10, max: MOCK_SPOT + 10 },
          points: 21,
        }),
      )
      const index = nearestPriceIndex(graph.prices, graph.spot)
      const t0 = graph.lines.find((l) => l.kind === 't0')
      expect(t0, `${trade.name} has a T+0 line`).toBeDefined()
      expect(t0?.pnl[index], trade.name).toBeCloseTo(trade.pnl_total, 0)
    }
  })

  it('drops realised P&L and fees when include_realized is false', () => {
    const trade = trades().trades.find((t) => t.realized !== 0) as Trade
    const graph = mockRiskGraph(
      baseRequest({
        trade_ids: [trade.id],
        price_range: { min: MOCK_SPOT - 10, max: MOCK_SPOT + 10 },
        points: 21,
        include_realized: false,
      }),
    )
    const index = nearestPriceIndex(graph.prices, graph.spot)
    expect(graph.lines.find((l) => l.kind === 't0')?.pnl[index]).toBeCloseTo(trade.pnl_open, 0)
  })

  it('honours an absolute price range and the point count', () => {
    const ids = [trades().trades[0]!.id]
    const graph = mockRiskGraph(
      baseRequest({ trade_ids: ids, price_range: { min: 6000, max: 8000 }, points: 101 }),
    )
    expect(graph.prices[0]).toBe(6000)
    expect(graph.prices.at(-1)).toBe(8000)
    expect(graph.prices).toHaveLength(101)
  })

  it('centres a percentage range on the spot override without moving the tape', () => {
    const ids = [trades().trades[0]!.id]
    const graph = mockRiskGraph(
      baseRequest({ trade_ids: ids, price_range: { pct: 0.1 }, spot_override: 7000 }),
    )
    expect(graph.spot).toBe(7000)
    expect(graph.prices[0]).toBeCloseTo(6300, 0)
    expect(graph.prices.at(-1)).toBeCloseTo(7700, 0)
  })

  it('raises a long-vega position when volatility is shifted up', () => {
    // The calendar is the one open trade that is net long vega.
    const calendar = trades().trades.find((t) => t.name === 'Jan Calendar') as Trade
    expect(calendar.greeks.vega).toBeGreaterThan(0)
    const flat = mockRiskGraph(baseRequest({ trade_ids: [calendar.id] }))
    const bumped = mockRiskGraph(baseRequest({ trade_ids: [calendar.id], vol_shift: 0.05 }))
    const index = nearestPriceIndex(flat.prices, flat.spot)
    const flatT0 = flat.lines.find((l) => l.kind === 't0')?.pnl[index] ?? 0
    const bumpedT0 = bumped.lines.find((l) => l.kind === 't0')?.pnl[index] ?? 0
    expect(bumpedT0).toBeGreaterThan(flatT0)
  })

  it('emits one per-trade curve for every selected trade', () => {
    const ids = trades().trades.slice(0, 2).map((t) => t.id)
    const graph = mockRiskGraph(baseRequest({ trade_ids: ids }))
    expect(graph.per_trade.map((p) => p.trade_id)).toEqual(ids)
  })

  it('is empty-safe with no selection', () => {
    const graph = mockRiskGraph(baseRequest())
    expect(graph.lines.length).toBeGreaterThan(0)
    expect(graph.per_trade).toEqual([])
  })
})

describe('resolveGraphDates', () => {
  const legs = [{ expiry: '2026-12-18', strike: 6000, right: 'put' as const, qty: 1, tradeId: 'x' }]

  it('expands the frozen spec vocabulary', () => {
    const dates = resolveGraphDates(['today', '+45d', 'expiry:2026-12-18'], legs)
    expect(dates).toHaveLength(3)
    expect(dates).toContain('2026-12-18')
  })

  it('falls back to the ONE preset, ending on the front expiry', () => {
    const dates = resolveGraphDates(null, legs)
    expect(dates.at(-1)).toBe('2026-12-18')
    expect(dates.length).toBeGreaterThanOrEqual(5)
  })
})

describe('POST /what-if', () => {
  // The mock builds trades off todayIso() (the local date); toISOString would
  // roll to tomorrow after 7 PM ET and break the at-spot/today invariant.
  const today = () => todayIso()
  const whatIfRequest = (id: string): WhatIfRequest => ({
    trade_ids: [id],
    price: MOCK_SPOT,
    date: today(),
    vol_shift: 0,
    include_realized: true,
  })

  it('answers with P&L and position greeks from the same engine', () => {
    const trade = trades().trades[0] as Trade
    const result = mockWhatIf(whatIfRequest(trade.id))
    expect(result.pnl).toBeCloseTo(trade.pnl_total, 0)
    expect(Number.isFinite(result.greeks.vega)).toBe(true)
  })

  /** Same toggle as the curves: at spot and today the answer must land on
   *  pnl_total with realised in, and on pnl_open with it out. */
  it('honours include_realized, defaulting to true when the field is absent', () => {
    const trade = trades().trades[0] as Trade
    const open = mockWhatIf({ ...whatIfRequest(trade.id), include_realized: false })
    expect(open.pnl).toBeCloseTo(trade.pnl_open, 0)
    expect(open.pnl).not.toBeCloseTo(trade.pnl_total, 0)
    // The server declares the field optional with a `true` default.
    const withoutFlag = { trade_ids: [trade.id], price: MOCK_SPOT, date: today(), vol_shift: 0 }
    expect(mockWhatIf(withoutFlag as WhatIfRequest).pnl).toBeCloseTo(trade.pnl_total, 0)
  })
})

describe('GET /reconciliation and /warnings', () => {
  it('compares quantities only and flags the cross-trade netting', () => {
    const response = mockRequest('/reconciliation') as ReconciliationResponse
    const netted = response.rows.find((row) => row.tos_code === 'SPX270115P7400')
    expect(netted?.status).toBe('qty_mismatch')
    expect(response.rows.some((row) => row.status === 'unassigned')).toBe(true)
    expect(response.note).toMatch(/broker\/tax lots/)
  })

  it('sorts warnings worst-first and includes the quarantine', () => {
    const response = mockRequest('/warnings') as WarningsResponse
    expect(response.warnings[0]?.severity).toBe('error')
    expect(response.warnings.some((w) => w.code === 'duplicate_execution_claim')).toBe(true)
    expect(response.warnings.some((w) => w.code === 'unassigned_account_position')).toBe(true)
  })
})

describe('mock routing through the API client', () => {
  it('serves GET and POST routes when mock mode is forced', async () => {
    setMockMode('forced')
    const warnings = await apiFetch<WarningsResponse>('/warnings', { method: 'GET' })
    expect(warnings.warnings.length).toBeGreaterThan(0)
    const reload = await apiFetch<{ reloaded: boolean }>('/reload', {
      method: 'POST',
      body: JSON.stringify({}),
    })
    expect(reload.reloaded).toBe(true)
  })

  it('turns an unknown route into a 404 ApiError rather than a raw throw', async () => {
    setMockMode('forced')
    await expect(apiFetch('/nope', { method: 'GET' })).rejects.toBeInstanceOf(ApiError)
    await expect(apiFetch('/nope', { method: 'GET' })).rejects.toMatchObject({ status: 404 })
  })
})

describe('the trade source (sheet / CSV)', () => {
  const source = () => mockRequest('/trades/source') as TradeSourceResponse
  const putSource = (body: TradeSourceRequest) =>
    apiFetch<TradeSourceResponse>('/trades/source', { method: 'PUT', body: JSON.stringify({ source: body }) })
  const probeSource = (body: TradeSourceRequest) =>
    apiFetch<TradeSourceResponse>('/trades/source/probe', {
      method: 'POST',
      body: JSON.stringify({ source: body }),
    })

  beforeEach(() => {
    setMockMode('forced')
    resetMockTradeSource()
  })

  it('GET answers the demo fallback shape when nothing has been chosen', () => {
    const body = source()
    expect(body.kind).toBe('demo')
    expect(body.label).toBe('fixtures/demo/legs.csv')
    expect(body.tab).toBeNull()
    expect(body.open_trades).toBe(trades().trades.length)
  })

  it('PUT a sheet url makes kind "sheet" with a masked label carrying no 20-char id', async () => {
    const loaded = await putSource({ kind: 'sheet', url: 'https://docs.google.com/spreadsheets/d/x', tab: 'Legs' })
    expect(loaded.kind).toBe('sheet')
    expect(loaded.tab).toBe('Legs')
    expect(loaded.label).not.toBeNull()
    // No 20+-char id may ever appear in a label the mock hands back.
    expect(loaded.label ?? '').not.toMatch(/[A-Za-z0-9_-]{20,}/)
    expect(source().kind).toBe('sheet')
  })

  it('PUT a bad host is a 422 invalid-trade-source', async () => {
    await expect(putSource({ kind: 'sheet', url: 'https://example.com/not-a-sheet' })).rejects.toMatchObject({
      status: 422,
    })
    expect(source().kind).toBe('demo')
  })

  it('PUT a url containing "not-shared" is a 422 whose slug is sheet-not-shared', async () => {
    const error = await putSource({
      kind: 'sheet',
      url: 'https://docs.google.com/spreadsheets/d/not-shared-id',
    }).catch((e: unknown) => e)
    expect(error).toBeInstanceOf(ApiError)
    const api = error as ApiError
    expect(api.status).toBe(422)
    expect(api.problem?.type?.split('/').pop()).toBe('sheet-not-shared')
  })

  it('a probe changes nothing: GET before and after agree on the loaded source', async () => {
    const before = source()
    await probeSource({ kind: 'csv', path: MOCK_CSV_PATH })
    const after = source()
    // `fetched_at` is a fresh timestamp on every GET by design; everything
    // that actually reflects the loaded source must be untouched by a probe.
    expect(after).toMatchObject({
      kind: before.kind,
      label: before.label,
      tab: before.tab,
      path: before.path,
      recent: before.recent,
    })
  })

  it('DELETE returns to the demo fallback and pushes the cleared source onto recent', async () => {
    await putSource({ kind: 'csv', path: MOCK_CSV_PATH })
    expect(source().kind).toBe('csv')

    const back = await apiFetch<TradeSourceResponse>('/trades/source', { method: 'DELETE' })
    expect(back.kind).toBe('demo')
    expect(back.recent).toHaveLength(1)
    expect(back.recent[0]).toMatchObject({ kind: 'csv', label: MOCK_CSV_PATH })
    expect(source().kind).toBe('demo')
  })
})

describe('mockRiskGraph t0_greeks', () => {
  it('gives one value per price, keyed to the T+0 line', () => {
    const ids = trades().trades.map((t) => t.id)
    const graph = mockRiskGraph(baseRequest({ trade_ids: ids }))
    const g = graph.t0_greeks
    expect(g?.key).toBe('t0')
    expect(g?.delta).toHaveLength(graph.prices.length)
    expect(g?.gamma).toHaveLength(graph.prices.length)
    expect(g?.theta).toHaveLength(graph.prices.length)
    expect(g?.vega).toHaveLength(graph.prices.length)
    expect(g?.weighted_vega).toHaveLength(graph.prices.length)
  })

  it('is finite at spot, and delta is non-zero for the demo book', () => {
    const ids = trades().trades.map((t) => t.id)
    const graph = mockRiskGraph(baseRequest({ trade_ids: ids }))
    const index = nearestPriceIndex(graph.prices, graph.spot)
    const g = graph.t0_greeks
    expect(g?.delta[index]).not.toBe(0)
    for (const row of [g?.gamma, g?.theta, g?.vega, g?.weighted_vega]) {
      expect(Number.isFinite(row?.[index])).toBe(true)
    }
  })

  it('follows the position: the short strangle carries the flipped (short) signs', () => {
    // Oct Strangle sells both legs outright — the one demo spec that is
    // unambiguously "short options" rather than a spread — so its t0 vega
    // and theta show the textbook short-option signs (vega < 0, theta > 0),
    // the flip of a naked long option's (vega > 0, theta < 0).
    const strangle = trades().trades.find((t) => t.name === 'Oct Strangle') as Trade
    const graph = mockRiskGraph(baseRequest({ trade_ids: [strangle.id] }))
    const index = nearestPriceIndex(graph.prices, graph.spot)
    const g = graph.t0_greeks
    expect(g?.vega[index]).toBeLessThan(0)
    expect(g?.theta[index]).toBeGreaterThan(0)
  })

  it('follows the position: the calendar (net long vega) carries positive vega', () => {
    // The calendar is the one open trade that is net long vega (see "raises a
    // long-vega position…" above). It is a spread, not a naked long option,
    // so unlike the strangle its theta need not carry a single clean sign.
    const calendar = trades().trades.find((t) => t.name === 'Jan Calendar') as Trade
    const graph = mockRiskGraph(baseRequest({ trade_ids: [calendar.id] }))
    const index = nearestPriceIndex(graph.prices, graph.spot)
    expect(graph.t0_greeks?.vega[index]).toBeGreaterThan(0)
  })

  it('names the weighted-vega reference the way the server does', () => {
    const ids = trades().trades.map((t) => t.id)
    const graph = mockRiskGraph(baseRequest({ trade_ids: ids }))
    expect(graph.t0_greeks?.weighted_vega_reference).toBe('30d')
  })

  it('moves vega when the vol shift moves', () => {
    const calendar = trades().trades.find((t) => t.name === 'Jan Calendar') as Trade
    const flat = mockRiskGraph(baseRequest({ trade_ids: [calendar.id] }))
    const bumped = mockRiskGraph(baseRequest({ trade_ids: [calendar.id], vol_shift: 0.05 }))
    const index = nearestPriceIndex(flat.prices, flat.spot)
    expect(bumped.t0_greeks?.vega[index]).not.toBe(flat.t0_greeks?.vega[index])
  })

  it('keeps gamma at finer resolution than 2dp, unlike the other rounded rows', () => {
    // Gamma is dollars-of-delta per index point — routinely two orders of
    // magnitude smaller than the other greeks for a single trade — while the
    // strip formats it at 4dp (`GREEK_DP.gamma`). Rounding it like the
    // others (2dp) would flatten most of a single trade's gamma row to 0.
    const flyOrCalendar = trades().trades.find((t) => t.name === 'Dec Fly') as Trade
    const graph = mockRiskGraph(baseRequest({ trade_ids: [flyOrCalendar.id] }))
    const gamma = graph.t0_greeks?.gamma ?? []
    const quantizedTo2dp = gamma.every((v) => Math.abs(v - Math.round(v * 100) / 100) < 1e-9)
    expect(quantizedTo2dp).toBe(false)
    // And it should not collapse to a flat run of zeros across the window.
    expect(gamma.filter((v) => v === 0).length).toBeLessThan(gamma.length / 4)
  })

  it('is null when the date list has no T+0 line', () => {
    const ids = trades().trades.map((t) => t.id)
    const graph = mockRiskGraph(baseRequest({ trade_ids: ids, dates: ['+5d'] }))
    expect(graph.lines.some((l) => l.kind === 't0')).toBe(false)
    expect(graph.t0_greeks).toBeNull()
  })
})
