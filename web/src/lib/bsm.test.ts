import { describe, expect, it } from 'vitest'
import { bsmGreeks, bsmPrice, intrinsic, normCdf } from './bsm'

const BASE = { spot: 100, strike: 100, t: 1, vol: 0.2, rate: 0.05, divYield: 0 } as const

describe('normCdf', () => {
  it('matches the standard normal at the usual reference points', () => {
    expect(normCdf(0)).toBeCloseTo(0.5, 6)
    expect(normCdf(1)).toBeCloseTo(0.8413447, 5)
    expect(normCdf(-1.96)).toBeCloseTo(0.025, 4)
  })
})

describe('bsmPrice', () => {
  it('reproduces the textbook one-year at-the-money call', () => {
    // S=100, K=100, T=1, sigma=0.2, r=0.05 -> 10.4506
    expect(bsmPrice({ ...BASE, right: 'call' })).toBeCloseTo(10.4506, 3)
  })

  it('reproduces the matching put', () => {
    expect(bsmPrice({ ...BASE, right: 'put' })).toBeCloseTo(5.5735, 3)
  })

  it('satisfies put-call parity', () => {
    const call = bsmPrice({ ...BASE, right: 'call' })
    const put = bsmPrice({ ...BASE, right: 'put' })
    const parity = BASE.spot * Math.exp(-BASE.divYield * BASE.t) - BASE.strike * Math.exp(-BASE.rate * BASE.t)
    expect(call - put).toBeCloseTo(parity, 6)
  })

  it('collapses to intrinsic at expiry and with zero vol', () => {
    expect(bsmPrice({ ...BASE, t: 0, spot: 110, right: 'call' })).toBe(10)
    expect(bsmPrice({ ...BASE, vol: 0, spot: 90, right: 'put' })).toBe(10)
    expect(intrinsic(90, 100, 'call')).toBe(0)
  })

  it('is monotone in volatility for a long option', () => {
    const low = bsmPrice({ ...BASE, vol: 0.1, right: 'call' })
    const high = bsmPrice({ ...BASE, vol: 0.4, right: 'call' })
    expect(high).toBeGreaterThan(low)
  })
})

describe('bsmGreeks', () => {
  it('bumps to roughly the analytic delta of an ATM call', () => {
    const greeks = bsmGreeks({ ...BASE, right: 'call' })
    expect(greeks.delta).toBeCloseTo(0.6368, 2)
    expect(greeks.gamma).toBeGreaterThan(0)
  })

  it('reports theta as the one-day reprice, so a long option decays', () => {
    expect(bsmGreeks({ ...BASE, right: 'call' }).theta).toBeLessThan(0)
  })

  it('reports vega per vol point, so it is small next to the raw derivative', () => {
    const vega = bsmGreeks({ ...BASE, right: 'call' }).vega
    // dPrice/dSigma is ~37.5 for this option; per 1 vol point that is ~0.375.
    expect(vega).toBeCloseTo(0.375, 2)
  })
})
