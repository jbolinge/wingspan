import { screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it } from 'vitest'
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

  it('shows the SPX tape with its change', async () => {
    renderWithClient(<StatusBar streamStatus="mock" />)
    expect(await screen.findByText('SPX')).toBeInTheDocument()
    expect(await screen.findByText('7,302.15')).toBeInTheDocument()
    expect(screen.getByText(/-18\.35/)).toBeInTheDocument()
  })

  it('flags that responses are coming from the mock layer', async () => {
    renderWithClient(<StatusBar streamStatus="mock" />)
    expect(await screen.findByText(/^mock/)).toBeInTheDocument()
  })

  it('exposes the SSE state as an accessible label', async () => {
    renderWithClient(<StatusBar streamStatus="open" />)
    expect(await screen.findByLabelText('SSE live')).toBeInTheDocument()
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
