/** What-if parity with the curves (D1 iii, D3). */

import { describe, expect, it } from 'vitest'
import { DEFAULT_GRAPH } from '../store/appStore'
import { buildWhatIfRequest } from './graphRequest'

describe('buildWhatIfRequest', () => {
  it('dates the default what-if on the server day handed to it', () => {
    expect(buildWhatIfRequest(['a'], DEFAULT_GRAPH, 7000, [], '2026-08-31')?.date).toBe('2026-08-31')
    expect(
      buildWhatIfRequest(['a'], { ...DEFAULT_GRAPH, whatIf: { ...DEFAULT_GRAPH.whatIf, date: '2026-09-04' } }, 7000, [], '2026-08-31')
        ?.date,
    ).toBe('2026-09-04')
  })

  it('sends the graph global shift plus its own slider', () => {
    const graph = { ...DEFAULT_GRAPH, volShift: 0.02, whatIf: { ...DEFAULT_GRAPH.whatIf, volShift: 0.01 } }
    expect(buildWhatIfRequest(['a'], graph, 7000)?.vol_shift).toBeCloseTo(0.03, 12)
  })

  it('sends the graph per-expiry shifts, and omits them when there are none', () => {
    const graph = { ...DEFAULT_GRAPH, volShiftByExpiry: { '2026-10-16': 0.015, '2026-11-20': 0 } }
    expect(buildWhatIfRequest(['a'], graph, 7000)?.vol_shift_by_expiry).toEqual({ '2026-10-16': 0.015 })
    expect(buildWhatIfRequest(['a'], DEFAULT_GRAPH, 7000)).not.toHaveProperty('vol_shift_by_expiry')
    expect(
      buildWhatIfRequest(['a'], { ...DEFAULT_GRAPH, volShiftByExpiry: { '2026-11-20': 0 } }, 7000),
    ).not.toHaveProperty('vol_shift_by_expiry')
  })
})
