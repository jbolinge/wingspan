import { act, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it } from 'vitest'
import { mockRequest, resetMockTradeSource } from '../api/mock'
import type { TradeSourceResponse } from '../api/types'
import { useUiSignals } from '../lib/uiSignals'
import { useAppStore } from '../store/appStore'
import { renderWithClient, resetAppStore } from '../test/utils'
import { ClearTradesControl } from './ClearTradesControl'

const CSV_PATH = '/home/trader/Trades/legs.csv'

/** Put the mock server into the state importing a CSV leaves it in. */
function importCsv(): void {
  mockRequest('/trades/source', {
    method: 'PUT',
    body: JSON.stringify({ source: { kind: 'csv', path: CSV_PATH } }),
  })
}

const selection = () => useAppStore.getState().selected

beforeEach(() => {
  resetAppStore()
  resetMockTradeSource()
  useUiSignals.setState({ last: null })
})

describe('ClearTradesControl', () => {
  it('is disabled when nothing is selected and no source is imported', async () => {
    renderWithClient(<ClearTradesControl />)
    const button = screen.getByRole('button', { name: 'clear trades' })
    expect(button).toBeDisabled()
    // Still disabled once the source query lands (kind is `demo`, not `sheet`
    // or `csv`), so the wait guards against a false pass on the loading gap.
    await waitFor(() => expect(button).toBeDisabled())
  })

  it('asks for confirmation, and Cancel leaves the selection alone', async () => {
    const user = userEvent.setup()
    useAppStore.getState().setSelected(['a', 'b'])
    renderWithClient(<ClearTradesControl />)

    await user.click(screen.getByRole('button', { name: 'clear trades' }))
    const dialog = screen.getByRole('dialog', { name: 'Clear trades' })
    expect(dialog).toHaveTextContent('Are you sure you want to clear all current trades?')
    expect(dialog).toHaveTextContent('The 2 trades in the analysis set will be deselected')

    await user.click(screen.getByRole('button', { name: 'Cancel' }))
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument()
    expect(selection()).toEqual(['a', 'b'])
  })

  it('clears the analysis set on confirm and leaves the deployer’s book alone', async () => {
    const user = userEvent.setup()
    useAppStore.getState().setSelected(['a', 'b'])
    renderWithClient(<ClearTradesControl />)

    await user.click(screen.getByRole('button', { name: 'clear trades' }))
    await user.click(screen.getByRole('button', { name: 'Clear trades' }))

    expect(selection()).toEqual([])
    expect(screen.getByRole('status')).toHaveTextContent('analysis set cleared')
    const source = mockRequest('/trades/source') as TradeSourceResponse
    expect(source.kind).not.toBe('csv')
    expect(source.kind).not.toBe('sheet')
    // The dialog dismisses itself once the confirmation has been seen.
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument(), {
      timeout: 3000,
    })
  })

  it('stops showing an imported CSV on confirm, and touches nothing on disk', async () => {
    const user = userEvent.setup()
    importCsv()
    useAppStore.getState().setSelected(['a'])
    renderWithClient(<ClearTradesControl />)

    // The button only knows a source is imported once the query lands.
    const button = screen.getByRole('button', { name: 'clear trades' })
    await waitFor(() =>
      expect(button).toHaveAttribute(
        'title',
        expect.stringContaining('stop showing the trades from this source'),
      ),
    )

    await user.click(button)
    expect(screen.getByRole('dialog')).toHaveTextContent(
      `Wingspan will stop showing the trades from ${CSV_PATH}. Nothing on disk or in your spreadsheet is touched.`,
    )
    await user.click(screen.getByRole('button', { name: 'Clear trades' }))

    await screen.findByRole('status')
    const after = mockRequest('/trades/source') as TradeSourceResponse
    // The whole D23 sentence, not a prefix: a clear reports what it fell back
    // to, and says nothing about deleting anything.
    expect(screen.getByRole('status')).toHaveTextContent(
      `trades cleared — back to ${after.open_trades} open / ${after.closed_trades} closed trades from ${after.label}`,
    )
    expect(selection()).toEqual([])
    expect(after.kind).not.toBe('csv')
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument(), {
      timeout: 3000,
    })
  })

  it('stays enabled with an empty selection while a CSV is imported', async () => {
    importCsv()
    renderWithClient(<ClearTradesControl />)
    await waitFor(() =>
      expect(screen.getByRole('button', { name: 'clear trades' })).toBeEnabled(),
    )
  })

  it('says plainly that there is nothing to clear when no source is imported', async () => {
    const user = userEvent.setup()
    useAppStore.getState().setSelected(['a'])
    renderWithClient(<ClearTradesControl />)

    await user.click(await screen.findByRole('button', { name: 'clear trades' }))
    expect(screen.getByRole('dialog')).toHaveTextContent(
      'No source is imported, so there is nothing to clear.',
    )
  })

  /**
   * `demo`, `config` and `empty` are books the trader never imported
   * themselves, so with one of those loaded Clear is selection-only, exactly
   * like `folder`/`manual` — the D23 rule now keyed off `kind` instead of the
   * old `source === 'local'`.
   */
  it('is selection-only for the demo book (kind: demo, not sheet or csv)', async () => {
    const user = userEvent.setup()
    useAppStore.getState().setSelected(['a'])
    renderWithClient(<ClearTradesControl />)

    const source = mockRequest('/trades/source') as TradeSourceResponse
    expect(source.kind).toBe('demo')

    await user.click(await screen.findByRole('button', { name: 'clear trades' }))
    expect(screen.getByRole('dialog')).toHaveTextContent(
      'No source is imported, so there is nothing to clear.',
    )
  })

  it('opens the confirmation when the menu bridge emits open-clear-dialog', async () => {
    renderWithClient(<ClearTradesControl />)
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument()

    act(() => useUiSignals.getState().emit('open-clear-dialog'))

    expect(await screen.findByRole('dialog', { name: 'Clear trades' })).toBeInTheDocument()
  })
})
