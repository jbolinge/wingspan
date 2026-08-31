import { describe, expect, it } from 'vitest'
import {
  addDays,
  daysBetween,
  formatDate,
  formatExpiry,
  formatExpiryShort,
  parseIsoDate,
  todayIso,
  yearFraction,
} from './dates'

describe('naive dates', () => {
  it('parses at UTC midnight so a browser timezone cannot shift an expiry', () => {
    expect(parseIsoDate('2027-02-19').toISOString()).toBe('2027-02-19T00:00:00.000Z')
  })

  it('adds days across month and year boundaries', () => {
    expect(addDays('2026-08-31', 1)).toBe('2026-09-01')
    expect(addDays('2026-12-31', 1)).toBe('2027-01-01')
    expect(addDays('2026-08-31', -1)).toBe('2026-08-30')
  })

  it('counts calendar days in both directions', () => {
    expect(daysBetween('2026-08-31', '2027-02-19')).toBe(172)
    expect(daysBetween('2026-07-14', '2026-08-31')).toBe(48)
    expect(daysBetween('2026-08-31', '2026-08-31')).toBe(0)
    expect(daysBetween('2026-08-31', '2026-08-30')).toBe(-1)
  })

  it('floors the year fraction at zero for a past date', () => {
    expect(yearFraction('2026-08-31', '2027-08-31')).toBeCloseTo(1, 2)
    expect(yearFraction('2026-08-31', '2026-08-01')).toBe(0)
  })

  it('formats expiries the TOS way and the compact way', () => {
    expect(formatExpiry('2027-02-19')).toBe('19 FEB 27')
    expect(formatExpiryShort('2027-02-19')).toBe('FEB27')
    expect(formatDate('2026-08-31')).toBe('Aug 31 2026')
    expect(formatDate(null)).toBe('—')
  })

  it('derives today from local calendar fields', () => {
    expect(todayIso(new Date(2026, 7, 31, 23, 45))).toBe('2026-08-31')
  })
})
