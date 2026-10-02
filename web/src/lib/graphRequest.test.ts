import { describe, expect, it } from 'vitest'
import { DEFAULT_GRAPH } from '../store/appStore'
import { dateSpecDate, dateSpecLabel, dateToSpec } from './dateSpec'
import {
  buildRiskGraphRequest,
  buildWhatIfRequest,
  effectiveDateSpecs,
  evenLadder,
  presetDates,
} from './graphRequest'
import type { ModelContract, RiskLine } from '../api/types'

const MODEL_CONTRACT: ModelContract = {
  underlying: 'SPX',
  root: 'SPXW',
  expiry: '2026-10-16',
  settlement: 'pm',
  strike: 7700,
  right: 'call',
  multiplier: 100,
}

describe('buildRiskGraphRequest', () => {
  it('sends a null price range in auto mode', () => {
    const request = buildRiskGraphRequest(['a'], DEFAULT_GRAPH)
    expect(request.price_range).toBeNull()
    expect(request.trade_ids).toEqual(['a'])
    expect(request.points).toBe(241)
    expect(request.include_realized).toBe(true)
  })

  it('sends a percentage band in % mode', () => {
    const request = buildRiskGraphRequest(['a'], { ...DEFAULT_GRAPH, rangeMode: 'pct', rangePct: 0.2 })
    expect(request.price_range).toEqual({ pct: 0.2 })
  })

  it('sends absolute bounds only when both are set', () => {
    const partial = buildRiskGraphRequest(['a'], {
      ...DEFAULT_GRAPH,
      rangeMode: 'abs',
      rangeMin: 6000,
      rangeMax: null,
    })
    expect(partial.price_range).toBeNull()
    const full = buildRiskGraphRequest(['a'], {
      ...DEFAULT_GRAPH,
      rangeMode: 'abs',
      rangeMin: 6000,
      rangeMax: 8000,
    })
    expect(full.price_range).toEqual({ min: 6000, max: 8000 })
  })

  it('passes vol shifts, dates and the spot override straight through', () => {
    const request = buildRiskGraphRequest(['a', 'b'], {
      ...DEFAULT_GRAPH,
      volShift: 0.02,
      volShiftByExpiry: { '2027-02-19': -0.01 },
      dates: ['today', '+45d'],
      spotOverride: 7000,
      includeRealized: false,
    })
    expect(request.vol_shift).toBe(0.02)
    expect(request.vol_shift_by_expiry).toEqual({ '2027-02-19': -0.01 })
    expect(request.dates).toEqual(['today', '+45d'])
    expect(request.spot_override).toBe(7000)
    expect(request.include_realized).toBe(false)
  })

  it('always carries model_legs, empty without a model, so the key shape is stable', () => {
    const request = buildRiskGraphRequest(['a'], DEFAULT_GRAPH)
    expect(request.model_legs).toEqual([])
    expect(buildRiskGraphRequest(['a'], DEFAULT_GRAPH, [])).toEqual(request)
  })

  it('passes the modeled legs through, copied', () => {
    const legs = [{ contract: MODEL_CONTRACT, qty: -2 }]
    const request = buildRiskGraphRequest(['a'], DEFAULT_GRAPH, legs)
    expect(request.model_legs).toEqual(legs)
    legs.push({ contract: MODEL_CONTRACT, qty: 1 })
    expect(request.model_legs).toHaveLength(1)
  })

  it('copies the id list rather than aliasing the store array', () => {
    const ids = ['a']
    const request = buildRiskGraphRequest(ids, DEFAULT_GRAPH)
    ids.push('b')
    expect(request.trade_ids).toEqual(['a'])
  })
})

