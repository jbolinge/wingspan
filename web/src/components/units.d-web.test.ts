/** Tooltip strings for per-contract greeks and XSP exposure (D5). */

import { describe, expect, it } from 'vitest'
import {
  CHAIN_UNITS,
  LEG_GREEK_UNITS,
  SPX_POINTS,
  XSP_AXIS_NOTE,
  XSP_CHAIN_UNITS,
  XSP_EXPOSURE_NOTE,
} from './units'

describe('XSP_EXPOSURE_NOTE', () => {
  it('states the exposure as 0.1 × the SPXW proxy greeks', () => {
    expect(XSP_EXPOSURE_NOTE).toMatch(/0\.1 × the greeks of the SPXW proxy/)
  })

  it('gives the factors against the XSP option own greeks per greek', () => {
    expect(XSP_EXPOSURE_NOTE).toMatch(/Δ × 0\.1 \(× 10 for XSP points\)/)
    expect(XSP_EXPOSURE_NOTE).toMatch(/Γ × 0\.01 \(× 100 for XSP points\)/)
    expect(XSP_EXPOSURE_NOTE).toMatch(/Θ and V unchanged/)
    expect(XSP_EXPOSURE_NOTE).not.toMatch(/one tenth of the XSP option’s own Δ and Γ/)
  })
})

describe('per-contract greek texts', () => {
  it('never carry the position-level dollar texts', () => {
    for (const text of [...Object.values(LEG_GREEK_UNITS), CHAIN_UNITS.delta, CHAIN_UNITS.gamma, CHAIN_UNITS.theta, CHAIN_UNITS.vega]) {
      expect(text).toMatch(/^Per contract, per share/)
      expect(text).not.toMatch(/Σ qty/)
      expect(text).not.toMatch(/position P&L/)
    }
  })

  it('keeps the XSP chain in the option own terms, against the proxy', () => {
    expect(XSP_CHAIN_UNITS.delta).toMatch(/^Per XSP contract, in XSP points, per share/)
    expect(XSP_CHAIN_UNITS.gamma).toMatch(/Γ × 10/)
    expect(XSP_CHAIN_UNITS.gamma).toMatch(/0\.1 × the SPXW proxy’s greeks/)
  })
})

describe('SPX-point price units', () => {
  it('names the unit and explains XSP strikes against it', () => {
    expect(SPX_POINTS).toBe('SPX pts')
    expect(XSP_AXIS_NOTE).toMatch(/XSP 650 is SPX 6500/)
  })
})
