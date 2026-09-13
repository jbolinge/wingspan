import { render, screen } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { RiskLine } from '../../api/types'
import { PnlInset } from './PnlInset'

const prices = [6000, 6100, 6200, 6300, 6400]

const lines: RiskLine[] = [
  { date: '2026-09-12', kind: 'tplus', key: 't+12', label: 'T+12', pnl: [-500, -300, -100, 100, 300] },
  { date: '2026-08-31', kind: 't0', key: 't0', label: 'T+0', pnl: [-400, -200, 0, 200, 400] },
  { date: '2027-02-19', kind: 'expiry', key: 'expiry', label: 'Exp 2027-02-19', pnl: [-800, -400, 0, 400, 800] },
]

describe('PnlInset', () => {
  // Line labels are keyed off the local calendar day (todayIso()), not any
  // fixture date — pin the clock so T+n labels are deterministic.
  beforeEach(() => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date(2026, 7, 31, 12, 0))
  })

  afterEach(() => {
    vi.useRealTimers()
  })

  it('lists one row per visible line, sorted by date', () => {
    render(<PnlInset data={{ prices, spot: 6200 }} lines={lines} index={2} theme="dark" />)
    const rows = screen.getAllByTestId('pnl-inset-row')
    expect(rows).toHaveLength(3)
    expect(rows.map((r) => r.textContent)).toEqual([
      expect.stringContaining('T+0'),
      expect.stringContaining('T+12'),
      expect.stringContaining('EXP Feb 19 2027'),
    ])
    // Never a raw ISO date.
    expect(rows.some((r) => /\d{4}-\d{2}-\d{2}/.test(r.textContent ?? ''))).toBe(false)
  })

  it('shows "At cursor" and the price when a cursor index is given', () => {
    render(<PnlInset data={{ prices, spot: 6200 }} lines={lines} index={2} theme="dark" />)
    expect(screen.getByText('At cursor')).toBeInTheDocument()
    expect(screen.getByText('6,200')).toBeInTheDocument()
  })

  it('shows "At spot" with no cursor, when spot is on the grid', () => {
    render(<PnlInset data={{ prices, spot: 6200 }} lines={lines} index={null} theme="dark" />)
    expect(screen.getByText('At spot')).toBeInTheDocument()
    expect(screen.getByText('6,200')).toBeInTheDocument()
  })

  it('says so when spot is off the grid and there is no cursor', () => {
    render(<PnlInset data={{ prices, spot: 9000 }} lines={lines} index={null} theme="dark" />)
    expect(screen.getByText('Outside the fetched range')).toBeInTheDocument()
    // The curves are still listed, every value a dash.
    const rows = screen.getAllByTestId('pnl-inset-row')
    expect(rows).toHaveLength(3)
    expect(rows.every((r) => r.textContent?.includes('—'))).toBe(true)
  })

  it('colours P&L values by sign', () => {
    render(<PnlInset data={{ prices, spot: 6200 }} lines={lines} index={4} theme="dark" />)
    const rows = screen.getAllByTestId('pnl-inset-row')
    const t0Row = rows.find((r) => r.textContent?.includes('T+0'))!
    expect(t0Row.querySelector('.text-pos')).not.toBeNull()
  })

  it('is a pointer-events-none inset with the pinned test id', () => {
    render(<PnlInset data={{ prices, spot: 6200 }} lines={lines} index={2} theme="dark" />)
    const inset = screen.getByTestId('pnl-inset')
    expect(inset.className).toContain('pointer-events-none')
  })
})
