/**
 * Line labels and "today" come from the server's response, not the browser
 * clock (audit D1): between local midnight and New York midnight — or for a
 * pinned, stale snapshot — the browser's day is not the day the curves were
 * valued on.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { RiskLine } from '../api/types'
import { lineLabel, liveExpiries, serverDateOf, tplusOffset } from './lines'

const line = (kind: RiskLine['kind'], key: string, date: string): RiskLine => ({
  kind,
  key,
  date,
  label: key.toUpperCase(),
  pnl: [],
})

describe('lineLabel', () => {
  // The browser clock a day ahead of the server's date — the evening case.
  beforeEach(() => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date(2026, 8, 1, 0, 30))
  })
  afterEach(() => vi.useRealTimers())

  it('reads the T+N off the server key, whatever the browser day is', () => {
    expect(lineLabel(line('tplus', 't+15', '2026-09-15'))).toBe('T+15')
    expect(lineLabel(line('t0', 't0', '2026-08-31'))).toBe('T+0')
  })

  it('never goes negative for a stale snapshot', () => {
    vi.setSystemTime(new Date(2026, 9, 30, 12, 0))
    expect(lineLabel(line('tplus', 't+3', '2026-09-03'))).toBe('T+3')
  })

  it('keeps the client EXP <date> form for expiration lines', () => {
    expect(lineLabel(line('expiry', 'expiry', '2026-12-18'))).toBe('EXP Dec 18 2026')
    expect(lineLabel(line('expiry', 'expiry:2027-01-15', '2027-01-15'))).toBe('EXP Jan 15 2027')
  })

  it('falls back to the server label for an unexpected key', () => {
    expect(lineLabel({ ...line('tplus', 'odd', '2026-09-20'), label: 'T+20' })).toBe('T+20')
  })
})

describe('serverDateOf', () => {
  it('is the T+0 line date', () => {
    expect(serverDateOf([line('tplus', 't+5', '2026-09-05'), line('t0', 't0', '2026-08-31')])).toBe(
      '2026-08-31',
    )
  })

  it('is a T+N line date less N when T+0 was edited out', () => {
    expect(serverDateOf([line('tplus', 't+12', '2026-09-12'), line('expiry', 'expiry', '2026-12-18')])).toBe(
      '2026-08-31',
    )
  })

  it('is null when only expiration lines say nothing about today', () => {
    expect(serverDateOf([line('expiry', 'expiry', '2026-12-18')])).toBeNull()
    expect(serverDateOf(undefined)).toBeNull()
  })

  it('parses only t+N keys', () => {
    expect(tplusOffset('t+42')).toBe(42)
    expect(tplusOffset('t0')).toBeNull()
    expect(tplusOffset('expiry')).toBeNull()
  })
})

describe('liveExpiries', () => {
  it('drops an expiry already settled on the server date and keeps expiry day', () => {
    expect(liveExpiries(['2026-10-16', '2026-10-19', '2026-11-20'], '2026-10-19')).toEqual([
      '2026-10-19',
      '2026-11-20',
    ])
  })
})
