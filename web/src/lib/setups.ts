/**
 * Trade setups: named structures the Model tab builds from the chain in one
 * click. A setup is data — its legs (right, quantity, target delta, which
 * expiry slot), its slots (target DTE, the DTE window and the expiration
 * kinds it may use, and for a diagonal's long leg the DTE ratio it must
 * keep to another slot), and which legs form a balanced butterfly — and
 * `pickSetupExpiries` + `fitSetup` find its best fit on the listed
 * expirations and their chains. The two M200 tranches and the STT are the
 * setups so far; adding another is adding an entry to `SETUPS`.
 *
 * Fitting rules, from the M200 trade plan (Werner, Trading Dominion, 2025)
 * and the STT's entry rules:
 *
 * - A slot takes the listed expiration nearest its target DTE (a tie — an
 *   AM monthly and its PM twin — goes to the AM one), among the kinds it
 *   allows (the STT enters regular monthlies only) and inside its DTE
 *   window (160–200 for the STT); with none there the pick fails and says
 *   so. A slot with a ratio keeps to the expirations whose DTE ÷ the base
 *   slot's DTE is inside [min, max], nearest its target among them; with
 *   none inside, the nearest later expiration, and the fit says so.
 * - A leg takes the strike whose |Δ| (our solved per-contract delta) is
 *   nearest its target, among strikes with a mark and greeks. A
 *   broken-wing butterfly is three such legs: each strike on its own delta,
 *   whatever the widths come out as.
 * - A balanced butterfly's wings sit at one width above and below its
 *   body. Every body within the delta tolerance of its target is tried,
 *   each with every width both sides list; the fly with the smallest total
 *   delta error (body and both wings) wins — so a body that happens to sit
 *   on the fine strike grid near the money, whose matching wings are not
 *   listed on the coarse grid further out, does not collapse the fly.
 * - Two legs fitted to the same contract is not a trade (the store would
 *   net them away); the fit refuses, naming them.
 * - Net Δ is reported in the plan's own delta points — Σ qty × Δ × 100,
 *   the ticket's Model Δ — against the plan's bound; outside it is a
 *   warning, not a refusal.
 */

import type { ChainExpiry, ChainQuote, ChainRow, ExpiryKind, OptionChainResponse, Right } from '../api/types'
import { expiryKey, fmtSectionDate, fmtStrike } from './model'

export interface SetupSlot {
  /** Calendar days to expiry the slot aims for. */
  targetDte: number
  /** Never an expiration under this many DTE ("not less than 160"). */
  minDte?: number
  /** Never an expiration over this many DTE. */
  maxDte?: number
  /** Only expirations listed as one of these kinds — `['monthly']` for the
   *  regular third-Friday expiration, no weeklys or quarterlies. Unset,
   *  every listed expiration is a candidate. */
  kinds?: readonly ExpiryKind[]
  /** Keep this slot's DTE within [min, max] × the DTE of slot `slot`. */
  ratioTo?: { slot: string; min: number; max: number }
}

export interface SetupLeg {
  /** The plan's own short name: ULP, SP, LLP, LC, SC. */
  id: string
  /** Spoken: "upper long put". */
  name: string
  right: Right
  /** Signed: +2 buys two, −4 sells four. */
  qty: number
  /** Target |Δ| per contract, as a fraction: 0.35 for "Δ35". */
  delta: number
  slot: string
}

/** Three legs of one right at one width above and below the body. */
export interface SetupFly {
  upper: string
  body: string
  lower: string
}

export interface TradeSetup {
  id: string
  /** The button: "M200 CD". */
  label: string
  /** The plan's full name. */
  name: string
  /** One sentence of construction, the button's tooltip. */
  summary: string
  slots: Record<string, SetupSlot>
  legs: SetupLeg[]
  /** Balanced butterflies among the legs; a broken-wing fly's legs are
   *  fitted one by one and are not listed here. */
  flies: SetupFly[]
  /** |Σ qty × Δ| the plan allows, in delta points (Δ35 = 35): the plan's
   *  "combo total Δ between −1 and +1" is ±1 of these. */
  maxNetDelta?: number
}

