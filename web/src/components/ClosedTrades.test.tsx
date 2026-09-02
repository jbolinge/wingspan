import { screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it } from 'vitest'
import { renderWithClient, resetAppStore } from '../test/utils'
import { ClosedTrades } from './ClosedTrades'

beforeEach(() => resetAppStore())

describe('ClosedTrades', () => {
  it('lists closed trades with their realised P&L and duration', async () => {
    renderWithClient(<ClosedTrades />)
    expect(await screen.findByText('Aug Hedge')).toBeInTheDocument()
    expect(screen.getByText('Sep Put Spread')).toBeInTheDocument()
    expect(screen.getByText('-$285.00')).toBeInTheDocument()
  })

  it('summarises the book: count, win rate, fees and net realised', async () => {
    renderWithClient(<ClosedTrades />)
    expect(await screen.findByText('win rate')).toBeInTheDocument()
    expect(screen.getByText('avg held')).toBeInTheDocument()
    expect(screen.getByText('realised')).toBeInTheDocument()
  })

  it('expands a closed trade to reveal its quarantined orders', async () => {
    const user = userEvent.setup()
    renderWithClient(<ClosedTrades />)
    await user.click(await screen.findByLabelText('Expand Aug Hedge'))
    expect(screen.getByText('quarantined')).toBeInTheDocument()
    expect(screen.getByText('6 orders excluded')).toBeInTheDocument()
    expect(screen.getAllByText(/already claimed by Dec Fly/).length).toBeGreaterThan(0)
  })
})
