/**
 * Materialising the server preset rebuilds it from the server's line keys
 * (D1 ii): the default expiration line stays an expiration line, and the
 * editor counts days from the server's date, not the browser's.
 */

import { fireEvent, render, screen } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { RiskLine } from '../../api/types'
import { useAppStore } from '../../store/appStore'
import { resetAppStore } from '../../test/utils'
import { DateEditor } from './DateEditor'

const lines: RiskLine[] = [
  { date: '2026-08-31', kind: 't0', key: 't0', label: 'T+0', pnl: [] },
  { date: '2026-09-15', kind: 'tplus', key: 't+15', label: 'T+15', pnl: [] },
  { date: '2026-12-18', kind: 'expiry', key: 'expiry', label: 'Exp 2026-12-18', pnl: [] },
]

beforeEach(() => {
  resetAppStore()
  // The browser is already on Sep 1; the server (New York) is on Aug 31.
  vi.useFakeTimers({ toFake: ['Date'] })
  vi.setSystemTime(new Date(2026, 8, 1, 0, 30))
})
afterEach(() => vi.useRealTimers())

function renderEditor() {
  return render(
    <DateEditor expiries={['2026-12-18', '2027-01-15']} frontDte={109} lines={lines} today="2026-08-31" />,
  )
}

describe('DateEditor', () => {
  it('keeps the default expiration line an expiration line when a date is added', () => {
    renderEditor()
    fireEvent.click(screen.getByRole('button', { name: /\+ EXP JAN27/ }))
    expect(useAppStore.getState().graph.dates).toEqual([
      'today',
      '+15d',
      'expiry:2026-12-18',
      'expiry:2027-01-15',
    ])
  })

  it('adds a T+60 line on the nearest trading day, counted from the server day', () => {
    renderEditor()
    // 2026-08-31 + 60 is Friday October 30: T+60 as asked.
    const button = screen.getByRole('button', { name: '+ T+60' })
    expect(button).toHaveAttribute('title', 'Add the T+60 line: Oct 30 2026')
    fireEvent.click(button)
    expect(useAppStore.getState().graph.dates).toEqual(['today', '+15d', 'expiry:2026-12-18', '+60d'])
    // A second click is not a second line.
    fireEvent.click(button)
    expect(useAppStore.getState().graph.dates).toHaveLength(4)
  })

  it('moves T+60 off a weekend to the nearest trading day and says so', () => {
    // From September 1, T+60 is Saturday October 31: Friday is nearer than Monday.
    render(<DateEditor expiries={['2026-12-18']} frontDte={108} lines={lines} today="2026-09-01" />)
    const button = screen.getByRole('button', { name: '+ T+60' })
    expect(button).toHaveAttribute('title', 'Add the T+59 line, the nearest trading day to T+60: Oct 30 2026')
    fireEvent.click(button)
    expect(useAppStore.getState().graph.dates).toContain('+59d')
    expect(useAppStore.getState().graph.dates).not.toContain('+60d')
    expect(screen.getByText('T+59')).toBeInTheDocument()
  })

  it('counts an added calendar date from the server day', () => {
    renderEditor()
    const input = screen.getByLabelText('Add a date') as HTMLInputElement
    expect(input.min).toBe('2026-08-31')
    fireEvent.change(input, { target: { value: '2026-09-10' } })
    fireEvent.click(screen.getByRole('button', { name: 'add date' }))
    expect(useAppStore.getState().graph.dates).toContain('+10d')
  })
})
