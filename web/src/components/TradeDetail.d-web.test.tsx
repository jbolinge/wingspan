/** The XSP Hedge's leg row (D4, D5, D8): XSP label, a quote at its own
 *  precision, tenth-decimal greeks and the SPX-point exposure note. */

import { render, screen, within } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { mockRequest } from '../api/mock'
import type { Trade, TradesResponse } from '../api/types'
import { fmtLegGreek, fmtQuote } from '../lib/format'
import { TradeDetail } from './TradeDetail'
import { XSP_EXPOSURE_NOTE } from './units'

beforeEach(() => {
  vi.useFakeTimers({ toFake: ['Date'] })
  vi.setSystemTime(new Date(2026, 7, 31, 11, 0, 0))
})
afterEach(() => vi.useRealTimers())

function hedge(): Trade {
  return (mockRequest('/trades') as TradesResponse).trades.find((t) => t.name === 'XSP Hedge')!
}

/** The leg row's cells, in column order. */
function legCells(): HTMLElement[] {
  const row = screen.getByText('XSP 18 DEC 26 650 PUT').closest('tr')!
  return within(row).getAllByRole('cell')
}

describe('TradeDetail — the XSP Hedge', () => {
  it('labels the leg as XSP and prints its quote without truncating it', () => {
    const trade = hedge()
    const leg = trade.legs[0]!
    render(<TradeDetail trade={trade} />)
    const cells = legCells()
    // Qty, Contract, DTE, Avg cost, Mark, Bid, Ask, IV, Δ, Γ, Θ, V, Quote.
    expect(cells[4]).toHaveTextContent(fmtQuote(leg.mark, { tenth: true }))
    expect(Number(cells[4]!.textContent)).toBe(Math.round(leg.mark! * 1000) / 1000)
    expect(Number(cells[5]!.textContent)).toBe(Math.round(leg.bid! * 1000) / 1000)
    expect(Number(cells[6]!.textContent)).toBe(Math.round(leg.ask! * 1000) / 1000)
  })

  it('prints a third decimal when the proxy quote ÷ 10 has one', () => {
    const trade = hedge()
    const leg = { ...trade.legs[0]!, mark: 19.35 / 10, bid: 18.95 / 10, ask: 19.75 / 10, avg_cost: 3.8 }
    render(<TradeDetail trade={{ ...trade, legs: [leg] }} />)
    const cells = legCells()
    expect(cells[3]).toHaveTextContent('3.80')
    expect(cells[4]).toHaveTextContent('1.935')
    expect(cells[5]).toHaveTextContent('1.895')
    expect(cells[6]).toHaveTextContent('1.975')
  })

  it('gives the tenth-sized greeks one more decimal and the exposure note', () => {
    const trade = hedge()
    const leg = trade.legs[0]!
    render(<TradeDetail trade={trade} />)
    const cells = legCells()
    expect(cells[8]).toHaveTextContent(fmtLegGreek('delta', leg.greeks.delta, true))
    expect(cells[8]!.textContent).toMatch(/^[−-]?\d\.\d{3}$/)
    expect(cells[9]).toHaveTextContent(fmtLegGreek('gamma', leg.greeks.gamma, true))
    expect(cells[9]!.textContent).toMatch(/^[−-]?\d\.\d{6}$/)
    for (const cell of cells.slice(8, 12)) expect(cell).toHaveAttribute('title', XSP_EXPOSURE_NOTE)
  })
})
