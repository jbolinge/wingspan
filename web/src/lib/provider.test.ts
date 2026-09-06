import { describe, expect, it } from 'vitest'
import { SPOT_SOURCES, type MarketSnapshot, type QuoteSource } from '../api/types'
import { providerPill, snapshotAgeSeconds, sourceLabel } from './provider'

const NOW = new Date('2026-08-31T14:30:00Z')

function snapshot(
  spot: QuoteSource,
  option: QuoteSource,
  degraded: string[] = [],
  asOf = '2026-08-31T14:29:56Z',
): MarketSnapshot {
  return {
    as_of: asOf,
    spot_source: spot,
    option_source: option,
    degraded,
    spot: { symbol: 'SPX', last: 7750, change: -12.5, as_of: asOf },
    quote_count: 24,
    market_open: true,
  }
}

describe('providerPill', () => {
  it('is LIVE only when both sources are Schwab and nothing is degraded', () => {
    const pill = providerPill(snapshot('schwab', 'schwab'), NOW)
    expect(pill.state).toBe('LIVE')
    expect(pill.label).toBe('LIVE · Schwab')
    expect(pill.tone).toBe('pos')
  })

  it('downgrades Schwab to DELAYED when the snapshot carries a reason', () => {
    const pill = providerPill(snapshot('schwab', 'schwab', ['quotes are 20 min behind']), NOW)
    expect(pill.state).toBe('DELAYED')
    expect(pill.detail).toBe('quotes are 20 min behind')
  })

  it('reports yfinance as DELAYED and surfaces why Schwab was skipped', () => {
    const pill = providerPill(
      snapshot('yfinance', 'yfinance', ['Schwab: token file missing']),
      NOW,
    )
    expect(pill.label).toBe('DELAYED · yfinance')
    expect(pill.detail).toBe('Schwab: token file missing')
    expect(pill.reasons).toHaveLength(1)
  })

  it('reports the cache as STALE with the snapshot age', () => {
    const pill = providerPill(snapshot('cache', 'cache', [], '2026-08-31T12:30:00Z'), NOW)
    expect(pill.state).toBe('STALE')
    expect(pill.label).toBe('STALE · cached 2.0h')
  })

  it('reports statement marks as OFFLINE with the statement date', () => {
    const pill = providerPill(snapshot('cache', 'statement', [], '2026-08-28T20:00:00Z'), NOW)
    expect(pill.state).toBe('OFFLINE')
    expect(pill.label).toBe('OFFLINE · statement marks 2026-08-28')
  })

  it('lets the worse of the two sources decide', () => {
    // Live spot but cached options is still a stale book.
    expect(providerPill(snapshot('schwab', 'cache'), NOW).state).toBe('STALE')
    expect(providerPill(snapshot('yfinance', 'schwab'), NOW).state).toBe('DELAYED')
  })

  it('calls the fixture provider out as a demo', () => {
    expect(providerPill(snapshot('fixture', 'fixture'), NOW).state).toBe('DEMO')
  })

  it('reports tastytrade as LIVE and never as Schwab', () => {
    // `providerPill`'s switch is not exhaustive, so adding a source to
    // `SPOT_SOURCES` compiles happily and would have fallen through to the
    // old hardcoded `· Schwab` label — a tastytrade quote attributed to a
    // broker the user may not even have an account with.
    const pill = providerPill(snapshot('tastytrade', 'tastytrade'), NOW)
    expect(pill.state).toBe('LIVE')
    expect(pill.label).toBe('LIVE · tastytrade')
    expect(pill.tone).toBe('pos')
  })

  it('downgrades tastytrade to DELAYED when the snapshot carries a reason', () => {
    // `[tastytrade] delayed` is a user statement, not a wire marker: it
    // reaches the pill as a degraded reason like any other.
    const pill = providerPill(
      snapshot('tastytrade', 'tastytrade', ['tastytrade quotes are delayed']),
      NOW,
    )
    expect(pill.state).toBe('DELAYED')
    expect(pill.label).toBe('DELAYED · tastytrade')
  })

  it('ranks tastytrade level with schwab so the worse of the two still wins', () => {
    // A tie prefers `option_source`, the conservative side.
    expect(providerPill(snapshot('schwab', 'tastytrade'), NOW).source).toBe('tastytrade')
    expect(providerPill(snapshot('tastytrade', 'schwab'), NOW).source).toBe('Schwab')
    // And a genuinely worse source still beats both.
    expect(providerPill(snapshot('tastytrade', 'cache'), NOW).state).toBe('STALE')
  })

  it('says CONNECTING before the first snapshot arrives', () => {
    const pill = providerPill(undefined, NOW)
    expect(pill.state).toBe('UNKNOWN')
    expect(pill.label).toBe('CONNECTING…')
  })
})

describe('snapshotAgeSeconds', () => {
  it('never goes negative when the server clock runs ahead', () => {
    expect(snapshotAgeSeconds(snapshot('schwab', 'schwab', [], '2026-08-31T14:31:00Z'), NOW)).toBe(0)
  })

  it('is null without a snapshot', () => {
    expect(snapshotAgeSeconds(undefined, NOW)).toBeNull()
  })
})

describe('sourceLabel', () => {
  it('names every source in the contract', () => {
    expect(SPOT_SOURCES).toHaveLength(6)
    expect(sourceLabel('schwab')).toBe('Schwab')
    expect(sourceLabel('tastytrade')).toBe('tastytrade')
    expect(sourceLabel('yfinance')).toBe('yfinance')
    expect(sourceLabel('cache')).toBe('cache')
    expect(sourceLabel('statement')).toBe('statement marks')
    expect(sourceLabel('fixture')).toBe('fixture')
    expect(sourceLabel(null)).toBe('unknown')
  })
})
