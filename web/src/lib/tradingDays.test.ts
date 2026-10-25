import { describe, expect, it } from 'vitest'
import { isTradingDay, marketHolidays, nearestTradingOffset } from './tradingDays'

describe('market holidays', () => {
  it('lists the ten NYSE closures of a year, as observed', () => {
    expect(marketHolidays(2026)).toEqual([
      '2026-01-01',
      '2026-01-19',
      '2026-02-16',
      '2026-04-03',
      '2026-05-25',
      '2026-06-19',
      // July 4 is a Saturday: the Friday is the holiday (the Rust calendar
      // agrees — no early close that day).
      '2026-07-03',
      '2026-09-07',
      '2026-11-26',
      '2026-12-25',
    ])
    expect(marketHolidays(2027)).toEqual([
      '2027-01-01',
      '2027-01-18',
      '2027-02-15',
      '2027-03-26',
      '2027-05-31',
      // Juneteenth and Christmas fall on Saturdays: observed the Friday before.
      '2027-06-18',
      '2027-07-05',
      '2027-09-06',
      '2027-11-25',
      '2027-12-24',
    ])
  })

  it('does not observe a Saturday New Year’s Day on the Friday before', () => {
    // 2028-01-01 is a Saturday: December 31, 2027 is a trading day.
    expect(marketHolidays(2028)[0]).toBe('2028-01-17')
    expect(isTradingDay('2027-12-31')).toBe(true)
    // 2023-01-01 was a Sunday: observed Monday.
    expect(marketHolidays(2023)[0]).toBe('2023-01-02')
  })

  it('knows weekends and holidays from trading days', () => {
    expect(isTradingDay('2026-10-30')).toBe(true)
    expect(isTradingDay('2026-10-31')).toBe(false)
    expect(isTradingDay('2026-11-01')).toBe(false)
    expect(isTradingDay('2026-11-26')).toBe(false)
    expect(isTradingDay('2026-11-27')).toBe(true)
  })
})

describe('nearestTradingOffset', () => {
  it('keeps T+60 when it is a trading day', () => {
    // The demo book's day: 60 days on is Friday October 30.
    expect(nearestTradingOffset('2026-08-31', 60)).toBe(60)
  })

  it('moves a Saturday back a day and a Sunday forward a day', () => {
    expect(nearestTradingOffset('2026-09-01', 60)).toBe(59)
    expect(nearestTradingOffset('2026-09-02', 60)).toBe(61)
  })

  it('steps over a Monday holiday to the Tuesday, and ties to the earlier day', () => {
    // 2026-11-19 + 60 = 2027-01-18, Martin Luther King Jr. Day: Sunday
    // before, Tuesday after.
    expect(nearestTradingOffset('2026-11-19', 60)).toBe(61)
    // 2029-05-05 + 60 = 2029-07-04, a Wednesday: Tuesday and Thursday both
    // trade, the earlier wins.
    expect(nearestTradingOffset('2029-05-05', 60)).toBe(59)
  })

  it('never goes under T+1', () => {
    // A Saturday today: T+1 is Sunday, T+2 Monday.
    expect(nearestTradingOffset('2026-10-31', 1)).toBe(2)
  })
})
