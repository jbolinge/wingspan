/**
 * What a risk-graph line is called, and what day the server says it is.
 *
 * Both come from the response, never from the browser clock. The server
 * values everything on its New York date; the browser's local day disagrees
 * with it for part of every evening (and a pinned or stale snapshot can be a
 * day or more behind the wall clock), so a label recomputed from
 * `todayIso()` read "T+16" for the server's `t+15` line — or a negative T+x
 * for an old snapshot — and every toggle, inset row and ghost label with it.
 * Kept apart from `chartOption` so `readout` can name a line without an
 * import cycle.
 */

import type { IsoDate, RiskGraphRequest, RiskLine } from '../api/types'
import { addDays, formatDate } from './dates'

/** `t+12` → 12; null for any other key. */
export function tplusOffset(key: string): number | null {
  const match = /^t\+(\d+)$/.exec(key)
  return match ? Number(match[1]) : null
}

/**
 * The label every surface shows for a line: `T+0`, the server's own `T+N`
 * (read off the line key, so it is the offset the server priced at), and
 * `EXP <date>` for an expiration line.
 */
export function lineLabel(line: Pick<RiskLine, 'kind' | 'key' | 'label' | 'date'>): string {
  if (line.kind === 'expiry') return `EXP ${formatDate(line.date)}`
  if (line.kind === 't0') return 'T+0'
  const offset = tplusOffset(line.key)
  if (offset !== null) return `T+${offset}`
  return line.label || formatDate(line.date)
}

/**
 * The server's valuation date (its New York calendar day) as a response
 * states it: the T+0 line's date, or any T+N line's date less N. Null when
 * the response has neither — a date list of expiration lines alone says
 * nothing about today.
 */
export function serverDateOf(
  lines: readonly Pick<RiskLine, 'kind' | 'key' | 'date'>[] | null | undefined,
): IsoDate | null {
  if (!lines) return null
  const t0 = lines.find((line) => line.kind === 't0')
  if (t0) return t0.date
  for (const line of lines) {
    if (line.kind !== 'tplus') continue
    const offset = tplusOffset(line.key)
    if (offset !== null) return addDays(line.date, -offset)
  }
  return null
}

/**
 * The expirations still live on `today` (the server's date): an expiry
 * before it has settled. The preset ladder, the "all expiries" list and the
 * per-expiry vol sliders all count from the live front, as the engine does —
 * a settled expiry left in a trade's legs would otherwise make the front
 * DTE negative and the ladder vanish.
 *
 * An expiry dated `today` is live until its settlement instant (09:30 ET
 * for AM, the close for PM), which only the server knows: once the engine's
 * live front (`RiskGraphResponse.front_expiry`, `serverFront`) is later than
 * today, today's expiry has settled and is dropped too. Without it (before
 * the first answer) today's expiry counts as live.
 */
export function liveExpiries(
  expiries: readonly IsoDate[],
  today: IsoDate,
  serverFront?: IsoDate | null,
): IsoDate[] {
  const settledToday = serverFront != null && serverFront > today
  return expiries.filter((expiry) => expiry > today || (expiry === today && !settledToday))
}

/**
 * What the live front of a risk-graph request depends on: which trades and
 * which modeled legs it draws. A vol, date or range change keeps the key;
 * a selection change does not.
 */
export function selectionKey(
  request: Pick<RiskGraphRequest, 'trade_ids' | 'model_legs'> | null,
): string | null {
  return request === null
    ? null
    : JSON.stringify({ trade_ids: [...request.trade_ids].sort(), model_legs: request.model_legs })
}

/**
 * The server's live front for the selection on screen, or `undefined` when
 * the answer in hand is another selection's — a placeholder kept across a
 * selection change, or the last answer while the new request debounces.
 * Trusting that front would drop a still-live expiry dated today whenever
 * the previous selection's front was later (`liveExpiries`).
 */
export function frontForSelection(
  answer: { front_expiry?: IsoDate | null } | undefined,
  answeredFor: string | null | undefined,
  current: string | null,
): IsoDate | null | undefined {
  return answer !== undefined && answeredFor != null && answeredFor === current
    ? answer.front_expiry
    : undefined
}
