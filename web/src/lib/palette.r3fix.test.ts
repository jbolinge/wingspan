/** Round-3 review fix: the canonical `expiry` line (the live front) takes the
 *  solid rank-0 style even when an earlier expiration line is drawn. */

import { describe, expect, it } from 'vitest'
import { expiryRanks, expiryStyle } from './palette'

describe('expiryRanks', () => {
  it('ranks the canonical expiry line first, the rest by date', () => {
    const ranks = expiryRanks([
      { kind: 't0', key: 't0', date: '2026-10-20' },
      { kind: 'expiry', key: 'expiry:2027-03-19', date: '2027-03-19' },
      { kind: 'expiry', key: 'expiry', date: '2026-12-18' },
      { kind: 'expiry', key: 'expiry:2026-10-16', date: '2026-10-16' },
    ])
    expect(ranks.get('expiry')).toBe(0)
    expect(ranks.get('expiry:2026-10-16')).toBe(1)
    expect(ranks.get('expiry:2027-03-19')).toBe(2)
    expect(ranks.has('t0')).toBe(false)
    expect(expiryStyle(ranks.get('expiry')!)).toEqual({ width: 3, dash: null })
    expect(expiryStyle(ranks.get('expiry:2026-10-16')!).dash).not.toBeNull()
  })

  it('falls back to date order when the canonical line is not drawn', () => {
    const ranks = expiryRanks([
      { kind: 'expiry', key: 'expiry:2027-03-19', date: '2027-03-19' },
      { kind: 'expiry', key: 'expiry:2026-12-18', date: '2026-12-18' },
    ])
    expect(ranks.get('expiry:2026-12-18')).toBe(0)
    expect(ranks.get('expiry:2027-03-19')).toBe(1)
  })
})