/** The put butterfly both M200 variants share: +2 Δ35 / −4 Δ24 / +2 Δ16. */
const M200_FLY: SetupLeg[] = [
  { id: 'ULP', name: 'upper long put', right: 'put', qty: 2, delta: 0.35, slot: 'fly' },
  { id: 'SP', name: 'short put', right: 'put', qty: -4, delta: 0.24, slot: 'fly' },
  { id: 'LLP', name: 'lower long put', right: 'put', qty: 2, delta: 0.16, slot: 'fly' },
]

export const M200_CDS: TradeSetup = {
  id: 'm200-cds',
  label: 'M200 CDS',
  name: 'M200 butterfly + call debit spread',
  summary:
    'Put butterfly +2 Δ35 / −4 Δ24 / +2 Δ16 at ~200 DTE, balanced wings, plus a call debit spread +1 Δ25 / −1 Δ18 in the same expiration. Combo delta within ±1. Replaces the model.',
  slots: { fly: { targetDte: 200 } },
  legs: [
    ...M200_FLY,
    { id: 'LC', name: 'long call', right: 'call', qty: 1, delta: 0.25, slot: 'fly' },
    { id: 'SC', name: 'short call', right: 'call', qty: -1, delta: 0.18, slot: 'fly' },
  ],
  flies: [{ upper: 'ULP', body: 'SP', lower: 'LLP' }],
  maxNetDelta: 1,
}

export const M200_CD: TradeSetup = {
  id: 'm200-cd',
  label: 'M200 CD',
  name: 'M200 butterfly + call diagonal',
  summary:
    'Put butterfly +2 Δ35 / −4 Δ24 / +2 Δ16 at ~200 DTE, balanced wings, plus a call diagonal: −1 Δ27.5 in that expiration and +1 Δ32.5 at ~250 DTE, the long call 1.05–1.4× the short call’s DTE. Replaces the model.',
  slots: {
    fly: { targetDte: 200 },
    long: { targetDte: 250, ratioTo: { slot: 'fly', min: 1.05, max: 1.4 } },
  },
  legs: [
    ...M200_FLY,
    { id: 'SC', name: 'short call', right: 'call', qty: -1, delta: 0.275, slot: 'fly' },
    { id: 'LC', name: 'long call', right: 'call', qty: 1, delta: 0.325, slot: 'long' },
  ],
  flies: [{ upper: 'ULP', body: 'SP', lower: 'LLP' }],
}

/**
 * The STT: a broken-wing put butterfly +1 Δ25 / −2 Δ15 / +1 Δ5, every
 * strike on its own delta, entered in the regular monthly expiration
 * 160–200 DTE nearest 160 — never a weekly, never under 160.
 */
export const STT: TradeSetup = {
  id: 'stt',
  label: 'STT',
  name: 'STT broken-wing put butterfly',
  summary:
    'Broken-wing put butterfly +1 Δ25 / −2 Δ15 / +1 Δ5, each strike on its own delta, in the regular monthly expiration 160–200 DTE nearest 160 (no weeklys). Replaces the model.',
  slots: { fly: { targetDte: 160, minDte: 160, maxDte: 200, kinds: ['monthly'] } },
  legs: [
    { id: 'ULP', name: 'upper long put', right: 'put', qty: 1, delta: 0.25, slot: 'fly' },
    { id: 'SP', name: 'short put', right: 'put', qty: -2, delta: 0.15, slot: 'fly' },
    { id: 'LLP', name: 'lower long put', right: 'put', qty: 1, delta: 0.05, slot: 'fly' },
  ],
  flies: [],
}

