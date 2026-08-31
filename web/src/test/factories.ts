/** Minimal builders for API shapes, so tests state only what they care about. */

import type { Greeks, Leg, OptionContract, Trade } from '../api/types'

export function makeGreeks(partial: Partial<Greeks> = {}): Greeks {
  return { delta: 0, gamma: 0, theta: 0, vega: 0, ...partial }
}

export function makeContract(partial: Partial<OptionContract> = {}): OptionContract {
  const base: OptionContract = {
    underlying: 'SPX',
    root: 'SPX',
    expiry: '2027-02-19',
    settlement: 'am',
    strike: 7325,
    right: 'put',
    multiplier: 100,
    osi: 'SPX   270219P07325000',
    tos_code: 'SPX270219P7325',
  }
  return { ...base, ...partial }
}

export function makeLeg(partial: Partial<Leg> = {}): Leg {
  return {
    contract: makeContract(partial.contract),
    qty: 1,
    avg_cost: 22.1,
    mark: 23.5,
    bid: 23.2,
    ask: 23.8,
    iv: 0.17,
    greeks: makeGreeks({ delta: -0.4, gamma: 0.0003, theta: -0.5, vega: 0.9 }),
    dte: 172,
    quote_as_of: '2026-08-31T14:30:00Z',
    quote_source: 'yfinance',
    ...partial,
  }
}

export function makeTrade(partial: Partial<Trade> = {}): Trade {
  return {
    id: 'm1-2026-07-14',
    name: 'M1',
    status: 'open',
    opened_on: '2026-07-14',
    closed_on: null,
    dit: 48,
    dte: 172,
    structure: '2x BF 7325/6975/6625 FEB27',
    cost: 4420,
    value: 5100,
    pnl_open: 680,
    pnl_total: 247,
    pnl_pct: 5.6,
    realized: -433,
    fees: 3.78,
    greeks: makeGreeks({ delta: -1.2, gamma: 0.003, theta: 55, vega: -320 }),
    weighted_vega: -812.4,
    staleness_secs: 12,
    warnings: [],
    legs: [makeLeg()],
    quarantined: [],
    ...partial,
  }
}
