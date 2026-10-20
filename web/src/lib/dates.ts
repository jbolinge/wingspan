/** Naive-date helpers. All dates are `YYYY-MM-DD` strings handled in UTC so a
 *  browser timezone can never shift an expiry by a day. */

import type { IsoDate } from '../api/types'

export const MS_PER_DAY = 86_400_000

/** Parse `YYYY-MM-DD` into a UTC-midnight Date. */
export function parseIsoDate(date: IsoDate): Date {
  const [y, m, d] = date.split('-').map(Number)
  return new Date(Date.UTC(y ?? 1970, (m ?? 1) - 1, d ?? 1))
}

export function toIsoDate(date: Date): IsoDate {
  return date.toISOString().slice(0, 10)
}

export function todayIso(now: Date = new Date()): IsoDate {
  return toIsoDate(new Date(Date.UTC(now.getFullYear(), now.getMonth(), now.getDate())))
}

const NEW_YORK_DAY = new Intl.DateTimeFormat('en-US', {
  timeZone: 'America/New_York',
  year: 'numeric',
  month: '2-digit',
  day: '2-digit',
})

/**
 * The New York calendar day of an instant — the day the server values on —
 * or null for a missing or unparseable one. Unlike the UTC date
 * (`valuationDateOf`), it never runs a day ahead in the evening, so an
 * expiration dated today is still today's until New York's midnight.
 */
export function newYorkDateOf(instant: string | null | undefined): IsoDate | null {
  if (!instant) return null
  const at = new Date(instant)
  if (Number.isNaN(at.getTime())) return null
  const parts = Object.fromEntries(NEW_YORK_DAY.formatToParts(at).map((part) => [part.type, part.value]))
  return parts.year && parts.month && parts.day ? `${parts.year}-${parts.month}-${parts.day}` : null
}

export function addDays(date: IsoDate, days: number): IsoDate {
  return toIsoDate(new Date(parseIsoDate(date).getTime() + days * MS_PER_DAY))
}

/** Calendar days from `from` to `to` (negative when `to` is earlier). */
export function daysBetween(from: IsoDate, to: IsoDate): number {
  return Math.round((parseIsoDate(to).getTime() - parseIsoDate(from).getTime()) / MS_PER_DAY)
}

/** ACT/365 year fraction, floored at zero. */
export function yearFraction(from: IsoDate, to: IsoDate): number {
  return Math.max(0, daysBetween(from, to) / 365)
}

const MONTHS = [
  'Jan',
  'Feb',
  'Mar',
  'Apr',
  'May',
  'Jun',
  'Jul',
  'Aug',
  'Sep',
  'Oct',
  'Nov',
  'Dec',
] as const

/** `2027-02-19` -> `19 FEB 27` (the TOS way of writing an expiry). */
export function formatExpiry(date: IsoDate): string {
  const d = parseIsoDate(date)
  const mon = MONTHS[d.getUTCMonth()] ?? '???'
  return `${d.getUTCDate()} ${mon.toUpperCase()} ${String(d.getUTCFullYear()).slice(2)}`
}

/** `2027-02-19` -> `FEB27` (compact structure/column form). */
export function formatExpiryShort(date: IsoDate): string {
  const d = parseIsoDate(date)
  const mon = MONTHS[d.getUTCMonth()] ?? '???'
  return `${mon.toUpperCase()}${String(d.getUTCFullYear()).slice(2)}`
}

/** `2026-08-31` -> `Aug 31 2026`. */
export function formatDate(date: IsoDate | null | undefined): string {
  if (!date) return '—'
  const d = parseIsoDate(date)
  const mon = MONTHS[d.getUTCMonth()] ?? '???'
  return `${mon} ${d.getUTCDate()} ${d.getUTCFullYear()}`
}
