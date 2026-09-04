import { act, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it } from 'vitest'
import { mockRequest, resetMockTradesRoot } from '../api/mock'
import type { TradesRootResponse } from '../api/types'
import { useUiSignals } from '../lib/uiSignals'
import { useAppStore } from '../store/appStore'
import { renderWithClient, resetAppStore } from '../test/utils'
import { ClearTradesControl } from './ClearTradesControl'

/** Put the mock server into the state a chosen folder leaves it in. */
function chooseLocalFolder(): void {
  mockRequest('/trades/root', { method: 'PUT', body: JSON.stringify({ root: '/home/trader/Trades' }) })
}

const selection = () => useAppStore.getState().selected

beforeEach(() => {
  resetAppStore()
  resetMockTradesRoot()
  useUiSignals.setState({ last: null })
})

describe('ClearTradesControl', () => {
  it('is disabled when nothing is selected and no folder is loaded', async () => {
    renderWithClient(<ClearTradesControl />)
    const button = screen.getByRole('button', { name: 'clear trades' })
    expect(button).toBeDisabled()
    // Still disabled once the root query lands (source is `config`, not
    // `local`), so the wait guards against a false pass on the loading gap.
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
    const root = mockRequest('/trades/root') as TradesRootResponse
    expect(root.source).not.toBe('local')
    // The dialog dismisses itself once the confirmation has been seen.
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument(), {
      timeout: 3000,
    })
  })

  it('stops showing a loaded folder on confirm, and touches nothing on disk', async () => {
    const user = userEvent.setup()
    chooseLocalFolder()
    useAppStore.getState().setSelected(['a'])
    renderWithClient(<ClearTradesControl />)

    // The button only knows a folder is loaded once the query lands.
    const button = screen.getByRole('button', { name: 'clear trades' })
    await waitFor(() =>
      expect(button).toHaveAttribute(
        'title',
        expect.stringContaining('stop showing the trades from this folder'),
      ),
    )

    await user.click(button)
    expect(screen.getByRole('dialog')).toHaveTextContent(
      'Wingspan will stop showing the trades from /home/trader/Trades. Nothing on disk is touched.',
    )
    await user.click(screen.getByRole('button', { name: 'Clear trades' }))

    await screen.findByRole('status')
    const after = mockRequest('/trades/root') as TradesRootResponse
    // The whole D23 sentence, not a prefix: a clear reports what it fell back
    // to, and says nothing about deleting anything.
    expect(screen.getByRole('status')).toHaveTextContent(
      `trades cleared \u2014 back to ${after.open_trades} open / ${after.closed_trades} closed trades from ${after.root}`,
    )
    expect(selection()).toEqual([])
    expect(after.source).not.toBe('local')
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument(), {
      timeout: 3000,
    })
  })

  it('stays enabled with an empty selection while a folder is loaded', async () => {
    chooseLocalFolder()
    renderWithClient(<ClearTradesControl />)
    await waitFor(() =>
      expect(screen.getByRole('button', { name: 'clear trades' })).toBeEnabled(),
    )
  })

  it('says plainly that there is nothing to clear when no folder is loaded', async () => {
    const user = userEvent.setup()
    useAppStore.getState().setSelected(['a'])
    renderWithClient(<ClearTradesControl />)

    await user.click(await screen.findByRole('button', { name: 'clear trades' }))
    expect(screen.getByRole('dialog')).toHaveTextContent(
      'No folder is loaded, so there is nothing to clear.',
    )
  })

  it('opens the confirmation when the menu bridge emits open-clear-dialog', async () => {
    renderWithClient(<ClearTradesControl />)
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument()

    act(() => useUiSignals.getState().emit('open-clear-dialog'))

    expect(await screen.findByRole('dialog', { name: 'Clear trades' })).toBeInTheDocument()
  })
})