/** Every setup the panel offers, in button order. */
export const SETUPS: readonly TradeSetup[] = [M200_CD, M200_CDS, STT]

export interface PickedExpiries {
  slots: Record<string, ChainExpiry>
  warnings: string[]
}

/** A leg's delta is "on target" within this; further off, the fit warns. */
const DELTA_TOLERANCE = 0.05
/** A slot's DTE is close enough to its target within this many days. */
const DTE_TOLERANCE = 45

/** A delta as the plan writes it — Δ35, Δ27.5 — to a tenth of a point. */
function fmtDelta(delta: number): string {
  const points = Math.round(Math.abs(delta) * 1000) / 10
  return `Δ${Number.isInteger(points) ? points : points.toFixed(1)}`
}

export function fmtSlotExpiry(expiry: ChainExpiry): string {
  return `${fmtSectionDate(expiry.expiry)} · ${expiry.dte} DTE`
}

/** The candidate nearest `target` DTE; the same day twice (an AM monthly
 *  and its PM twin, however the provider labels them) goes to the AM one,
 *  then to the monthly, wherever they sit in the listing. */
function nearestDte(candidates: readonly ChainExpiry[], target: number): ChainExpiry | null {
  let best: ChainExpiry | null = null
  for (const candidate of candidates) {
    if (best === null) {
      best = candidate
      continue
    }
    const gap = Math.abs(candidate.dte - target)
    const bestGap = Math.abs(best.dte - target)
    if (gap < bestGap) best = candidate
    else if (gap === bestGap) {
      if (candidate.settlement === 'am' && best.settlement !== 'am') best = candidate
      else if (candidate.settlement === best.settlement && candidate.kind === 'monthly' && best.kind !== 'monthly')
        best = candidate
    }
  }
  return best
}

/** What a slot will take, for a failed pick: `monthly expiration 160–200 DTE`. */
function describeSlot(slot: SetupSlot): string {
  const kinds = slot.kinds === undefined ? '' : `${slot.kinds.join('/')} `
  const window =
    slot.minDte !== undefined && slot.maxDte !== undefined
      ? ` ${slot.minDte}–${slot.maxDte} DTE`
      : slot.minDte !== undefined
        ? ` of ${slot.minDte} DTE or more`
        : slot.maxDte !== undefined
          ? ` of ${slot.maxDte} DTE or less`
          : ''
  return `${kinds}expiration${window}`
}

/**
 * One listed expiration per slot. Slots without a ratio go first, so a
 * ratio slot always has its base to measure against; a slot whose base is
 * missing from the setup is a setup bug and fails the pick.
 */
