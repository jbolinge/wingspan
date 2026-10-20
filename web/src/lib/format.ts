/** Display formatters. Pure, exhaustively unit-tested: every number the trader
 *  reads goes through here so the whole surface is consistent. */

const EM_DASH = '—'

export interface MoneyOptions {
  /** Decimal places (default 2; pass 0 for column-dense money). */
  dp?: number
  /** Prefix positives with `+`. */
  signed?: boolean
  /** Show a `$`. */
  currency?: boolean
}

export function isFiniteNumber(value: unknown): value is number {
  return typeof value === 'number' && Number.isFinite(value)
}

function group(value: number, dp: number): string {
  return value.toLocaleString('en-US', {
    minimumFractionDigits: dp,
    maximumFractionDigits: dp,
  })
}

/** `-1234.5` -> `-$1,234.50` (or `+$1,234.50` when `signed`). */
export function fmtMoney(value: number | null | undefined, options: MoneyOptions = {}): string {
  if (!isFiniteNumber(value)) return EM_DASH
  const { dp = 2, signed = false, currency = true } = options
  const rounded = Number(value.toFixed(dp))
  const magnitude = group(Math.abs(rounded), dp)
  const sign = rounded < 0 ? '-' : signed && rounded > 0 ? '+' : ''
  return `${sign}${currency ? '$' : ''}${magnitude}`
}

/** Signed money — the default for P&L cells. */
export function fmtPnl(value: number | null | undefined, dp = 2): string {
  return fmtMoney(value, { dp, signed: true })
}

/** `0.056` is NOT a percent here: the API already sends `5.6` for 5.6%. */
export function fmtPct(value: number | null | undefined, dp = 1): string {
  if (!isFiniteNumber(value)) return EM_DASH
  const sign = value > 0 ? '+' : ''
  return `${sign}${group(value, dp)}%`
}

/** Decimal fraction -> percent, e.g. `0.1712` -> `17.1%` (IV, PoP). */
export function fmtRatioPct(value: number | null | undefined, dp = 1): string {
  if (!isFiniteNumber(value)) return EM_DASH
  return `${group(value * 100, dp)}%`
}

export function fmtNumber(value: number | null | undefined, dp = 2): string {
  if (!isFiniteNumber(value)) return EM_DASH
  return group(value, dp)
}

/** Option price points, e.g. a mark of `22.10`. */
export function fmtPrice(value: number | null | undefined, dp = 2): string {
  if (!isFiniteNumber(value)) return EM_DASH
  return value.toFixed(dp)
}

/**
 * An option quote — bid, ask or mark — at the precision it carries.
 *
 * `tenth` is a proxied XSP contract, whose quote is the SPXW quote ÷ 10
 * exactly: 19.35 becomes 1.935, a third decimal that `toFixed(2)` would round
 * away (and 0.245 → 0.24 or 0.25 depending on the float), so the ticket's
 * price × qty × 100 would miss its own net premium. Such a quote prints at
 * 3 dp whenever its third decimal is non-zero, and at 2 otherwise (1.94, not
 * 1.940). Rounded through a scaled integer, not raw `toFixed`, so 0.245
 * prints 0.245 however it is stored. Without `tenth` this is `fmtPrice`.
 */
export function fmtQuote(value: number | null | undefined, options: { tenth?: boolean } = {}): string {
  if (!isFiniteNumber(value)) return EM_DASH
  if (!options.tenth) return fmtPrice(value)
  const milli = Math.round(value * 1000)
  if (milli % 10 !== 0) return (milli / 1000).toFixed(3)
  return (milli / 1000).toFixed(2)
}

export function fmtQty(value: number | null | undefined): string {
  if (!isFiniteNumber(value)) return EM_DASH
  return value > 0 ? `+${value}` : String(value)
}

export type GreekKey = 'delta' | 'gamma' | 'theta' | 'vega'

const GREEK_DP: Record<GreekKey, number> = { delta: 2, gamma: 4, theta: 0, vega: 0 }

/** Position greeks: delta/gamma get decimals, theta/vega are dollars-ish. */
export function fmtGreek(key: GreekKey, value: number | null | undefined): string {
  if (!isFiniteNumber(value)) return EM_DASH
  return group(value, GREEK_DP[key])
}

/** Per-contract greeks need more resolution than position greeks — except
 *  delta, which reads at two decimals everywhere but the option chain.
 *
 *  `tenth` is an XSP leg, whose SPX-point greeks are one tenth of an SPX
 *  leg's: delta and gamma get one more decimal, so the row keeps the same
 *  significant digits (−0.007 rather than a misleading −0.01). */
export function fmtLegGreek(key: GreekKey, value: number | null | undefined, tenth = false): string {
  if (!isFiniteNumber(value)) return EM_DASH
  const dp = (key === 'gamma' ? 5 : 2) + (tenth && (key === 'delta' || key === 'gamma') ? 1 : 0)
  return group(value, dp)
}

/** `95` -> `95s`, `640` -> `11m`, `9000` -> `2.5h`, `200000` -> `2.3d`. */
export function fmtAge(seconds: number | null | undefined): string {
  if (!isFiniteNumber(seconds) || seconds < 0) return EM_DASH
  if (seconds < 90) return `${Math.round(seconds)}s`
  const minutes = seconds / 60
  if (minutes < 90) return `${Math.round(minutes)}m`
  const hours = minutes / 60
  if (hours < 36) return `${hours.toFixed(1)}h`
  return `${(hours / 24).toFixed(1)}d`
}

/** Local wall-clock time of an instant, e.g. `14:30:02`. */
export function fmtClock(instant: string | null | undefined): string {
  if (!instant) return EM_DASH
  const d = new Date(instant)
  if (Number.isNaN(d.getTime())) return EM_DASH
  return d.toLocaleTimeString('en-GB', { hour12: false })
}

export type Staleness = 'fresh' | 'aging' | 'stale' | 'dead'

/** Drives the per-row staleness dot. Thresholds: 1 min / 15 min / 2 h. */
export function stalenessLevel(seconds: number | null | undefined): Staleness {
  if (!isFiniteNumber(seconds)) return 'dead'
  if (seconds < 60) return 'fresh'
  if (seconds < 900) return 'aging'
  if (seconds < 7200) return 'stale'
  return 'dead'
}

/** Tailwind class for a P&L number. Neutral (not green) at exactly zero. */
export function pnlColorClass(value: number | null | undefined): string {
  if (!isFiniteNumber(value) || value === 0) return 'text-muted'
  return value > 0 ? 'text-pos' : 'text-neg'
}

export { EM_DASH }
