import { describe, expect, it } from 'vitest'
import type { ChainExpiry, ChainQuote, ChainRow, OptionChainResponse, Right } from '../api/types'
import {
  M200_CD,
  M200_CDS,
  SETUPS,
  STT,
  fitLines,
  fitOpensSection,
  fitSetup,
  fmtNetDelta,
  pickSetupExpiries,
  type TradeSetup,
} from './setups'

function listing(expiry: string, dte: number, root = 'SPX', kind: ChainExpiry['kind'] = 'monthly'): ChainExpiry {
  return { expiry, dte, root, settlement: root === 'SPX' ? 'am' : 'pm', kind }
}

/** Dates from 2026-08-31 are irrelevant to the picker: only `dte` is read. */
const EXPIRATIONS: ChainExpiry[] = [
  listing('2026-09-04', 4, 'SPXW', 'weekly'),
  listing('2026-10-16', 46),
  listing('2027-02-19', 172),
  listing('2027-03-19', 200),
  listing('2027-03-19', 200, 'SPXW', 'weekly'),
  listing('2027-03-31', 212, 'SPXW', 'quarterly'),
  listing('2027-04-16', 228),
  listing('2027-05-21', 263),
  listing('2027-06-30', 303, 'SPXW', 'quarterly'),
]

function quote(expiry: ChainExpiry, strike: number, right: Right, delta: number | null, mark: number | null = 10): ChainQuote {
  const cp = right === 'call' ? 'C' : 'P'
  return {
    contract: {
      underlying: 'SPX',
      root: expiry.root,
      expiry: expiry.expiry,
      settlement: expiry.settlement,
      strike,
      right,
      multiplier: 100,
      osi: `SPX ${expiry.expiry} ${cp}${strike}`,
      tos_code: `${expiry.root}${expiry.expiry.replace(/-/g, '').slice(2)}${cp}${strike}`,
    },
    bid: mark,
    ask: mark,
    mark,
    last: null,
    iv: 0.2,
    iv_hint: null,
    greeks: delta === null ? null : { delta: right === 'put' ? -delta : delta, gamma: 0.001, theta: -1, vega: 5 },
    volume: null,
    open_interest: null,
    quote_as_of: '2026-08-31T16:00:00Z',
  }
}

/** A chain from `[strike, |put Δ|, call Δ]` triples; a null delta is an
 *  unsolved quote, a missing side a strike listed one-sided. */
function chain(
  expiry: ChainExpiry,
  rows: [number, number | null, number | null][],
  options: { noMark?: number[] } = {},
): OptionChainResponse {
  return {
    as_of: '2026-08-31T16:00:00Z',
    source: 'fixture',
    stale: false,
    degraded: [],
    underlying: 'SPX',
    spot: 7750,
    expiry: expiry.expiry,
    root: expiry.root,
    settlement: expiry.settlement,
    dte: expiry.dte,
    forward: 7800,
    rows: rows.map(([strike, put, call]): ChainRow => {
      const mark = options.noMark?.includes(strike) ? null : 10
      return {
        strike,
        call: call === undefined ? null : quote(expiry, strike, 'call', call, mark),
        put: put === undefined ? null : quote(expiry, strike, 'put', put, mark),
      }
    }),
    warnings: [],
  }
}

const FLY = listing('2027-03-19', 200)
const LONG = listing('2027-05-21', 263)

/** Puts: Δ falls 0.05 a strike down from 7500 (Δ40); calls rise the other way. */
const FLY_ROWS: [number, number, number][] = [
  [6500, 0.08, 0.92],
  [6700, 0.12, 0.88],
  [6800, 0.15, 0.85],
  [6900, 0.17, 0.83],
  [7000, 0.19, 0.81],
  [7100, 0.21, 0.79],
  [7200, 0.23, 0.77],
  [7300, 0.26, 0.74],
  [7400, 0.3, 0.7],
  [7500, 0.34, 0.66],
  [7600, 0.37, 0.63],
  [7700, 0.43, 0.57],
  [7800, 0.5, 0.5],
  [7900, 0.58, 0.42],
  [8000, 0.65, 0.35],
  [8100, 0.72, 0.29],
  [8200, 0.78, 0.24],
  [8300, 0.82, 0.19],
  [8400, 0.86, 0.16],
  [8500, 0.9, 0.12],
]