export function pickSetupExpiries(
  setup: TradeSetup,
  expirations: readonly ChainExpiry[],
): { ok: true; picked: PickedExpiries } | { ok: false; reason: string } {
  const live = expirations.filter((e) => e.dte > 0)
  if (live.length === 0)
    return { ok: false, reason: expirations.length === 0 ? 'No expirations are listed.' : 'No expiration after today is listed.' }
  const slots: Record<string, ChainExpiry> = {}
  const warnings: string[] = []
  const entries = Object.entries(setup.slots).sort(
    ([, a], [, b]) => Number(a.ratioTo !== undefined) - Number(b.ratioTo !== undefined),
  )
  for (const [id, slot] of entries) {
    // The slot's own kinds and DTE window come first: a weekly at 165 DTE
    // is not a candidate for a monthlies-only slot, nor is a monthly at
    // 137 for a slot that must not go under 160.
    const eligible = live.filter(
      (e) =>
        (slot.kinds === undefined || slot.kinds.includes(e.kind)) &&
        (slot.minDte === undefined || e.dte >= slot.minDte) &&
        (slot.maxDte === undefined || e.dte <= slot.maxDte),
    )
    if (eligible.length === 0)
      return { ok: false, reason: `No ${describeSlot(slot)} is listed for the ${id} slot.` }
    let chosen: ChainExpiry | null
    // The ratio fallback already says how far the pick is; one warning.
    let explained = false
    let band = ''
    if (slot.ratioTo === undefined) {
      chosen = nearestDte(eligible, slot.targetDte)
    } else {
      const base = slots[slot.ratioTo.slot]
      if (base === undefined) return { ok: false, reason: `Setup ${setup.label} has no "${slot.ratioTo.slot}" slot for "${id}" to follow.` }
      const { min, max } = slot.ratioTo
      band = ` inside the ${min}–${max}× band`
      // In hundredths, so 200 × 1.05 is exactly 210 and not a float short of it.
      const lo = base.dte * Math.round(min * 100)
      const hi = base.dte * Math.round(max * 100)
      chosen = nearestDte(
        eligible.filter((e) => e.dte * 100 >= lo && e.dte * 100 <= hi),
        slot.targetDte,
      )
      if (chosen === null) {
        // Later than the base, whatever the ratio: a long leg dated before
        // the short one is a different trade, not a looser fit.
        chosen = nearestDte(
          eligible.filter((e) => e.dte > base.dte),
          slot.targetDte,
        )
        if (chosen === null)
          return { ok: false, reason: `No expiration after the ${slot.ratioTo.slot}’s ${base.dte} DTE is listed for the ${id} slot.` }
        warnings.push(
          `No expiration is ${min}–${max}× the ${base.dte} DTE of the ${slot.ratioTo.slot}; ${fmtSlotExpiry(chosen)} is ${(chosen.dte / base.dte).toFixed(2)}×.`,
        )
        explained = true
      }
    }
    if (chosen === null) return { ok: false, reason: `No expiration fits the ${id} slot.` }
    if (!explained && Math.abs(chosen.dte - slot.targetDte) > DTE_TOLERANCE)
      warnings.push(`${fmtSlotExpiry(chosen)} is the nearest to the ${slot.targetDte} DTE target${band}.`)
    slots[id] = chosen
  }
  return { ok: true, picked: { slots, warnings } }
}

export interface FittedLeg {
  leg: SetupLeg
  quote: ChainQuote
  expiry: ChainExpiry
  /** The contract's signed per-contract delta. */
  delta: number
}

export interface SetupFit {
  setup: TradeSetup
  slots: Record<string, ChainExpiry>
  legs: FittedLeg[]
  /** Σ qty × Δ × 100 — delta points, the plan's "combo total Δ". */
  netDelta: number
  warnings: string[]
  /** Slots where a leg sits on the first or last strike the chain lists:
   *  the fit may have been stopped by the chain's window rather than by the
   *  delta, so the panel fetches the whole expiry and fits again. */
  atEdge: string[]
}

export type SetupFitResult = { ok: true; fit: SetupFit } | { ok: false; reason: string }

/** The quote of `right` on a row, when it can be modeled and has a delta. */
function fittable(row: ChainRow, right: Right): ChainQuote | null {
  const quote = right === 'call' ? row.call : row.put
  if (quote === null || quote.mark === null || quote.greeks === null) return null
  if (!Number.isFinite(quote.greeks.delta)) return null
  return quote
}

function deltaGap(quote: ChainQuote, target: number): number {
  return Math.abs(Math.abs(quote.greeks?.delta ?? Number.NaN) - target)
}

/** The fittable quote of `right` nearest the target |Δ|. */
function nearestByDelta(rows: readonly ChainRow[], right: Right, target: number): ChainQuote | null {
  let best: ChainQuote | null = null
  for (const row of rows) {
    const quote = fittable(row, right)
    if (quote === null) continue
    if (best === null || deltaGap(quote, target) < deltaGap(best, target)) best = quote
  }
  return best
}

/** The best balanced wings around `body`: of every width both sides list,
 *  the one whose two deltas are nearest their targets together. */
