import { act, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it } from 'vitest'
import { queryKeys } from '../api/queries'
import type { ReconciliationResponse } from '../api/types'
import { useAppStore } from '../store/appStore'
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
    expect(screen.getByText(/Secrets are redacted before this view is built/)).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'show' }))
    expect(
      await screen.findByText(/"token_path": "\/home\/trader\/.config\/wingspan\/schwab_token.json"/),
    ).toBeInTheDocument()
  })

  it('shows the Settings and Diagnostics cards beside the provider/config ones', async () => {
    renderWithClient(<DataHealth />)
    expect(
      await screen.findByRole('heading', { name: 'Provider diagnostics' }),
    ).toBeInTheDocument()
    expect(screen.getByRole('heading', { name: 'Settings' })).toBeInTheDocument()
    expect(screen.getByRole('heading', { name: 'Diagnostics' })).toBeInTheDocument()
  })

  it('clears the listed warnings, remembers that, and restores them on request', async () => {
    const user = userEvent.setup()
    renderWithClient(<DataHealth />)
    await screen.findByText('duplicate_execution_claim')
    const total = useAppStore.getState().dismissedWarnings.length
    expect(total).toBe(0)

    // Clear only the info-level noise: the filter scopes the button.
    await user.click(screen.getByRole('button', { name: /^info \d+$/ }))
    const infoCount = Number(
      screen.getByRole('button', { name: /^info \d+$/ }).textContent?.replace(/\D/g, ''),
    )
    expect(infoCount).toBeGreaterThan(0)
    await user.click(screen.getByRole('button', { name: `clear ${infoCount}` }))

    expect(screen.queryByText('statement_date_disagreement')).not.toBeInTheDocument()
    expect(screen.getByText(/Nothing left to show/)).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'info 0' })).toBeInTheDocument()
    expect(useAppStore.getState().dismissedWarnings).toHaveLength(infoCount)
    // The warn-level ones are untouched.
    await user.click(screen.getByRole('button', { name: /^all \d+$/ }))
    expect(screen.getByText('duplicate_execution_claim')).toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: `restore ${infoCount}` }))
    expect(await screen.findByText('statement_date_disagreement')).toBeInTheDocument()
    expect(useAppStore.getState().dismissedWarnings).toEqual([])
    expect(screen.queryByRole('button', { name: /^restore/ })).not.toBeInTheDocument()
  })

  it('shows the trade source card above reconciliation', async () => {
    renderWithClient(<DataHealth />)
    const heading = await screen.findByRole('heading', { name: 'Trade source' })
    expect(heading).toBeInTheDocument()
    const card = heading.closest('section') as HTMLElement
    await waitFor(() => expect(card).toHaveTextContent('demo book'))
    expect(card).toHaveTextContent('fixtures/demo/legs.csv')
  })

  it('reports the source_note sentence instead of an empty reconciliation table', async () => {
    const { client } = renderWithClient(<DataHealth />)
    // Wait for the default (non-empty) reconciliation to land before
    // overriding it, so the override is not clobbered by the real fetch.
    await screen.findByText('SPX270115P7400')

    const emptyWithNote: ReconciliationResponse & { source_note: string } = {
      statement_as_of: null,
      rows: [],
      note: '',
      source_note: 'An imported Legs sheet or CSV has nothing to reconcile against a statement.',
    }
    act(() => client.setQueryData(queryKeys.reconciliation, emptyWithNote))

    expect(
      await screen.findByText(
        'An imported Legs sheet or CSV has nothing to reconcile against a statement.',
      ),
    ).toBeInTheDocument()
    expect(screen.getByRole('heading', { name: /^Reconciliation$/ })).toBeInTheDocument()
    expect(screen.queryByText(/statement 2026/)).not.toBeInTheDocument()
  })

  it('reworks the unmatched_order hint for the sheet/CSV era, and adds sheet_* hints', async () => {
    const { client } = renderWithClient(<DataHealth />)
    await screen.findByText('duplicate_execution_claim')

    act(() =>
      client.setQueryData(queryKeys.warnings, {
        warnings: [
          {
            severity: 'warn',
            code: 'unmatched_order',
            message: 'no execution matched',
            trade_id: null,
            contract: null,
          },
          {
            severity: 'warn',
            code: 'sheet_column_absent',
            message: 'Opened is blank',
            trade_id: null,
            contract: null,
          },
        ],
      }),
    )

    expect(await screen.findByText(/assembled from TOS order files/)).toBeInTheDocument()
    expect(screen.queryByText(/Daily Trade Statement/)).not.toBeInTheDocument()
    expect(
      screen.getByText(/without a cost basis or days-in-trade for the affected legs/),
    ).toBeInTheDocument()
  })

  it('folds the warnings card down to its header and remembers it', async () => {
    const user = userEvent.setup()
    renderWithClient(<DataHealth />)
    await screen.findByText('duplicate_execution_claim')
    await user.click(screen.getByRole('button', { name: 'Collapse Warnings' }))
    expect(screen.queryByText('duplicate_execution_claim')).not.toBeInTheDocument()
    expect(useAppStore.getState().warningsCollapsed).toBe(true)
    // The header — with its filters — stays.
    expect(screen.getByRole('heading', { name: 'Warnings' })).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Expand Warnings' }))
    expect(await screen.findByText('duplicate_execution_claim')).toBeInTheDocument()
  })
})
