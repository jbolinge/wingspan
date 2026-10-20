/** "At spot" reads the exact spot node of the non-uniform grid (D6), and
 *  the rows are named by the server keys (D1). */

import { render, screen } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { RiskLine } from '../../api/types'
import { PnlInset } from './PnlInset'

// Uniform 100-point samples plus a strike cluster and spot (6201.37).
const prices = [6000, 6100, 6195, 6200, 6201.37, 6205, 6300]
const lines: RiskLine[] = [
  { date: '2026-08-31', kind: 't0', key: 't0', label: 'T+0', pnl: [-9, -8, -7, -6, 1234, -4, -3] },
  { date: '2026-09-12', kind: 'tplus', key: 't+12', label: 'T+12', pnl: [0, 0, 0, 0, 0, 0, 0] },
]

beforeEach(() => {
  vi.useFakeTimers()
  vi.setSystemTime(new Date(2026, 8, 1, 0, 30))
})
afterEach(() => vi.useRealTimers())

describe('PnlInset on a non-uniform grid', () => {
  it('parks on the exact spot node with no cursor', () => {
    render(<PnlInset data={{ prices, spot: 6201.37 }} lines={lines} index={null} theme="dark" />)
    const rows = screen.getAllByTestId('pnl-inset-row').map((row) => row.textContent)
    expect(rows[0]).toBe('T+0+$1,234')
    expect(rows[1]).toMatch(/^T\+12/)
  })
})
