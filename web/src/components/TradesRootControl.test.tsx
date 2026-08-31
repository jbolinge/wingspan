import { act, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it } from 'vitest'
import { mockRequest, resetMockTradesRoot } from '../api/mock'
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
  it('shows the active root by basename, with the path and source in the tooltip', async () => {
    renderWithClient(<TradesRootControl />)

    const button = await screen.findByRole('button', { name: 'Trades folder: Trades' })
    const tip = await tooltipOf(button)
    expect(tip).toHaveTextContent('/home/trader/workspace/example_trades/Trades')
    expect(tip).toHaveTextContent('from config')
    expect(tip).toHaveTextContent(/\d+ open \/ \d+ closed/)
    expect(button).toHaveAttribute('aria-describedby', tip.id)
  })

  it('says so when the folder was chosen in the app', async () => {
    mockRequest('/trades/root', { method: 'PUT', body: JSON.stringify({ root: '/mnt/backup' }) })
    renderWithClient(<TradesRootControl />)

    const button = await screen.findByRole('button', { name: 'Trades folder: trades-2025' })
    expect(await tooltipOf(button)).toHaveTextContent('chosen in app')
  })

  it('says so when the book is the demo fixture set', async () => {
    const { client } = renderWithClient(<TradesRootControl />)
    await screen.findByRole('button', { name: 'Trades folder: Trades' })

    const demo: TradesRootResponse = {
      root: '/opt/wingspan/fixtures/trades',
      source: 'demo',
      config_root: null,
      open_trades: 6,
      closed_trades: 2,
    }
    act(() => client.setQueryData(queryKeys.tradesRoot, demo))

    const button = await screen.findByRole('button', { name: 'Trades folder: trades' })
    expect(await tooltipOf(button)).toHaveTextContent('demo fixtures')
  })

  it('opens the picker, then gives focus back when it closes', async () => {
    const user = userEvent.setup()
    renderWithClient(<TradesRootControl />)

    const button = await screen.findByRole('button', { name: 'Trades folder: Trades' })
    expect(button).toHaveAttribute('aria-haspopup', 'dialog')
    expect(button).toHaveAttribute('aria-expanded', 'false')

    await user.click(button)
    const dialog = await screen.findByRole('dialog')
    expect(dialog).toBeInTheDocument()
    expect(button).toHaveAttribute('aria-expanded', 'true')

    await user.keyboard('{Escape}')
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument())
    expect(document.activeElement).toBe(button)
  })

  it('reflects the new root after the picker sets one', async () => {
    const user = userEvent.setup()
    renderWithClient(<TradesRootControl />)

    await user.click(await screen.findByRole('button', { name: 'Trades folder: Trades' }))
    await user.click(await screen.findByRole('button', { name: 'Up to parent folder' }))
    await user.click(await screen.findByRole('button', { name: /^notes$/ }))
    await user.click(screen.getByRole('button', { name: 'Select' }))

    // /home/trader/workspace/notes holds no trade folders: 422, dialog stays.
    expect(await screen.findByRole('alert')).toHaveTextContent('no trades folders under')
    expect(screen.getByRole('dialog')).toBeInTheDocument()
  })
})
