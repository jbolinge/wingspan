import { screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it } from 'vitest'
import { renderWithClient, resetAppStore } from '../test/utils'
import { DataHealth } from './DataHealth'

beforeEach(() => resetAppStore())

describe('DataHealth', () => {
  it('lists warnings worst-first with their codes', async () => {
    renderWithClient(<DataHealth />)
    expect(await screen.findByText('duplicate_execution_claim')).toBeInTheDocument()
    expect(screen.getByText('unassigned_account_position')).toBeInTheDocument()
  })

  it('tells the trader how to fix a quarantined order', async () => {
    renderWithClient(<DataHealth />)
    expect(await screen.findByText(/Add an explicit hint comment/)).toBeInTheDocument()
    expect(screen.getAllByText(/# ref=<execution ref>/).length).toBeGreaterThan(0)
  })

  it('filters by severity', async () => {
    const user = userEvent.setup()
    renderWithClient(<DataHealth />)
    await screen.findByText('duplicate_execution_claim')
    await user.click(screen.getByRole('button', { name: /^info/ }))
    expect(screen.queryByText('duplicate_execution_claim')).not.toBeInTheDocument()
    expect(screen.getByText('statement_date_disagreement')).toBeInTheDocument()
  })

  it('shows only the reconciliation rows that need attention by default', async () => {
    const user = userEvent.setup()
    renderWithClient(<DataHealth />)
    expect(await screen.findByText('SPX270115P7400')).toBeInTheDocument()
    expect(screen.queryByText('SPX261016C8200')).not.toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'problems only' }))
    expect(await screen.findByText('SPX261016C8200')).toBeInTheDocument()
  })

  it('states that per-trade attribution differs from broker lots', async () => {
    renderWithClient(<DataHealth />)
    expect(await screen.findByText(/broker\/tax lots/)).toBeInTheDocument()
  })

  it('reports the live provider diagnostics', async () => {
    renderWithClient(<DataHealth />)
    expect(await screen.findByText(/yfinance quotes are ~15 min delayed/)).toBeInTheDocument()
  })

  it('keeps the resolved config collapsed until asked', async () => {
    const user = userEvent.setup()
    renderWithClient(<DataHealth />)
    expect(screen.getByText(/Secrets are redacted server-side/)).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'show' }))
    expect(await screen.findByText(/"auth_token": "\*\*\*"/)).toBeInTheDocument()
  })
})
