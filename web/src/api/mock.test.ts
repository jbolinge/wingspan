import { describe, expect, it } from 'vitest'
import { nearestPriceIndex } from '../lib/readout'
import { ApiError, apiGet, apiPost, setMockMode } from './client'
import { MOCK_SPOT, mockRequest, mockRiskGraph, mockWhatIf, resolveGraphDates } from './mock'
import type {
  ClosedTradesResponse,
  PortfolioStats,
  ReconciliationResponse,
  RiskGraphRequest,
  Trade,
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

  it('carries the plan’s BSH roll: realised -433 on fees of 3.78', () => {
    const bsh = trades().trades.find((t) => t.name === 'BSH')
    expect(bsh?.realized).toBe(-433)
    expect(bsh?.fees).toBe(3.78)
    expect(bsh?.legs).toHaveLength(1)
    expect(bsh?.legs[0]?.contract.tos_code).toBe('SPX261016P5450')
  })

  it('builds OSI symbols with a six-character root and thousandths strike', () => {
    const leg = trades().trades.flatMap((t) => t.legs)[0]
    expect(leg?.contract.osi).toMatch(/^.{6}\d{6}[PC]\d{8}$/)
  })

  it('aggregates the portfolio and names its weighted-vega reference', () => {
    const stats = trades().portfolio_all
    expect(stats.count).toBe(trades().trades.length)
    expect(stats.weighted_vega_reference).toMatch(/^front_expiry \(\d{4}-\d{2}-\d{2}\)$/)
  })
})

describe('GET /trades/closed', () => {
  it('reports totals, a win rate and the LPTA quarantine', () => {
    const response = mockRequest('/trades/closed') as ClosedTradesResponse
    expect(response.totals.count).toBe(response.trades.length)
    expect(response.totals.win_rate).toBeGreaterThanOrEqual(0)
    expect(response.totals.win_rate).toBeLessThanOrEqual(1)
    const lpta = response.trades.find((t) => t.name === 'LPTA')
    expect(lpta?.quarantined).toHaveLength(6)
    expect(lpta?.closed_on).not.toBeNull()
    expect(lpta?.pnl_total).toBeCloseTo(-162.35, 2)
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

describe('GET /trades/{id}', () => {
  it('resolves a single trade and 404s an unknown one', () => {
    const first = trades().trades[0] as Trade
    expect((mockRequest(`/trades/${first.id}`) as Trade).id).toBe(first.id)
    expect(() => mockRequest('/trades/nope')).toThrow()
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
    const bsh = trades().trades.find((t) => t.name === 'BSH') as Trade
    const flat = mockRiskGraph(baseRequest({ trade_ids: [bsh.id] }))
    const bumped = mockRiskGraph(baseRequest({ trade_ids: [bsh.id], vol_shift: 0.05 }))
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
  const today = () => new Date().toISOString().slice(0, 10)
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
    const netted = response.rows.find((row) => row.tos_code === 'SPX270219P6800')
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
    const health = await apiGet<{ status: string }>('/health')
    expect(health.status).toBe('ok')
    const reload = await apiPost<{ reloaded: boolean }>('/reload', {})
    expect(reload.reloaded).toBe(true)
  })

  it('turns an unknown route into a 404 ApiError rather than a raw throw', async () => {
    setMockMode('forced')
    await expect(apiGet('/nope')).rejects.toBeInstanceOf(ApiError)
    await expect(apiGet('/nope')).rejects.toMatchObject({ status: 404 })
  })
})
