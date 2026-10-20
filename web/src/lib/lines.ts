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

import type { IsoDate, RiskGraphRequest, RiskGraphResponse, RiskLine, RiskTerminal } from '../api/types'
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

/**
 * The identity a line toggle hides a line by: its key, except for an
 * expiration line, which is hidden by its DATE (`expiry:<date>`), and a T+N
 * line, which is hidden by its key AND its date (`t+N@<date>`).
 *
 * The bare `expiry` key is the live front expiry's line, so it moves with
 * the selection: hiding "the SPX Oct expiry" by key would
 * hide the XSP Dec line the moment Oct left the set, and the Oct line would
 * come back visible when it returned. A T+N key is an offset from the
 * server's day, so it means the same day whatever is selected — but only
 * for that day: the default ladder's offsets follow the front's DTE and move
 * daily, so a bare `t+38` would later hide whichever line lands on 38. With
 * its date the key names one day's line exactly, and `liveHiddenLines`
 * retires it once the server's day has moved on. `t0` is always "now".
 */
export function hideKey(line: Pick<RiskLine, 'kind' | 'key' | 'date'>): string {
  if (line.kind === 'expiry') return `expiry:${line.date}`
  if (line.kind === 'tplus') return `${line.key}@${line.date}`
  return line.key
}

/** Whether the trader has switched `line` off. */
export function isLineHidden(
  line: Pick<RiskLine, 'kind' | 'key' | 'date'>,
  hiddenLines: readonly string[],
): boolean {
  return hiddenLines.includes(hideKey(line))
}

/**
 * The line the header's max profit, max loss, PoP and BE describe: the
 * server's `terminal` (additive), or — from an answer that predates it — the
 * canonical `expiry` line, else the last line drawn (the rule before the
 * server sent `terminal`). Null with no lines at all.
 */
export function terminalOf(data: Pick<RiskGraphResponse, 'lines' | 'terminal'>): RiskTerminal | null {
  if (data.terminal) return data.terminal
  const line = data.lines.find((l) => l.key === 'expiry') ?? data.lines[data.lines.length - 1]
  return line ? { key: line.key, date: line.date, kind: line.kind, drawn: true, analysis_breakevens: [] } : null
}

/** One breakeven the header shows: `offRange` when it lies outside the
 *  base response's drawn prices (read off the analysis axis). */
export interface HeaderBreakeven {
  value: number
  offRange: boolean
}

/**
 * The header's breakevens of `terminal`: the drawn range's crossings
 * (`breakevens[terminal.key]`), unless the terminal's analysis axis
 * (`analysis_breakevens`, any range) holds more — a narrow window that
 * misses a crossing — in which case the analysis-axis ones, each flagged
 * when it lies outside the drawn prices.
 */
export function headerBreakevens(
  data: Pick<RiskGraphResponse, 'breakevens' | 'prices'>,
  terminal: RiskTerminal | null,
): HeaderBreakeven[] {
  const display = data.breakevens[terminal?.key ?? 'expiry'] ?? []
  const analysis = terminal?.analysis_breakevens ?? []
  if (analysis.length <= display.length || data.prices.length === 0) {
    return display.map((value) => ({ value, offRange: false }))
  }
  const lo = data.prices[0]!
  const hi = data.prices[data.prices.length - 1]!
  return analysis.map((value) => ({ value, offRange: value < lo || value > hi }))
}

/** `at EXP Dec 18 2026` — what the header's readouts are measured at. */
export function terminalLabel(terminal: RiskTerminal): string {
  return `at ${lineLabel({ kind: terminal.kind, key: terminal.key, label: '', date: terminal.date })}`
}

/**
 * The chart's accessible summary of the lines it draws. "T+x" counts T+0 as
 * the ladder's first rung (the ONE convention, and what the default reads:
 * "expiration line plus 5 T+x lines"); every expiration line is counted as
 * one, never as a T+x line, and a set with none says so.
 */
export function describeLines(lines: readonly Pick<RiskLine, 'kind'>[]): string {
  const expiries = lines.filter((line) => line.kind === 'expiry').length
  const ladder = lines.length - expiries
  const rungs = `${ladder} T+x line${ladder === 1 ? '' : 's'}`
  if (expiries === 0) return `no expiration line, ${rungs}`
  const head = expiries === 1 ? 'expiration line' : `${expiries} expiration lines`
  return `${head} plus ${rungs}`
}

/** The risk-graph warning (`info`) the engine raises for a date spec it
 *  dropped — an `expiry:<date>` whose close is already behind the snapshot,
 *  e.g. one persisted from a session before that expiry passed. The store prunes such specs itself
 *  once the server's date is known (`pruneExpiredGraphDates`); the warning is
 *  what explains a line the list named that is not drawn. */
export const DROPPED_DATE_WARNING = 'stale_graph_date'

/**
 * The date specs still drawable on `today` (the server's date): every
 * `expiry:<date>` before it is dropped — that expiration has settled and the
 * engine drops it with a `stale_graph_date` warning — and everything else is
 * kept as it is. Returns the same array when nothing was dropped.
 */
export function liveDateSpecs(specs: readonly string[], today: IsoDate): readonly string[] {
  const kept = specs.filter((spec) => !spec.startsWith('expiry:') || spec.slice('expiry:'.length) >= today)
  return kept.length === specs.length ? specs : kept
}

/**
 * The hidden-line keys that can still match a line on `today`: the legacy
 * bare `expiry` and `t+N` keys (an older blob's; `hideKey` no longer makes
 * them, and they would hide whichever line takes the key next), every
 * `expiry:<date>` before `today` (settled, never drawn again) and every
 * `t+N@<date>` laid out on another day (`<date>` is not `today` + N) are
 * dropped. A key the *current* selection does not draw is kept — a line
 * hidden on one trade stays hidden when the trader comes back to it.
 * Returns the same array when nothing was dropped.
 */
export function liveHiddenLines(keys: readonly string[], today: IsoDate): readonly string[] {
  const kept = keys.filter((key) => {
    if (key === 'expiry' || tplusOffset(key) !== null) return false
    if (key.startsWith('expiry:')) return key.slice('expiry:'.length) >= today
    const tplus = /^t\+(\d+)@(\d{4}-\d{2}-\d{2})$/.exec(key)
    return tplus === null || addDays(today, Number(tplus[1])) === tplus[2]
  })
  return kept.length === keys.length ? keys : kept
}
