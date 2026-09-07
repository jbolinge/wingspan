import { act, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it } from 'vitest'
import { mockSnapshot } from '../api/mock'
import { queryKeys } from '../api/queries'
import type { MarketSnapshot } from '../api/types'
import { useAppStore } from '../store/appStore'
import { renderWithClient, resetAppStore } from '../test/utils'
import { StatusBar } from './StatusBar'

beforeEach(() => resetAppStore())

describe('StatusBar', () => {
  it('names the provider actually in use and why the preferred one was skipped', async () => {
    renderWithClient(<StatusBar streamStatus="mock" />)
    expect(await screen.findByText('DELAYED · yfinance')).toBeInTheDocument()
    expect(screen.getByText(/Schwab: token file missing/)).toBeInTheDocument()
  })

  /**
   * The two ladders are independent — a live spot with statement marks for
   * the chain is the flagship offline case — so the tooltip must name each
   * capability's own source, not the pill's single worst-of-the-two.
   */
  it('names the spot and option sources independently in the tooltip', async () => {
    const user = userEvent.setup()
    const { client } = renderWithClient(<StatusBar streamStatus="mock" />)
    await screen.findByText('DELAYED · yfinance')

    const mixed: MarketSnapshot = {
      ...mockSnapshot(),
      spot_source: 'schwab',
      option_source: 'statement',
    }
    act(() => client.setQueryData(queryKeys.snapshot, mixed))

    await user.hover(await screen.findByText(/^OFFLINE · statement marks/))
    const tip = await screen.findByRole('tooltip')
    expect(tip).toHaveTextContent('spot: Schwab · options: statement marks')
  })

  it('shows the SPX tape with its change', async () => {
    renderWithClient(<StatusBar streamStatus="mock" />)
    expect(await screen.findByText('SPX')).toBeInTheDocument()
    expect(await screen.findByText('7,750.00')).toBeInTheDocument()
    expect(screen.getByText(/-12\.50/)).toBeInTheDocument()
  })

  it('flags that responses are coming from the mock layer', async () => {
    renderWithClient(<StatusBar streamStatus="mock" />)
    expect(await screen.findByText(/^mock/)).toBeInTheDocument()
  })

  it('exposes the tape state as an accessible label', async () => {
    renderWithClient(<StatusBar streamStatus="open" />)
    expect(await screen.findByLabelText('live')).toBeInTheDocument()
  })

  it('toggles the theme through the store', async () => {
    const user = userEvent.setup()
    renderWithClient(<StatusBar streamStatus="mock" />)
    await user.click(await screen.findByLabelText('Switch to the light theme'))
    expect(useAppStore.getState().theme).toBe('light')
    await user.click(await screen.findByLabelText('Switch to the dark theme'))
    expect(useAppStore.getState().theme).toBe('dark')
  })

  it('re-scans the trades directory on demand', async () => {
    const user = userEvent.setup()
    const { client } = renderWithClient(<StatusBar streamStatus="mock" />)
    await screen.findByText('DELAYED · yfinance')
    await user.click(screen.getByRole('button', { name: /reload/ }))
    await waitFor(() => expect(client.isFetching()).toBe(0))
    expect(screen.getByRole('button', { name: /reload/ })).toBeEnabled()
  })
})
