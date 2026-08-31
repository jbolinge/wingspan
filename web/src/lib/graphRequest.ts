/** Translates the persisted graph settings into the frozen request shapes. */

import type { RiskGraphRequest, WhatIfRequest } from '../api/types'
import type { GraphSettings } from '../store/appStore'
import { todayIso } from './dates'

export function buildRiskGraphRequest(
  tradeIds: readonly string[],
  graph: GraphSettings,
): RiskGraphRequest {
  let priceRange: RiskGraphRequest['price_range'] = null
  if (graph.rangeMode === 'abs' && graph.rangeMin !== null && graph.rangeMax !== null) {
    priceRange = { min: graph.rangeMin, max: graph.rangeMax }
  } else if (graph.rangeMode === 'pct') {
    priceRange = { pct: graph.rangePct }
  }
  return {
    trade_ids: [...tradeIds],
    price_range: priceRange,
    points: graph.points,
    dates: graph.dates,
    vol_shift: graph.volShift,
    vol_shift_by_expiry: graph.volShiftByExpiry,
    spot_override: graph.spotOverride,
    include_realized: graph.includeRealized,
  }
}

export function buildWhatIfRequest(
  tradeIds: readonly string[],
  graph: GraphSettings,
  spot: number | null,
): WhatIfRequest | null {
  if (tradeIds.length === 0) return null
  const price = graph.whatIf.price ?? graph.spotOverride ?? spot
  if (price === null || !Number.isFinite(price)) return null
  return {
    trade_ids: [...tradeIds],
    price,
    date: graph.whatIf.date ?? todayIso(),
    vol_shift: graph.whatIf.volShift,
    // Must track the same toggle as the curves, or the panel's P&L and the
    // T+0 line beside it disagree by the trade's realised P&L and fees.
    include_realized: graph.includeRealized,
  }
}

/** Preset T+x ladders offered in the date editor. */
export function presetDates(frontDte: number | null, frontExpiry: string | null): string[] | null {
  if (frontDte === null || frontDte <= 1) return null
  const fractions = [0, 0.25, 0.5, 0.75]
  const dates = fractions.map((f) => {
    const days = Math.round(frontDte * f)
    return days === 0 ? 'today' : `+${days}d`
  })
  dates.push(`+${Math.max(1, frontDte - 1)}d`)
  if (frontExpiry) dates.push(`expiry:${frontExpiry}`)
  return [...new Set(dates)]
}

/** Even ladder of `count` steps out to the front expiry. */
export function evenLadder(frontDte: number | null, count: number): string[] | null {
  if (frontDte === null || frontDte <= 1) return null
  const out = ['today']
  for (let i = 1; i <= count; i += 1) {
    out.push(`+${Math.max(1, Math.round((frontDte * i) / (count + 1)))}d`)
  }
  return [...new Set(out)]
}