function bestWings(
  rows: readonly ChainRow[],
  byStrike: ReadonlyMap<number, ChainRow>,
  body: ChainQuote,
  upper: SetupLeg,
  lower: SetupLeg,
): { upper: ChainQuote; lower: ChainQuote; score: number } | null {
  const bodyStrike = body.contract.strike
  let best: { upper: ChainQuote; lower: ChainQuote; score: number } | null = null
  for (const row of rows) {
    const width = row.strike - bodyStrike
    if (width <= 0) continue
    const up = fittable(row, upper.right)
    const lowRow = byStrike.get(bodyStrike - width)
    const low = lowRow ? fittable(lowRow, lower.right) : null
    if (up === null || low === null) continue
    const score = deltaGap(up, upper.delta) + deltaGap(low, lower.delta)
    if (best === null || score < best.score) best = { upper: up, lower: low, score }
  }
  return best
}

/**
 * Fit every leg of `setup` on the chains of its slots (`chains` keyed by
 * slot id, each the chain of the expiration `slots` picked for it).
 */
export function fitSetup(
  setup: TradeSetup,
  slots: Record<string, ChainExpiry>,
  chains: Record<string, OptionChainResponse>,
  pickWarnings: readonly string[] = [],
): SetupFitResult {
  const warnings = [...pickWarnings]
  const legById = new Map(setup.legs.map((leg) => [leg.id, leg]))
  const chosen = new Map<string, ChainQuote>()
  const inFly = new Set(setup.flies.flatMap((fly) => [fly.upper, fly.body, fly.lower]))

  const rowsOf = (slot: string): readonly ChainRow[] | null => chains[slot]?.rows ?? null
  const slotName = (slot: string): string => {
    const expiry = slots[slot]
    return expiry ? fmtSlotExpiry(expiry) : slot
  }

  // Single legs, by delta.
  for (const leg of setup.legs) {
    if (inFly.has(leg.id)) continue
    const rows = rowsOf(leg.slot)
    if (rows === null) return { ok: false, reason: `The ${slotName(leg.slot)} chain did not load.` }
    const quote = nearestByDelta(rows, leg.right, leg.delta)
    if (quote === null) return { ok: false, reason: `No ${leg.right} with a mark and a delta for the ${leg.name}.` }
    chosen.set(leg.id, quote)
  }

  // Butterflies: every body near its delta, each with its best wings; the
  // fly with the least delta error over all three legs.
  for (const fly of setup.flies) {
    const body = legById.get(fly.body)
    const upper = legById.get(fly.upper)
    const lower = legById.get(fly.lower)
    if (!body || !upper || !lower) return { ok: false, reason: `Setup ${setup.label} names a butterfly leg it does not have.` }
    const rows = rowsOf(body.slot)
    if (rows === null) return { ok: false, reason: `The ${slotName(body.slot)} chain did not load.` }
    const nearest = nearestByDelta(rows, body.right, body.delta)
    if (nearest === null) return { ok: false, reason: `No ${body.right} with a mark and a delta for the ${body.name}.` }
    const byStrike = new Map<number, ChainRow>(rows.map((row) => [row.strike, row]))
    const bodies = rows
      .map((row) => fittable(row, body.right))
      .filter((quote): quote is ChainQuote => quote !== null && deltaGap(quote, body.delta) <= DELTA_TOLERANCE)
    if (bodies.length === 0) bodies.push(nearest)
    let best: { body: ChainQuote; upper: ChainQuote; lower: ChainQuote; score: number } | null = null
    for (const candidate of bodies) {
      const wings = bestWings(rows, byStrike, candidate, upper, lower)
      if (wings === null) continue
      const score = deltaGap(candidate, body.delta) + wings.score
      if (best === null || score < best.score) best = { body: candidate, ...wings, score }
    }
    if (best === null)
      return {
        ok: false,
        reason: `No pair of equal-width ${body.right} wings is listed around the ${fmtStrike(nearest.contract.strike)} ${body.name}.`,
      }
    chosen.set(fly.body, best.body)
    chosen.set(fly.upper, best.upper)
    chosen.set(fly.lower, best.lower)
  }

  // Two legs on one contract would net to nothing in the model: the chain
  // ran out of strikes before the deltas did, or the targets are too close
  // on this grid. Either way it is not the trade.
  const byContract = new Map<string, SetupLeg>()
  for (const leg of setup.legs) {
    const quote = chosen.get(leg.id)
    if (quote === undefined) continue
    const other = byContract.get(quote.contract.tos_code)
    if (other !== undefined)
      return {
        ok: false,
        reason: `The ${other.name} and the ${leg.name} both fit ${fmtStrike(quote.contract.strike)}: no strike further out is listed on the ${slotName(leg.slot)} chain.`,
      }
    byContract.set(quote.contract.tos_code, leg)
  }

  const legs: FittedLeg[] = []
  const atEdge = new Set<string>()
  let netDelta = 0
  for (const leg of setup.legs) {
    const quote = chosen.get(leg.id)
    const expiry = slots[leg.slot]
    const rows = rowsOf(leg.slot)
    if (quote === undefined || expiry === undefined || rows === null) return { ok: false, reason: `The ${leg.name} was not fitted.` }
    const delta = quote.greeks?.delta ?? 0
    const gap = Math.abs(Math.abs(delta) - leg.delta)
    if (gap > DELTA_TOLERANCE)
      warnings.push(
        `${leg.id} ${fmtStrike(quote.contract.strike)} is ${fmtDelta(delta)}, ${gap.toFixed(2)} off its ${fmtDelta(leg.delta)} target.`,
      )
    const strike = quote.contract.strike
    if (rows.length > 0 && (strike === rows[0]?.strike || strike === rows[rows.length - 1]?.strike)) atEdge.add(leg.slot)
    netDelta += leg.qty * delta * 100
    legs.push({ leg, quote, expiry, delta })
  }
  if (setup.maxNetDelta !== undefined && Math.abs(netDelta) > setup.maxNetDelta)
    warnings.push(`Net Δ ${fmtNetDelta(netDelta)} is outside ±${setup.maxNetDelta}.`)
  return { ok: true, fit: { setup, slots, legs, netDelta, warnings, atEdge: [...atEdge] } }
}