describe('buildWhatIfRequest', () => {
  it('is null without a selection', () => {
    expect(buildWhatIfRequest([], DEFAULT_GRAPH, 7000)).toBeNull()
  })

  it('answers for a model on its own, and carries the same legs as the curves', () => {
    const legs = [{ contract: MODEL_CONTRACT, qty: 1 }]
    expect(buildWhatIfRequest([], DEFAULT_GRAPH, 7000, legs)?.model_legs).toEqual(legs)
    expect(buildWhatIfRequest(['a'], DEFAULT_GRAPH, 7000)?.model_legs).toEqual([])
  })

  it('falls back to spot, then to the spot override, then to the explicit price', () => {
    expect(buildWhatIfRequest(['a'], DEFAULT_GRAPH, 7000)?.price).toBe(7000)
    expect(buildWhatIfRequest(['a'], { ...DEFAULT_GRAPH, spotOverride: 6800 }, 7000)?.price).toBe(6800)
    expect(
      buildWhatIfRequest(
        ['a'],
        { ...DEFAULT_GRAPH, spotOverride: 6800, whatIf: { ...DEFAULT_GRAPH.whatIf, price: 6500 } },
        7000,
      )?.price,
    ).toBe(6500)
  })

  it('is null when no price is knowable', () => {
    expect(buildWhatIfRequest(['a'], DEFAULT_GRAPH, null)).toBeNull()
  })

  /** The panel sits inches from the T+0 curve; both must be on the same basis
   *  or they disagree by the trade's realised P&L and fees. */
  it('carries the include-realised toggle, like the risk-graph request', () => {
    expect(buildWhatIfRequest(['a'], DEFAULT_GRAPH, 7000)?.include_realized).toBe(true)
    expect(
      buildWhatIfRequest(['a'], { ...DEFAULT_GRAPH, includeRealized: false }, 7000)
        ?.include_realized,
    ).toBe(false)
  })
})

describe('date presets', () => {
  it('builds the ONE ladder plus the expiration line', () => {
    expect(presetDates(100, '2026-12-08')).toEqual([
      'today',
      '+25d',
      '+50d',
      '+75d',
      '+99d',
      'expiry:2026-12-08',
    ])
  })

  it('declines to build a ladder inside one day of expiry', () => {
    expect(presetDates(1, '2026-09-01')).toBeNull()
    expect(presetDates(null, null)).toBeNull()
  })

  it('spaces an even ladder out to the front expiry', () => {
    expect(evenLadder(60, 3)).toEqual(['today', '+15d', '+30d', '+45d'])
  })
})

describe('date specs', () => {
  it('labels every spec form', () => {
    expect(dateSpecLabel('today')).toBe('T+0')
    expect(dateSpecLabel('+45d')).toBe('T+45')
    expect(dateSpecLabel('expiry:2027-02-19')).toBe('EXP FEB27')
  })

  it('resolves specs to calendar dates', () => {
    expect(dateSpecDate('today', '2026-08-31')).toBe('2026-08-31')
    expect(dateSpecDate('+45d', '2026-08-31')).toBe('2026-10-15')
    expect(dateSpecDate('expiry:2027-02-19', '2026-08-31')).toBe('2027-02-19')
  })

  it('prefers the expiry form when the picked date is an expiry', () => {
    expect(dateToSpec('2027-02-19', ['2027-02-19'], '2026-08-31')).toBe('expiry:2027-02-19')
    expect(dateToSpec('2026-10-15', ['2027-02-19'], '2026-08-31')).toBe('+45d')
    expect(dateToSpec('2026-08-31', [], '2026-08-31')).toBe('today')
  })
})

describe('effectiveDateSpecs', () => {
  const line = (kind: RiskLine['kind'], key: string, date: string): RiskLine => ({
    kind,
    key,
    date,
    label: key,
    pnl: [],
  })

  it('reproduces a response\'s lines key for key', () => {
    expect(
      effectiveDateSpecs([
        line('t0', 't0', '2026-08-31'),
        line('tplus', 't+12', '2026-09-12'),
        line('tplus', 't+45', '2026-10-15'),
        line('expiry', 'expiry', '2026-10-16'),
        line('expiry', 'expiry:2026-11-20', '2026-11-20'),
      ]),
    ).toEqual(['today', '+12d', '+45d', 'expiry:2026-10-16', 'expiry:2026-11-20'])
  })

  it('never repeats a spec, and reads an unkeyed T+0 as today', () => {
    expect(
      effectiveDateSpecs([line('t0', 't0', '2026-08-31'), line('t0', 'odd', '2026-08-31')]),
    ).toEqual(['today'])
  })
})
