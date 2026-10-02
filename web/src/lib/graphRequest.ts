/** Translates the persisted graph settings into the frozen request shapes. */

import type { ModelLeg, RiskGraphRequest, RiskLine, WhatIfRequest } from '../api/types'
import type { GraphSettings } from '../store/appStore'
import { todayIso } from './dates'

/**
 * `modelLegs` is the request shape (`toModelLegs`), already filtered by the
 * caller to what should be drawn — `[]` when the model is off or empty. It is
 * always present, so a graph without a model has one stable key shape.
 */
export function buildRiskGraphRequest(
  tradeIds: readonly string[],
  graph: GraphSettings,
  modelLegs: readonly ModelLeg[] = [],
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
    model_legs: [...modelLegs],
  }
}

/**
 * The date specs that reproduce a response's lines exactly, key for key:
 * `t0` → `today`, `t+12` → `+12d`, an expiration line → `expiry:` its date.
 *
 * What the compare request sends when the trader has no date list of their
 * own: `dates: null` lets the server pick a ladder from each request's *own*
 * front expiry, and a model that adds a nearer expiration would hand the
 * position without it a different ladder — ghost lines at T+20/41/61 behind
 * live ones at T+12/23/35, nothing to compare and no key for a toggle to
 * hide them by. Pinned to the live lines, the position is drawn at the very
 * dates the combined curves are, the model's expiration among them.
 */
export function effectiveDateSpecs(lines: readonly RiskLine[]): string[] {
  const specs = lines.map((line) => {
    if (line.kind === 'expiry') return `expiry:${line.date}`
    const offset = /^t\+(\d+)$/.exec(line.key)
    return line.kind === 't0' || !offset ? 'today' : `+${offset[1]}d`
  })
  return [...new Set(specs)]
}

export function buildWhatIfRequest(
  tradeIds: readonly string[],
  graph: GraphSettings,
  spot: number | null,
  modelLegs: readonly ModelLeg[] = [],
): WhatIfRequest | null {
  if (tradeIds.length === 0 && modelLegs.length === 0) return null
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
    // …and the same model, or the answer describes a different position.
    model_legs: [...modelLegs],
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
