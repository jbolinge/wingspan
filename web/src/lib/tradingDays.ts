/**
 * The US equity market's full-day closures, rule-based so there is no year
 * list to rot (the same stance as the Rust `calendar` module, which knows
 * only the early closes because that is all pricing needs). This is for the
 * trader's convenience — "give me the T+60 line, on a day the market is
 * open" — never for pricing: a `+Nd` line prices at `as_of` + N days
 * whatever day of the week that is.
 *
 * NYSE holidays: New Year's Day, Martin Luther King Jr. Day (third Monday
 * of January), Presidents' Day (third Monday of February), Good Friday,
 * Memorial Day (last Monday of May), Juneteenth, Independence Day, Labor
 * Day (first Monday of September), Thanksgiving (fourth Thursday of
 * November) and Christmas. A fixed-date holiday on a Saturday is observed
 * the Friday before and on a Sunday the Monday after — except New Year's
 * Day on a Saturday, which the exchange does not observe on December 31.
 */

import type { IsoDate } from '../api/types'
import { addDays, parseIsoDate, toIsoDate } from './dates'

/** 0 = Sunday … 6 = Saturday, like `Date.getUTCDay`. */
function weekday(date: IsoDate): number {
  return parseIsoDate(date).getUTCDay()
}

function iso(year: number, month: number, day: number): IsoDate {
  return toIsoDate(new Date(Date.UTC(year, month - 1, day)))
}

/** The `n`th (1-based) given weekday of a month; `n = -1` for the last. */
function nthWeekday(year: number, month: number, dow: number, n: number): IsoDate {
  if (n > 0) {
    const first = iso(year, month, 1)
    const offset = (dow - weekday(first) + 7) % 7
    return addDays(first, offset + (n - 1) * 7)
  }
  const last = addDays(iso(year, month + 1, 1), -1)
  return addDays(last, -((weekday(last) - dow + 7) % 7))
}

/** A fixed-date holiday as the exchange observes it. */
function observed(year: number, month: number, day: number, fridayBefore = true): IsoDate | null {
  const date = iso(year, month, day)
  const dow = weekday(date)
  if (dow === 6) return fridayBefore ? addDays(date, -1) : null
  if (dow === 0) return addDays(date, 1)
  return date
}

/** Western Easter Sunday (Meeus/Jones/Butcher). */
function easter(year: number): IsoDate {
  const a = year % 19
  const b = Math.floor(year / 100)
  const c = year % 100
  const d = Math.floor(b / 4)
  const e = b % 4
  const f = Math.floor((b + 8) / 25)
  const g = Math.floor((b - f + 1) / 3)
  const h = (19 * a + b - d - g + 15) % 30
  const i = Math.floor(c / 4)
  const k = c % 4
  const l = (32 + 2 * e + 2 * i - h - k) % 7
  const m = Math.floor((a + 11 * h + 22 * l) / 451)
  const month = Math.floor((h + l - 7 * m + 114) / 31)
  const day = ((h + l - 7 * m + 114) % 31) + 1
  return iso(year, month, day)
}

/** Every full-day closure of `year`, as observed. */
export function marketHolidays(year: number): IsoDate[] {
  const days: (IsoDate | null)[] = [
    observed(year, 1, 1, false),
    nthWeekday(year, 1, 1, 3),
    nthWeekday(year, 2, 1, 3),
    addDays(easter(year), -2),
    nthWeekday(year, 5, 1, -1),
    observed(year, 6, 19),
    observed(year, 7, 4),
    nthWeekday(year, 9, 1, 1),
    nthWeekday(year, 11, 4, 4),
    observed(year, 12, 25),
  ]
  return days.filter((day): day is IsoDate => day !== null)
}

/** A weekday that is not a market holiday. */
export function isTradingDay(date: IsoDate): boolean {
  const dow = weekday(date)
  if (dow === 0 || dow === 6) return false
  return !marketHolidays(parseIsoDate(date).getUTCFullYear()).includes(date)
}

/**
 * The offset nearest `days` from `today` that lands on a trading day: 60
 * when T+60 is a trading day, else 59 or 61, then 58 or 62, and so on. A
 * tie (a Wednesday holiday) goes to the earlier day. Never under 1.
 */
export function nearestTradingOffset(today: IsoDate, days: number): number {
  for (let step = 0; step <= 7; step += 1) {
    for (const candidate of [days - step, days + step]) {
      if (candidate >= 1 && isTradingDay(addDays(today, candidate))) return candidate
    }
  }
  return days
}
