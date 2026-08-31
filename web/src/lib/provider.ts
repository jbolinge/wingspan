/** Derives the status-bar provider pill from `/market/snapshot`.
 *
 *  The pill must never over-promise: the snapshot carries the sources actually
 *  used plus the reasons the preferred ones were skipped, and the worst source
 *  in play decides the state. */

import type { MarketSnapshot, QuoteSource } from '../api/types'
import { fmtAge } from './format'

export const PROVIDER_STATES = ['LIVE', 'DELAYED', 'STALE', 'OFFLINE', 'DEMO', 'UNKNOWN'] as const
export type ProviderState = (typeof PROVIDER_STATES)[number]

export type ProviderTone = 'pos' | 'info' | 'warn' | 'neg' | 'muted'

export interface ProviderPill {
  state: ProviderState
  /** Short source name, e.g. `Schwab`. */
  source: string
  /** Full pill text, e.g. `DELAYED · yfinance`. */
  label: string
  /** Parenthetical shown next to the pill, e.g. `Schwab: token file missing`. */
  detail: string | null
  /** Every degraded reason, for the tooltip. */
  reasons: string[]
  tone: ProviderTone
}

const SOURCE_LABEL: Record<QuoteSource, string> = {
  schwab: 'Schwab',
  yfinance: 'yfinance',
  cache: 'cache',
  statement: 'statement marks',
  fixture: 'fixture',
}

export function sourceLabel(source: QuoteSource | null | undefined): string {
  return source ? SOURCE_LABEL[source] : 'unknown'
}

/** Worst source wins: statement > cache > fixture > yfinance > schwab. */
const RANK: Record<QuoteSource, number> = {
  statement: 4,
  cache: 3,
  fixture: 2,
  yfinance: 1,
  schwab: 0,
}

function worstSource(snapshot: MarketSnapshot): QuoteSource {
  return RANK[snapshot.option_source] >= RANK[snapshot.spot_source]
    ? snapshot.option_source
    : snapshot.spot_source
}

export function snapshotAgeSeconds(
  snapshot: MarketSnapshot | undefined,
  now: Date = new Date(),
): number | null {
  if (!snapshot) return null
  const ms = now.getTime() - new Date(snapshot.as_of).getTime()
  return Number.isFinite(ms) ? Math.max(0, ms / 1000) : null
}

export function providerPill(
  snapshot: MarketSnapshot | undefined,
  now: Date = new Date(),
): ProviderPill {
  if (!snapshot) {
    return {
      state: 'UNKNOWN',
      source: 'unknown',
      label: 'CONNECTING…',
      detail: null,
      reasons: [],
      tone: 'muted',
    }
  }
  const reasons = snapshot.degraded ?? []
  const detail = reasons[0] ?? null
  const worst = worstSource(snapshot)
  const source = sourceLabel(worst)
  const age = snapshotAgeSeconds(snapshot, now)

  switch (worst) {
    case 'statement':
      return {
        state: 'OFFLINE',
        source,
        label: `OFFLINE · statement marks ${snapshot.as_of.slice(0, 10)}`,
        detail,
        reasons,
        tone: 'neg',
      }
    case 'cache':
      return {
        state: 'STALE',
        source,
        label: `STALE · cached ${fmtAge(age)}`,
        detail,
        reasons,
        tone: 'warn',
      }
    case 'fixture':
      return { state: 'DEMO', source, label: 'DEMO · fixture', detail, reasons, tone: 'info' }
    case 'yfinance':
      return {
        state: 'DELAYED',
        source,
        label: 'DELAYED · yfinance',
        detail,
        reasons,
        tone: 'warn',
      }
    default:
      return {
        state: reasons.length > 0 ? 'DELAYED' : 'LIVE',
        source,
        label: `${reasons.length > 0 ? 'DELAYED' : 'LIVE'} · Schwab`,
        detail,
        reasons,
        tone: reasons.length > 0 ? 'warn' : 'pos',
      }
  }
}

export const TONE_CLASS: Record<ProviderTone, string> = {
  pos: 'text-pos border-pos/50 bg-pos/10',
  info: 'text-info border-info/50 bg-info/10',
  warn: 'text-warn border-warn/50 bg-warn/10',
  neg: 'text-neg border-neg/50 bg-neg/10',
  muted: 'text-muted border-line bg-surface-2',
}
