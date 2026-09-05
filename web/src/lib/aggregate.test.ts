import { describe, expect, it } from 'vitest'
import { makeGreeks, makeLeg, makeTrade } from '../test/factories'
import {
  aggregateTrades,
  expiriesOf,
  frontExpiry,
  invertIds,
  pruneIds,
  selectAllIds,
  sortTrades,
  tradesById,
  warningCount,
  worstSeverity,
} from './aggregate'

const feb = makeTrade({
  id: 'a',
  name: 'A',
  legs: [makeLeg({ contract: { ...makeLeg().contract, expiry: '2027-02-19', strike: 7325 } })],
})
const mar = makeTrade({
  id: 'b',
  name: 'B',
  legs: [
    makeLeg({ contract: { ...makeLeg().contract, expiry: '2027-03-19', strike: 6500 } }),
    makeLeg({ qty: 0, contract: { ...makeLeg().contract, expiry: '2026-09-18', strike: 100 } }),
  ],
})

describe('expiry helpers', () => {
  it('ignores closed legs when finding the front expiry', () => {
    expect(frontExpiry([feb, mar])).toBe('2027-02-19')
    expect(frontExpiry([mar])).toBe('2027-03-19')
    expect(frontExpiry([])).toBeNull()
  })

  it('lists distinct open expiries ascending', () => {
    expect(expiriesOf([mar, feb])).toEqual(['2027-02-19', '2027-03-19'])
  })
})

describe('aggregateTrades', () => {
  it('sums P&L and greeks and names the weighted-vega reference', () => {
    const a = makeTrade({ id: 'a', pnl_open: 100, pnl_total: 50, weighted_vega: -10, greeks: makeGreeks({ delta: 2, theta: 5 }) })
    const b = makeTrade({ id: 'b', pnl_open: -40, pnl_total: -60, weighted_vega: 4, greeks: makeGreeks({ delta: -1, theta: 7 }) })
    const stats = aggregateTrades([a, b])
    expect(stats.count).toBe(2)
    expect(stats.pnl_open).toBe(60)
    expect(stats.pnl_total).toBe(-10)
    expect(stats.greeks.delta).toBe(1)
    expect(stats.greeks.theta).toBe(12)
    expect(stats.weighted_vega).toBe(-6)
    expect(stats.weighted_vega_reference).toBe('front_expiry (2027-02-19)')
  })

  it('is empty-safe', () => {
    const stats = aggregateTrades([])
    expect(stats.count).toBe(0)
    expect(stats.pnl_total).toBe(0)
    expect(stats.weighted_vega_reference).toContain('—')
  })
})

describe('selection algebra', () => {
  const trades = [makeTrade({ id: 'a' }), makeTrade({ id: 'b' }), makeTrade({ id: 'c' })]

  it('selects all and inverts', () => {
    expect(selectAllIds(trades)).toEqual(['a', 'b', 'c'])
    expect(invertIds(trades, ['b'])).toEqual(['a', 'c'])
    expect(invertIds(trades, [])).toEqual(['a', 'b', 'c'])
    expect(invertIds(trades, ['a', 'b', 'c'])).toEqual([])
  })

  it('prunes ids that no longer exist', () => {
    expect(pruneIds(trades, ['a', 'zzz'])).toEqual(['a'])
  })

  it('resolves ids back to trades', () => {
    expect(tradesById(trades, ['c', 'a']).map((t) => t.id)).toEqual(['a', 'c'])
  })
})

describe('sortTrades', () => {
  const trades = [
    makeTrade({ id: 'a', name: 'Bravo', pnl_total: 100, dte: 10, pnl_pct: null }),
    makeTrade({ id: 'b', name: 'Alpha', pnl_total: -50, dte: null, pnl_pct: 4 }),
    makeTrade({ id: 'c', name: 'Charlie', pnl_total: 100, dte: 5, pnl_pct: 9 }),
  ]

  it('sorts strings case-insensitively', () => {
    expect(sortTrades(trades, 'name', 'asc').map((t) => t.name)).toEqual(['Alpha', 'Bravo', 'Charlie'])
  })

  it('sorts numbers and breaks ties by name so the order never jitters', () => {
    expect(sortTrades(trades, 'pnl_total', 'desc').map((t) => t.name)).toEqual(['Bravo', 'Charlie', 'Alpha'])
  })

  it('sinks a null DTE to the far end rather than treating it as zero', () => {
    expect(sortTrades(trades, 'dte', 'asc').map((t) => t.name)).toEqual(['Charlie', 'Bravo', 'Alpha'])
  })

  it('sorts by a nested greek', () => {
    const byTheta = sortTrades(
      [
        makeTrade({ id: 'x', name: 'X', greeks: makeGreeks({ theta: -5 }) }),
        makeTrade({ id: 'y', name: 'Y', greeks: makeGreeks({ theta: 9 }) }),
      ],
      'theta',
      'desc',
    )
    expect(byTheta.map((t) => t.name)).toEqual(['Y', 'X'])
  })

  it('does not mutate its input', () => {
    const input = [...trades]
    sortTrades(input, 'name', 'desc')
    expect(input.map((t) => t.name)).toEqual(['Bravo', 'Alpha', 'Charlie'])
  })
})

describe('warning badges', () => {
  it('does not double-count quarantined orders already represented by a claim warning', () => {
    const trade = makeTrade({
      warnings: [
        {
          severity: 'warn',
          code: 'duplicate_execution_claim',
          message: 'm',
          trade_id: null,
          contract: null,
        },
      ],
      quarantined: [{ raw: 'BUY +1 …', reason: 'duplicate execution claim', claimed_by: 'Alpha' }],
    })
    expect(warningCount(trade)).toBe(1)
  })

  it('counts warnings and quarantined orders together', () => {
    const trade = makeTrade({
      warnings: [{ severity: 'info', code: 'x', message: 'm', trade_id: null, contract: null }],
      quarantined: [{ raw: 'BUY +1 …', reason: 'duplicate execution claim', claimed_by: 'Alpha' }],
    })
    expect(warningCount(trade)).toBe(2)
  })

  it('treats any quarantine as an error even when the warnings are mild', () => {
    const trade = makeTrade({
      warnings: [{ severity: 'info', code: 'x', message: 'm', trade_id: null, contract: null }],
      quarantined: [{ raw: 'BUY +1 …', reason: 'duplicate execution claim', claimed_by: null }],
    })
    expect(worstSeverity(trade)).toBe('error')
  })

  it('reports the worst severity present', () => {
    expect(
      worstSeverity(
        makeTrade({
          warnings: [
            { severity: 'info', code: 'a', message: '', trade_id: null, contract: null },
            { severity: 'warn', code: 'b', message: '', trade_id: null, contract: null },
          ],
        }),
      ),
    ).toBe('warn')
    expect(worstSeverity(makeTrade())).toBeNull()
  })
})
