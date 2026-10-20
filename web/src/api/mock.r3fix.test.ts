/** Round-3 review fix: the mock's what-if greeks at a leg's settlement are
 *  intrinsic's slope, as the engine's are (IPC.md §what_if). */

import { describe, expect, it } from 'vitest'
import { mockWhatIf } from './mock'
import type { WhatIfRequest } from './types'

const OCT = '2026-10-16'

function whatIf(price: number): WhatIfRequest {
  return {
    trade_ids: ['oct-strangle-2026-08-24'],
    price,
    date: OCT,
    vol_shift: 0,
    vol_shift_by_expiry: {},
    include_realized: true,
    model_legs: [],
  }
}

describe('mockWhatIf on the legs’ expiry', () => {
  // -1 OCT 8200 C, -1 OCT 7200 P, multiplier 100.
  it('carries an ITM short call’s delta of -100 per contract', () => {
    const { greeks } = mockWhatIf(whatIf(8500))
    expect(greeks.delta).toBeCloseTo(-100, 9)
    expect(greeks.gamma).toBe(0)
    expect(greeks.theta).toBe(0)
    expect(greeks.vega).toBe(0)
  })

  it('carries an ITM short put’s delta of +100, half at the strike, 0 between', () => {
    expect(mockWhatIf(whatIf(7000)).greeks.delta).toBeCloseTo(100, 9)
    expect(mockWhatIf(whatIf(7200)).greeks.delta).toBeCloseTo(50, 9)
    expect(mockWhatIf(whatIf(7700)).greeks.delta).toBe(0)
  })
})
