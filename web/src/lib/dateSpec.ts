/** The frozen risk-graph date-spec vocabulary: `today`, `+Nd`,
 *  `expiry:YYYY-MM-DD`. Kept apart from the editor component so both the
 *  editor and the tests can use it. */

import type { IsoDate } from '../api/types'
import { addDays, formatExpiryShort, todayIso } from './dates'

export function dateSpecLabel(spec: string): string {
  if (spec === 'today') return 'T+0'
  if (spec.startsWith('expiry:')) return `EXP ${formatExpiryShort(spec.slice(7))}`
  const match = /^\+(\d+)d$/.exec(spec)
  if (match) return `T+${match[1]}`
  return spec
}

export function dateSpecDate(spec: string, today: IsoDate = todayIso()): IsoDate {
  if (spec === 'today') return today
  if (spec.startsWith('expiry:')) return spec.slice(7)
  const match = /^\+(\d+)d$/.exec(spec)
  return match ? addDays(today, Number(match[1])) : spec
}

/** Turn a calendar date into a spec, preferring `expiry:` when it is one. */
export function dateToSpec(date: IsoDate, expiries: readonly IsoDate[], today: IsoDate): string {
  if (expiries.includes(date)) return `expiry:${date}`
  const offset = Math.max(0, Math.round((Date.parse(date) - Date.parse(today)) / 86_400_000))
  return offset === 0 ? 'today' : `+${offset}d`
}