/** Net delta in plan points, signed, to a tenth: `+1.0`, `−2.3`. */
export function fmtNetDelta(netDelta: number): string {
  const rounded = Math.round(netDelta * 10) / 10
  return `${rounded < 0 ? '−' : '+'}${Math.abs(rounded).toFixed(1)}`
}

/**
 * The fit in one line per slot — `19 Mar 2027 · 200 DTE: +2 7000P Δ35 / −4
 * 7400P Δ24 / +2 7800P Δ16 / +1 8100C Δ25 / −1 8300C Δ18` — the status
 * under the buttons.
 */
export function fitLines(fit: SetupFit): string[] {
  const lines: string[] = []
  for (const [slot, expiry] of Object.entries(fit.slots)) {
    const legs = fit.legs
      .filter((fitted) => fitted.leg.slot === slot)
      .map(
        (fitted) =>
          `${fitted.leg.qty > 0 ? '+' : '−'}${Math.abs(fitted.leg.qty)} ${fmtStrike(fitted.quote.contract.strike)}${
            fitted.leg.right === 'call' ? 'C' : 'P'
          } ${fmtDelta(fitted.delta)}`,
      )
    lines.push(`${fmtSlotExpiry(expiry)}: ${legs.join(' / ')}`)
  }
  return lines
}

/** The section key of the expiration the first leg sits in: the chain to open. */
export function fitOpensSection(fit: SetupFit): string | null {
  const first = fit.legs[0]
  return first ? expiryKey(first.expiry) : null
}
