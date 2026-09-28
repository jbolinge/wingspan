import { render, screen, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it } from 'vitest'
import { mockRequest } from '../api/mock'
import type { Trade, TradesResponse } from '../api/types'
import { useAppStore } from '../store/appStore'
import { resetAppStore } from '../test/utils'
import { PositionsTable } from './PositionsTable'

const response = mockRequest('/trades') as TradesResponse
const trades: Trade[] = response.trades

beforeEach(() => resetAppStore())

function renderTable(rows: Trade[] = trades) {
  return render(
    <PositionsTable
      trades={rows}
      weightedVegaReference={response.portfolio_all.weighted_vega_reference}
    />,
  )
}

describe('PositionsTable', () => {
  it('renders one row per open trade with every planned column', () => {
    renderTable()
    for (const trade of trades) {
      expect(screen.getByText(trade.name)).toBeInTheDocument()
    }
    for (const header of ['Name', 'Opened', 'DIT', 'DTE', 'Structure', 'Cost', 'Value', 'W.V']) {
      expect(screen.getByTitle(`Sort by ${header}`)).toBeInTheDocument()
    }
  })

  it('shows the weighted-vega reference next to the selection controls', () => {
    renderTable()
    // The mock, like the server, defaults to a fixed 30-day reference.
    expect(screen.getByText('30d')).toBeInTheDocument()
  })

  it('selects all, none and the inverse', async () => {
    const user = userEvent.setup()
    renderTable()
    await user.click(screen.getByRole('button', { name: 'all' }))
    expect(useAppStore.getState().selected).toHaveLength(trades.length)
    await user.click(screen.getByRole('button', { name: 'none' }))
    expect(useAppStore.getState().selected).toEqual([])
    await user.click(screen.getByLabelText(`Select ${trades[0]!.name}`))
    await user.click(screen.getByRole('button', { name: 'invert' }))
    expect(useAppStore.getState().selected).toHaveLength(trades.length - 1)
    expect(useAppStore.getState().selected).not.toContain(trades[0]!.id)
  })

  it('drives the header checkbox from the row checkboxes', async () => {
    const user = userEvent.setup()
    renderTable()
    const header = screen.getByLabelText('Select all trades') as HTMLInputElement
    expect(header.checked).toBe(false)
    await user.click(header)
    expect(useAppStore.getState().selected).toHaveLength(trades.length)
    expect((screen.getByLabelText('Select all trades') as HTMLInputElement).checked).toBe(true)
  })

  it('sorts on a column and flips direction on a second click', async () => {
    const user = userEvent.setup()
    renderTable()
    await user.click(screen.getByTitle('Sort by Name'))
    expect(useAppStore.getState().sortKey).toBe('name')
    expect(useAppStore.getState().sortDir).toBe('asc')
    const firstPass = screen.getAllByRole('row').at(1)
    expect(within(firstPass as HTMLElement).getByText('Dec Fly')).toBeInTheDocument()
    await user.click(screen.getByTitle('Sort by Name'))
    expect(useAppStore.getState().sortDir).toBe('desc')
    const secondPass = screen.getAllByRole('row').at(1)
    expect(within(secondPass as HTMLElement).getByText('Oct Strangle')).toBeInTheDocument()
  })

  it('expands a row into its netted legs', async () => {
    const user = userEvent.setup()
    renderTable()
    const m1 = trades.find((t) => t.name === 'Oct Strangle') as Trade
    await user.click(screen.getByLabelText('Expand Oct Strangle legs'))
    expect(screen.getByText('Netted legs')).toBeInTheDocument()
    const leg = m1.legs[0]!
    expect(
      screen.getByText(
        new RegExp(`${leg.contract.root}.*${leg.contract.strike}.*${leg.contract.right.toUpperCase()}`),
      ),
    ).toBeInTheDocument()
    await user.click(screen.getByLabelText('Collapse Oct Strangle legs'))
    expect(screen.queryByText('Netted legs')).not.toBeInTheDocument()
  })

  it('publishes greek units as column tooltips', () => {
    renderTable()
    expect(screen.getAllByTitle(/per 1\.00 vol point/).length).toBeGreaterThan(0)
    expect(screen.getAllByTitle(/per 1 index point/).length).toBeGreaterThan(0)
  })

  it('shows a staleness dot whose label reports the quote age', () => {
    renderTable()
    const stale = trades.find((t) => (t.staleness_secs ?? 0) > 900) as Trade
    expect(screen.getByLabelText(new RegExp(`worst quote.*\\(stale\\)`))).toBeInTheDocument()
    expect(stale).toBeDefined()
  })

  it('badges a trade that carries warnings', () => {
    const flagged = trades.find((t) => t.warnings.length > 0) as Trade
    renderTable([flagged])
    expect(
      screen.getByTitle(
        `${flagged.warnings.length} warning${flagged.warnings.length === 1 ? '' : 's'} on ${flagged.name}`,
      ),
    ).toBeInTheDocument()
  })

  it('explains itself when there are no trades at all', () => {
    renderTable([])
    expect(screen.getByText(/No open trades/)).toBeInTheDocument()
  })
})
