/**
 * A compact Black-Scholes-Merton implementation.
 *
 * The server (`wingspan-pricing`) owns the real pricing; this copy exists so
 * the browser-side mock provider (`src/api/mock.ts`) can synthesise a market
 * that is *internally consistent*: the same legs drive leg marks, position
 * greeks, the risk-graph curves and the what-if panel, which is what makes the
 * documented invariant (`t0` line at spot == dashboard `pnl_total`) hold in
 * demo mode. It is never used to price anything shown as live data.
 *
 * Unit conventions match `docs/API.md`:
 *   - delta, gamma: per 1 index point
 *   - vega: per 1.00 vol point (i.e. per 0.01 of decimal vol)
 *   - theta: reprice at T-1 calendar day (ONE-style), per contract
 */

import type { Greeks, Right } from '../api/types'

const SQRT_2PI = Math.sqrt(2 * Math.PI)

/** Standard normal PDF. */
export function normPdf(x: number): number {
  return Math.exp(-0.5 * x * x) / SQRT_2PI
}

/**
 * Standard normal CDF via the Numerical Recipes complementary error function
 * (fractional error < 1.2e-7 — ample for display and for the mock).
 */
export function normCdf(x: number): number {
  const z = Math.abs(x) / Math.SQRT2
  const t = 1 / (1 + 0.5 * z)
  const ans =
    t *
    Math.exp(
      -z * z -
        1.26551223 +
        t *
          (1.00002368 +
            t *
              (0.37409196 +
                t *
                  (0.09678418 +
                    t *
                      (-0.18628806 +
                        t *
                          (0.27886807 +
                            t *
                              (-1.13520398 +
                                t * (1.48851587 + t * (-0.82215223 + t * 0.17087277)))))))),
    )
  const erfc = x >= 0 ? ans : 2 - ans
  return 1 - 0.5 * erfc
}

export interface BsmInput {
  /** Spot of the underlying index. */
  spot: number
  strike: number
  /** Year fraction to expiry (ACT/365). */
  t: number
  /** Decimal volatility (0.16 = 16%). */
  vol: number
  /** Continuously compounded risk-free rate. */
  rate: number
  /** Continuous dividend yield. */
  divYield: number
  right: Right
}

/** Intrinsic value of one contract in option price points. */
export function intrinsic(spot: number, strike: number, right: Right): number {
  return right === 'call' ? Math.max(0, spot - strike) : Math.max(0, strike - spot)
}

/** European option price in option price points (per 1 contract, no multiplier). */
export function bsmPrice(input: BsmInput): number {
  const { spot, strike, t, vol, rate, divYield, right } = input
  if (!(t > 0) || !(vol > 0)) return intrinsic(spot, strike, right)
  const sqrtT = Math.sqrt(t)
  const d1 = (Math.log(spot / strike) + (rate - divYield + 0.5 * vol * vol) * t) / (vol * sqrtT)
  const d2 = d1 - vol * sqrtT
  const dfR = Math.exp(-rate * t)
  const dfQ = Math.exp(-divYield * t)
  return right === 'call'
    ? spot * dfQ * normCdf(d1) - strike * dfR * normCdf(d2)
    : strike * dfR * normCdf(-d2) - spot * dfQ * normCdf(-d1)
}

const DAY = 1 / 365

/**
 * Bump-and-reprice greeks, mirroring the server's conventions.
 * Returned per contract and per unit of the underlying (no qty, no multiplier).
 */
export function bsmGreeks(input: BsmInput): Greeks {
  const price = bsmPrice(input)
  const h = Math.max(0.5, input.spot * 1e-4)
  const up = bsmPrice({ ...input, spot: input.spot + h })
  const down = bsmPrice({ ...input, spot: input.spot - h })
  const delta = (up - down) / (2 * h)
  const gamma = (up - 2 * price + down) / (h * h)
  // Vega per 1.00 vol point == per 0.01 of decimal vol.
  const volUp = bsmPrice({ ...input, vol: input.vol + 0.005 })
  const volDown = bsmPrice({ ...input, vol: Math.max(1e-4, input.vol - 0.005) })
  const vega = volUp - volDown
  // Theta: reprice one calendar day forward.
  const tomorrow = bsmPrice({ ...input, t: Math.max(0, input.t - DAY) })
  const theta = tomorrow - price
  return { delta, gamma, theta, vega }
}

/** Sum of `qty x multiplier x greek` over legs — the position convention. */
export function scaleGreeks(greeks: Greeks, factor: number): Greeks {
  return {
    delta: greeks.delta * factor,
    gamma: greeks.gamma * factor,
    theta: greeks.theta * factor,
    vega: greeks.vega * factor,
  }
}

export function addGreeks(a: Greeks, b: Greeks): Greeks {
  return {
    delta: a.delta + b.delta,
    gamma: a.gamma + b.gamma,
    theta: a.theta + b.theta,
    vega: a.vega + b.vega,
  }
}

export const ZERO_GREEKS: Greeks = { delta: 0, gamma: 0, theta: 0, vega: 0 }

export function sumGreeks(items: Greeks[]): Greeks {
  return items.reduce(addGreeks, ZERO_GREEKS)
}
