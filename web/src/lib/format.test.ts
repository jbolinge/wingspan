import { describe, expect, it } from 'vitest'
import {
  fmtAge,
  fmtGreek,
  fmtLegGreek,
  fmtMoney,
  fmtNumber,
  fmtPct,
  fmtPnl,
  fmtPrice,
  fmtQty,
  fmtRatioPct,
  pnlColorClass,
  stalenessLevel,
} from './format'

describe('fmtMoney', () => {
  it('groups thousands and keeps two decimals by default', () => {
    expect(fmtMoney(1234.5)).toBe('$1,234.50')
  })

  it('puts the sign before the currency symbol', () => {
    expect(fmtMoney(-1234.5)).toBe('-$1,234.50')
    expect(fmtMoney(1234.5, { signed: true })).toBe('+$1,234.50')
  })

  it('never renders a signed zero', () => {
    expect(fmtMoney(-0.001, { dp: 2, signed: true })).toBe('$0.00')
  })

  it('drops the currency symbol on request', () => {
    expect(fmtMoney(-12.5, { dp: 2, currency: false, signed: true })).toBe('-12.50')
  })

  it('renders an em dash for missing and non-finite values', () => {
    expect(fmtMoney(null)).toBe('—')
    expect(fmtMoney(undefined)).toBe('—')
    expect(fmtMoney(Number.NaN)).toBe('—')
    expect(fmtMoney(Number.POSITIVE_INFINITY)).toBe('—')
  })
})

describe('fmtPnl', () => {
  it('is signed money, zero decimals in dense columns', () => {
    expect(fmtPnl(680, 0)).toBe('+$680')
    expect(fmtPnl(-433, 0)).toBe('-$433')
    expect(fmtPnl(0, 0)).toBe('$0')
  })
})

describe('percentages', () => {
  it('treats API percents as already scaled', () => {
    expect(fmtPct(5.6)).toBe('+5.6%')
    expect(fmtPct(-5.64, 2)).toBe('-5.64%')
    expect(fmtPct(null)).toBe('—')
  })

  it('scales decimal ratios such as IV and PoP', () => {
    expect(fmtRatioPct(0.1712)).toBe('17.1%')
    expect(fmtRatioPct(0.61, 0)).toBe('61%')
  })
})

describe('numbers', () => {
  it('formats prices and quantities the TOS way', () => {
    expect(fmtPrice(22.1)).toBe('22.10')
    expect(fmtQty(2)).toBe('+2')
    expect(fmtQty(-4)).toBe('-4')
    expect(fmtNumber(6501.25, 2)).toBe('6,501.25')
  })
})

describe('greeks', () => {
  it('uses a different precision per greek at the position level', () => {
    expect(fmtGreek('delta', -1.246)).toBe('-1.25')
    expect(fmtGreek('gamma', 0.00312)).toBe('0.0031')
    expect(fmtGreek('theta', 55.4)).toBe('55')
    expect(fmtGreek('vega', -320.6)).toBe('-321')
  })

  it('gives per-contract greeks more resolution', () => {
    expect(fmtLegGreek('delta', -0.4321)).toBe('-0.43')
    expect(fmtLegGreek('gamma', 0.000123)).toBe('0.00012')
  })

  it('gives an XSP leg (a tenth of SPX) one more decimal of delta and gamma', () => {
    expect(fmtLegGreek('delta', -0.007438, true)).toBe('-0.007')
    expect(fmtLegGreek('gamma', 0.0000123, true)).toBe('0.000012')
    expect(fmtLegGreek('theta', -0.1234, true)).toBe('-0.12')
  })
})

describe('fmtAge', () => {
  it('steps through seconds, minutes, hours and days', () => {
    expect(fmtAge(12)).toBe('12s')
    expect(fmtAge(89)).toBe('89s')
    expect(fmtAge(640)).toBe('11m')
    expect(fmtAge(9000)).toBe('2.5h')
    expect(fmtAge(200_000)).toBe('2.3d')
    expect(fmtAge(null)).toBe('—')
    expect(fmtAge(-1)).toBe('—')
  })
})

describe('stalenessLevel', () => {
  it('bands quote age for the row dot', () => {
    expect(stalenessLevel(12)).toBe('fresh')
    expect(stalenessLevel(120)).toBe('aging')
    expect(stalenessLevel(1_800)).toBe('stale')
    expect(stalenessLevel(90_000)).toBe('dead')
    expect(stalenessLevel(null)).toBe('dead')
  })
})

describe('pnlColorClass', () => {
  it('is neutral at exactly zero so a flat trade does not read as a win', () => {
    expect(pnlColorClass(0)).toBe('text-muted')
    expect(pnlColorClass(1)).toBe('text-pos')
    expect(pnlColorClass(-1)).toBe('text-neg')
    expect(pnlColorClass(null)).toBe('text-muted')
  })
})