describe('pickSetupExpiries', () => {
  it('takes the expiration nearest 200 DTE for the fly, the AM one on a tie, wherever it is listed', () => {
    const picked = pickSetupExpiries(M200_CDS, EXPIRATIONS)
    expect(picked.ok && picked.picked.slots).toEqual({ fly: listing('2027-03-19', 200) })
    expect(picked.ok && picked.picked.warnings).toEqual([])
    // yfinance labels both twins `monthly`; the PM one listed first still loses.
    const twins = [listing('2027-03-19', 200, 'SPXW', 'monthly'), listing('2027-03-19', 200, 'SPX', 'monthly')]
    const unsorted = pickSetupExpiries(M200_CDS, [...twins, listing('2026-10-16', 46)])
    expect(unsorted.ok && unsorted.picked.slots.fly).toEqual(twins[1])
  })

  it('keeps the diagonal’s long call 1.05–1.4× the fly’s DTE, nearest 250 among those', () => {
    const picked = pickSetupExpiries(M200_CD, EXPIRATIONS)
    // 263 (1.32×) beats 228 (1.14×) on distance to 250; 303 is 1.52×, out.
    expect(picked.ok && picked.picked.slots).toEqual({
      fly: listing('2027-03-19', 200),
      long: listing('2027-05-21', 263),
    })
    expect(picked.ok && picked.picked.warnings).toEqual([])
  })

  it('falls back to the nearest later expiration, with a warning, when none keeps the ratio', () => {
    // 172 is nearer 250 than 382 is, but a long call dated before the short
    // one is not a diagonal.
    const sparse = [listing('2027-02-19', 172), listing('2027-03-19', 200), listing('2027-09-17', 382)]
    const picked = pickSetupExpiries(M200_CD, sparse)
    expect(picked.ok && picked.picked.slots.long).toEqual(listing('2027-09-17', 382))
    expect(picked.ok && picked.picked.warnings).toEqual([
      'No expiration is 1.05–1.4× the 200 DTE of the fly; 17 Sep 2027 · 382 DTE is 1.91×.',
    ])
    expect(pickSetupExpiries(M200_CD, [listing('2027-02-19', 172), listing('2027-03-19', 200)])).toEqual({
      ok: false,
      reason: 'No expiration after the fly’s 200 DTE is listed for the long slot.',
    })
    // The band's edges are exact: 210 is 1.05 × 200, in — and 40 days off
    // the target is inside the tolerance, so nothing to say.
    const edge = pickSetupExpiries(M200_CD, [listing('2027-03-19', 200), listing('2027-03-29', 210)])
    expect(edge.ok && edge.picked.slots.long?.dte).toBe(210)
    expect(edge.ok && edge.picked.warnings).toEqual([])
    // A pick far from the target but inside the band says which band.
    const far = pickSetupExpiries(M200_CD, [listing('2027-03-31', 212), listing('2027-06-23', 296)])
    expect(far.ok && far.picked.slots).toEqual({ fly: listing('2027-03-31', 212), long: listing('2027-06-23', 296) })
    expect(far.ok && far.picked.warnings).toEqual([
      '23 Jun 2027 · 296 DTE is the nearest to the 250 DTE target inside the 1.05–1.4× band.',
    ])
  })

  it('warns when the nearest expiration is far from a slot’s target', () => {
    const picked = pickSetupExpiries(M200_CDS, [listing('2026-10-16', 46), listing('2027-02-19', 120)])
    expect(picked.ok && picked.picked.slots.fly?.dte).toBe(120)
    expect(picked.ok && picked.picked.warnings).toEqual(['19 Feb 2027 · 120 DTE is the nearest to the 200 DTE target.'])
  })

  it('keeps the STT in a regular monthly 160–200 DTE, nearest 160, never a weekly or under 160', () => {
    // February's monthly (172) over March's (200); the quarterly and the
    // weeklies are not regular expirations, however near 160 they sit.
    const picked = pickSetupExpiries(STT, [...EXPIRATIONS, listing('2027-02-12', 165, 'SPXW', 'weekly')])
    expect(picked.ok && picked.picked.slots).toEqual({ fly: listing('2027-02-19', 172) })
    expect(picked.ok && picked.picked.warnings).toEqual([])
    // 159 is under the floor, however close; 200 is the ceiling, in.
    const edges = pickSetupExpiries(STT, [listing('2027-02-06', 159), listing('2027-03-19', 200)])
    expect(edges.ok && edges.picked.slots.fly?.dte).toBe(200)
    expect(edges.ok && edges.picked.warnings).toEqual([])
    // Nothing regular in the window is a refusal, not a weekly.
    expect(
      pickSetupExpiries(STT, [listing('2027-01-15', 137), listing('2027-02-12', 165, 'SPXW', 'weekly'), listing('2027-03-31', 212, 'SPXW', 'quarterly')]),
    ).toEqual({ ok: false, reason: 'No monthly expiration 160–200 DTE is listed for the fly slot.' })
  })

  it('skips today’s expiration and fails with none listed', () => {
    expect(pickSetupExpiries(M200_CDS, [listing('2026-08-31', 0)])).toEqual({
      ok: false,
      reason: 'No expiration after today is listed.',
    })
    expect(pickSetupExpiries(M200_CDS, [])).toEqual({ ok: false, reason: 'No expirations are listed.' })
  })
})

