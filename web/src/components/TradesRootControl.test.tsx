import { act, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it } from 'vitest'
import { resetMockTradesRoot } from '../api/mock'
import { queryKeys } from '../api/queries'
import type { TradesRootResponse } from '../api/types'
import { renderWithClient, resetAppStore } from '../test/utils'
import { TradesRootControl } from './TradesRootControl'

beforeEach(() => {
  resetAppStore()
  resetMockTradesRoot()
})

async function tooltipOf(button: HTMLElement): Promise<HTMLElement> {
  const user = userEvent.setup()
  await user.hover(button)
  return screen.findByRole('tooltip')
}

describe('TradesRootControl', () => {
  it('shows the loaded book by basename, with the path and source in the tooltip', async () => {
    renderWithClient(<TradesRootControl />)

    const button = await screen.findByRole('button', { name: 'Trades folder: Trades' })
    const tip = await tooltipOf(button)
    expect(tip).toHaveTextContent('/home/trader/workspace/tos-exports/Trades')
    expect(tip).toHaveTextContent('from the server config')
    expect(tip).toHaveTextContent(/\d+ open \/ \d+ closed/)
    expect(button).toHaveAttribute('aria-describedby', tip.id)
  })

  it('says so when the book is the demo one', async () => {
    const { client } = renderWithClient(<TradesRootControl />)
    await screen.findByRole('button', { name: 'Trades folder: Trades' })

    const demo: TradesRootResponse = {
      root: '/opt/wingspan/fixtures/demo',
      source: 'demo',
      config_root: null,
      open_trades: 6,
      closed_trades: 2,
    }
    act(() => client.setQueryData(queryKeys.tradesRoot, demo))

    const button = await screen.findByRole('button', { name: 'Trades folder: demo' })
    expect(await tooltipOf(button)).toHaveTextContent('demo book')
  })

  /**
   * The state an upload-only install sits in between a clear and the next
   * upload: no folder on the server at all. The control has to say that
   * rather than render an empty name or an em dash.
   */
  it('says nothing is loaded when the server has no book', async () => {
    const { client } = renderWithClient(<TradesRootControl />)
    await screen.findByRole('button', { name: 'Trades folder: Trades' })

    const empty: TradesRootResponse = {
      root: null,
      source: 'empty',
      config_root: null,
      open_trades: 0,
      closed_trades: 0,
    }
    act(() => client.setQueryData(queryKeys.tradesRoot, empty))

    const button = await screen.findByRole('button', { name: 'Trades folder: none' })
    const tip = await tooltipOf(button)
    expect(tip).toHaveTextContent('No trades loaded')
    expect(tip).toHaveTextContent('nothing loaded')
    expect(tip).toHaveTextContent('Click to upload a folder from this device')
  })

  it('opens the upload dialog, then gives focus back when it closes', async () => {
    const user = userEvent.setup()
    renderWithClient(<TradesRootControl />)

    const button = await screen.findByRole('button', { name: 'Trades folder: Trades' })
    expect(button).toHaveAttribute('aria-haspopup', 'dialog')
    expect(button).toHaveAttribute('aria-expanded', 'false')

    await user.click(button)
    const dialog = await screen.findByRole('dialog', { name: 'Load trades from this device' })
    expect(dialog).toBeInTheDocument()
    expect(button).toHaveAttribute('aria-expanded', 'true')

    await user.keyboard('{Escape}')
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument())
    expect(document.activeElement).toBe(button)
  })

  /**
   * The removed feature, pinned: the control used to open a folder browser on
   * the machine running the server. Trades do not live there any more, and no
   * route would answer it.
   */
  it('offers no way to point the server at a folder of its own', async () => {
    const user = userEvent.setup()
    renderWithClient(<TradesRootControl />)

    await user.click(await screen.findByRole('button', { name: 'Trades folder: Trades' }))
    await screen.findByRole('dialog')

    expect(screen.queryByLabelText('Folder path')).not.toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Up to parent folder' })).not.toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Select' })).not.toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Choose folder…' })).toBeInTheDocument()
  })
})
