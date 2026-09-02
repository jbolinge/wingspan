import { screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it } from 'vitest'
import { mockRequest, resetMockTradesRoot } from '../api/mock'
import type { TradesRootResponse } from '../api/types'
import { useAppStore } from '../store/appStore'
import { renderWithClient, resetAppStore } from '../test/utils'
import { ClearTradesControl } from './ClearTradesControl'

/** Put the mock server into the state a device upload leaves it in. */
function uploadFromDevice(): void {
  const form = new FormData()
  form.append('file', new File(['abc'], 'strangle'), 'Open Trades/Oct Strangle Aug 24 2026')
  form.append('file', new File(['de'], 's.csv'), 'Daily Trade Statement/2026-08-28.csv')
  mockRequest('/trades/upload', { method: 'POST', body: form })
}

const selection = () => useAppStore.getState().selected

beforeEach(() => {
  resetAppStore()
  resetMockTradesRoot()
})

describe('ClearTradesControl', () => {
  it('is disabled when nothing is selected and nothing was uploaded', async () => {
    renderWithClient(<ClearTradesControl />)
    const button = screen.getByRole('button', { name: 'clear trades' })
    expect(button).toBeDisabled()
    // Still disabled once the root query lands (source is `config`, not
    // `uploaded`), so the wait guards against a false pass on the loading gap.
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

  it('clears the analysis set on confirm and leaves a server-side root alone', async () => {
    const user = userEvent.setup()
    useAppStore.getState().setSelected(['a', 'b'])
    renderWithClient(<ClearTradesControl />)

    await user.click(screen.getByRole('button', { name: 'clear trades' }))
    await user.click(screen.getByRole('button', { name: 'Clear trades' }))

    expect(selection()).toEqual([])
    expect(screen.getByRole('status')).toHaveTextContent('analysis set cleared')
    const root = mockRequest('/trades/root') as TradesRootResponse
    expect(root.source).not.toBe('uploaded')
    // The dialog dismisses itself once the confirmation has been seen.
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument(), {
      timeout: 3000,
    })
  })

  it('removes an uploaded snapshot from the server on confirm', async () => {
    const user = userEvent.setup()
    uploadFromDevice()
    useAppStore.getState().setSelected(['a'])
    renderWithClient(<ClearTradesControl />)

    // The button only knows the root is uploaded once the query lands.
    const button = screen.getByRole('button', { name: 'clear trades' })
    await waitFor(() =>
      expect(button).toHaveAttribute('title', expect.stringContaining('remove the uploaded snapshot')),
    )

    await user.click(button)
    expect(screen.getByRole('dialog')).toHaveTextContent(
      'the snapshot uploaded from this device will be removed from the server',
    )
    await user.click(screen.getByRole('button', { name: 'Clear trades' }))

    await screen.findByRole('status')
    expect(screen.getByRole('status')).toHaveTextContent('upload removed')
    expect(selection()).toEqual([])
    expect((mockRequest('/trades/root') as TradesRootResponse).source).not.toBe('uploaded')
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument(), {
      timeout: 3000,
    })
  })

  it('stays enabled with an empty selection while an upload is on the server', async () => {
    uploadFromDevice()
    renderWithClient(<ClearTradesControl />)
    await waitFor(() =>
      expect(screen.getByRole('button', { name: 'clear trades' })).toBeEnabled(),
    )
  })
})
