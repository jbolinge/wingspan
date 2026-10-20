/** XSP quotes keep their third decimal (D4). */

import { describe, expect, it } from 'vitest'
import { fmtPrice, fmtQuote } from './format'

describe('fmtQuote', () => {
  it('prints a proxied (XSP) quote at 3 dp when the third decimal is non-zero', () => {
    expect(fmtQuote(1.935, { tenth: true })).toBe('1.935')
    expect(fmtQuote(0.245, { tenth: true })).toBe('0.245')
    expect(fmtQuote(19.35 / 10, { tenth: true })).toBe('1.935')
    expect(fmtQuote(2.45 / 10, { tenth: true })).toBe('0.245')
  })

  it('stays at 2 dp when the third decimal is zero', () => {
    expect(fmtQuote(1.94, { tenth: true })).toBe('1.94')
    expect(fmtQuote(19.4 / 10, { tenth: true })).toBe('1.94')
    expect(fmtQuote(3, { tenth: true })).toBe('3.00')
  })

  it('is fmtPrice for an SPX quote, and a dash for nothing', () => {
    expect(fmtQuote(22.1)).toBe(fmtPrice(22.1))
    expect(fmtQuote(null, { tenth: true })).toBe('—')
    expect(fmtQuote(Number.NaN)).toBe('—')
  })

  it('makes price × qty × 100 add up to the net premium', () => {
    const mark = 19.35 / 10
    expect(Number(fmtQuote(mark, { tenth: true })) * 10 * 100).toBeCloseTo(mark * 10 * 100, 9)
  })
})