describe('fitSetup', () => {
  it('fits the M200 CDS: body by delta, balanced wings nearest their deltas, calls by delta', () => {
    const picked = pickSetupExpiries(M200_CDS, EXPIRATIONS)
    if (!picked.ok) throw new Error(picked.reason)
    const result = fitSetup(M200_CDS, picked.picked.slots, { fly: chain(FLY, FLY_ROWS) })
    if (!result.ok) throw new Error(result.reason)
    const legs = result.fit.legs.map((l) => [l.leg.id, l.leg.qty, l.quote.contract.strike, l.delta])
    // SP: Δ24 → 7200 (0.23). Width 300: ULP 7500 (0.34 vs 0.35), LLP 6900
    // (0.17 vs 0.16) — error 0.02; width 400 would be 7600/6800 = 0.02 + 0.01
    // = 0.03; width 200: 7400/7000 = 0.05 + 0.03.
    expect(legs).toEqual([
      ['ULP', 2, 7500, -0.34],
      ['SP', -4, 7200, -0.23],
      ['LLP', 2, 6900, -0.17],
      ['LC', 1, 8200, 0.24],
      ['SC', -1, 8300, 0.19],
    ])
    // In the plan's points: −68 + 92 − 34 + 24 − 19 = −5.
    expect(result.fit.netDelta).toBeCloseTo(-5, 8)
    expect(result.fit.warnings).toEqual(['Net Δ −5.0 is outside ±1.'])
    expect(result.fit.atEdge).toEqual([])
    expect(fitOpensSection(result.fit)).toBe('2027-03-19|SPX')
    expect(fitLines(result.fit)).toEqual([
      '19 Mar 2027 · 200 DTE: +2 7500P Δ34 / −4 7200P Δ23 / +2 6900P Δ17 / +1 8200C Δ24 / −1 8300C Δ19',
    ])
  })

  it('fits the M200 CD across two chains and opens the fly’s section', () => {
    const picked = pickSetupExpiries(M200_CD, EXPIRATIONS)
    if (!picked.ok) throw new Error(picked.reason)
    // Longer-dated calls carry more delta: Δ32.5 lands a strike above the short call.
    const longRows: [number, number, number][] = FLY_ROWS.map(([k, p, c]) => [k, p, Math.min(0.99, c + 0.09)])
    const result = fitSetup(M200_CD, picked.picked.slots, { fly: chain(FLY, FLY_ROWS), long: chain(LONG, longRows) })
    if (!result.ok) throw new Error(result.reason)
    const legs = result.fit.legs.map((l) => [l.leg.id, l.quote.contract.expiry, l.quote.contract.strike, Math.round(l.delta * 1000) / 1000])
    expect(legs).toEqual([
      ['ULP', '2027-03-19', 7500, -0.34],
      ['SP', '2027-03-19', 7200, -0.23],
      ['LLP', '2027-03-19', 6900, -0.17],
      ['SC', '2027-03-19', 8100, 0.29], // Δ27.5: 8100 (0.29) over 8200 (0.24)
      ['LC', '2027-05-21', 8200, 0.33], // Δ32.5 on the longer chain: 8200 (0.24 + 0.09)
    ])
    expect(fitOpensSection(result.fit)).toBe('2027-03-19|SPX')
    expect(fitLines(result.fit)).toEqual([
      '19 Mar 2027 · 200 DTE: +2 7500P Δ34 / −4 7200P Δ23 / +2 6900P Δ17 / −1 8100C Δ29',
      '21 May 2027 · 263 DTE: +1 8200C Δ33',
    ])
  })

  it('skips strikes without a mark or a delta, and warns when a leg lands far from its delta', () => {
    const rows: [number, number | null, number | null][] = FLY_ROWS.map(([k, p, c]) => [k, k === 7200 ? null : p, c])
    const result = fitSetup(M200_CDS, { fly: FLY }, { fly: chain(FLY, rows, { noMark: [7300] }) })
    if (!result.ok) throw new Error(result.reason)
    // 7200 has no delta and 7300 no mark: the body goes to 7100 (0.21).
    expect(result.fit.legs.find((l) => l.leg.id === 'SP')?.quote.contract.strike).toBe(7100)
    expect(result.fit.warnings).toEqual(['Net Δ −3.0 is outside ±1.'])
    const coarse = fitSetup(M200_CDS, { fly: FLY }, { fly: chain(FLY, [[7000, 0.1, 0.9], [7500, 0.3, 0.6], [8000, 0.7, 0.3], [8500, 0.9, 0.1]]) })
    if (!coarse.ok) throw new Error(coarse.reason)
    // The long call's 0.30 is within the 0.05 tolerance of Δ25; nothing else is.
    expect(coarse.fit.warnings).toEqual([
      'ULP 8000 is Δ70, 0.35 off its Δ35 target.',
      'SP 7500 is Δ30, 0.06 off its Δ24 target.',
      'LLP 7000 is Δ10, 0.06 off its Δ16 target.',
      'SC 8500 is Δ10, 0.08 off its Δ18 target.',
      'Net Δ −20.0 is outside ±1.',
    ])
    // Legs on the chain's first and last strikes: the window may have cut
    // the fit short, so the panel fetches the whole expiry and refits.
    expect(coarse.fit.atEdge).toEqual(['fly'])
  })

  it('reports the combo delta in plan points, within the bound when the legs are on target', () => {
    // Deltas exactly on the plan's: −70 + 96 − 32 + 25 − 18 = +1, inside ±1.
    const exact: [number, number, number][] = [
      [6800, 0.16, 0.84], [7000, 0.2, 0.8], [7200, 0.24, 0.76], [7400, 0.3, 0.7], [7600, 0.35, 0.65],
      [8200, 0.75, 0.25], [8400, 0.82, 0.18],
    ]
    const result = fitSetup(M200_CDS, { fly: FLY }, { fly: chain(FLY, exact) })
    if (!result.ok) throw new Error(result.reason)
    expect(result.fit.netDelta).toBeCloseTo(1, 8)
    expect(result.fit.warnings).toEqual([])
    expect(fmtNetDelta(result.fit.netDelta)).toBe('+1.0')
    expect(fmtNetDelta(-2.26)).toBe('−2.3')
    expect(fmtNetDelta(-0.04)).toBe('+0.0')
    // No bound on the CD: a wider net delta passes without a word.
    const cd: TradeSetup = { ...M200_CD, slots: { fly: { targetDte: 200 } }, legs: M200_CD.legs.map((l) => ({ ...l, slot: 'fly' })) }
    const wide = fitSetup(cd, { fly: FLY }, { fly: chain(FLY, FLY_ROWS) })
    expect(wide.ok && wide.fit.warnings).toEqual([])
  })

  it('moves the body to the strike grid its wings are listed on', () => {
    // A 5-point grid near the money, 25-point further out: the Δ24 body
    // nearest by delta (7130) has no listed wings at any width that
    // reaches the coarse grid both ways, so the fly on 7125 wins.
    const rows: [number, number, number][] = [
      [6800, 0.16, 0.84], [6825, 0.165, 0.835], [6850, 0.17, 0.83], [6875, 0.18, 0.82], [6900, 0.19, 0.81],
      [6925, 0.195, 0.805], [6950, 0.2, 0.8], [6975, 0.21, 0.79], [7000, 0.215, 0.785], [7025, 0.22, 0.78],
      [7050, 0.225, 0.775], [7075, 0.23, 0.77], [7100, 0.232, 0.768], [7125, 0.236, 0.764], [7130, 0.24, 0.76],
      [7135, 0.245, 0.755], [7140, 0.25, 0.75], [7145, 0.255, 0.745], [7150, 0.26, 0.74], [7175, 0.27, 0.73],
      [7200, 0.28, 0.72], [7225, 0.29, 0.71], [7250, 0.3, 0.7], [7275, 0.31, 0.69], [7300, 0.32, 0.68],
      [7325, 0.33, 0.67], [7350, 0.34, 0.66], [7375, 0.35, 0.65], [7400, 0.36, 0.64], [7425, 0.37, 0.63],
      [7450, 0.38, 0.62], [7475, 0.4, 0.6], [7500, 0.42, 0.58], [8000, 0.7, 0.3], [8250, 0.8, 0.2],
    ]
    const result = fitSetup(M200_CDS, { fly: FLY }, { fly: chain(FLY, rows) })
    if (!result.ok) throw new Error(result.reason)
    const fly = result.fit.legs.slice(0, 3).map((l) => l.quote.contract.strike)
    // Body-first would have taken 7130 (Δ24 exactly) and the only wings it
    // can have, 7135/7125 — a 5-point fly. The search lands the body on
    // 7100 (Δ23.2), whose 275-wide wings are Δ35 and Δ16.5 to the point.
    expect(fly).toEqual([7375, 7100, 6825])
  })

  it('refuses two legs on one contract rather than letting the model net them away', () => {
    // Calls stop at 8000 with Δ30: Δ25 and Δ18 both land there.
    const result = fitSetup(M200_CDS, { fly: FLY }, { fly: chain(FLY, [[7000, 0.1, 0.9], [7500, 0.3, 0.6], [8000, 0.7, 0.3]]) })
    expect(result).toEqual({
      ok: false,
      reason: 'The long call and the short call both fit 8000: no strike further out is listed on the 19 Mar 2027 · 200 DTE chain.',
    })
  })

  it('fails, not guesses, when a chain is missing or no symmetric wings are listed', () => {
    expect(fitSetup(M200_CDS, { fly: FLY }, {})).toEqual({ ok: false, reason: 'The 19 Mar 2027 · 200 DTE chain did not load.' })
    const lopsided = chain(FLY, [[7200, 0.23, 0.77], [7500, 0.34, 0.66], [7800, 0.5, 0.5], [8200, 0.9, 0.24], [8300, 0.95, 0.19]])
    expect(fitSetup(M200_CDS, { fly: FLY }, { fly: lopsided })).toEqual({
      ok: false,
      reason: 'No pair of equal-width put wings is listed around the 7200 short put.',
    })
    expect(fitSetup(M200_CD, { fly: FLY, long: LONG }, { fly: chain(FLY, FLY_ROWS) })).toEqual({
      ok: false,
      reason: 'The 21 May 2027 · 263 DTE chain did not load.',
    })
  })

  it('fits the STT’s broken wings each on its own delta, flagging a wing on the chain’s edge', () => {
    const slot = listing('2027-02-19', 172)
    // Δ25 → 7300 (Δ26 beats 7200's Δ23); Δ15 → 6800; Δ5 → 6500 (Δ8), the
    // lowest strike listed — so the panel is told to fetch the whole chain.
    const edge = fitSetup(STT, { fly: slot }, { fly: chain(slot, FLY_ROWS) })
    expect(edge.ok && edge.fit.legs.map((l) => [l.leg.id, l.quote.contract.strike, l.leg.qty])).toEqual([
      ['ULP', 7300, 1],
      ['SP', 6800, -2],
      ['LLP', 6500, 1],
    ])
    expect(edge.ok && edge.fit.atEdge).toEqual(['fly'])
    // 1×−0.26 + −2×−0.15 + 1×−0.08 = −0.04 → −4 points; nothing to warn about.
    expect(edge.ok && fmtNetDelta(edge.fit.netDelta)).toBe('−4.0')
    expect(edge.ok && edge.fit.warnings).toEqual([])
    expect(edge.ok && fitLines(edge.fit)).toEqual(['19 Feb 2027 · 172 DTE: +1 7300P Δ26 / −2 6800P Δ15 / +1 6500P Δ8'])
    // With the chain whole, the wing goes to the Δ5 strike and the widths
    // are whatever the deltas make them: 500 above, 500 below here, but
    // nothing holds them equal.
    const whole = fitSetup(STT, { fly: slot }, { fly: chain(slot, [[6200, 0.04, 0.96], [6300, 0.05, 0.95], ...FLY_ROWS]) })
    expect(whole.ok && whole.fit.legs.map((l) => l.quote.contract.strike)).toEqual([7300, 6800, 6300])
    expect(whole.ok && whole.fit.atEdge).toEqual([])
    expect(whole.ok && fitOpensSection(whole.fit)).toBe('2027-02-19|SPX')
  })

  it('offers the M200 tranches, CD first, then the STT, each described by its plan', () => {
    expect(SETUPS.map((s) => s.label)).toEqual(['M200 CD', 'M200 CDS', 'STT'])
    for (const setup of [M200_CD, M200_CDS]) expect(setup.summary).toMatch(/Δ35 \/ −4 Δ24 \/ \+2 Δ16/)
    expect(STT.summary).toMatch(/\+1 Δ25 \/ −2 Δ15 \/ \+1 Δ5/)
    for (const setup of SETUPS) expect(setup.legs.map((l) => l.qty).reduce((a, b) => a + b, 0)).toBe(0)
  })
})
