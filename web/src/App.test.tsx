import { screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import App from './App'
import { useAppStore } from './store/appStore'
import { renderWithClient, resetAppStore } from './test/utils'

vi.mock('./components/graph/RiskChart', () => ({
  RiskChart: ({ ariaLabel }: { ariaLabel: string }) => (
    <div data-testid="risk-chart" aria-label={ariaLabel} />
  ),
}))

beforeEach(() => resetAppStore())

describe('App', () => {
  it('boots into the positions tab against the mock API', async () => {
    renderWithClient(<App />)
    expect(await screen.findByText('M1')).toBeInTheDocument()
    expect(screen.getByText('All open trades')).toBeInTheDocument()
    expect(screen.getByText(/Nothing selected/)).toBeInTheDocument()
  })

  it('drives the portfolio card and the risk graph from the selection', async () => {
    const user = userEvent.setup()
    renderWithClient(<App />)
    await screen.findByText('M1')
    await user.click(screen.getByRole('button', { name: 'all' }))
    await waitFor(() => expect(screen.queryByText(/Nothing selected/)).not.toBeInTheDocument())
    expect(await screen.findByTestId('risk-chart')).toBeInTheDocument()
    expect(screen.getByText(/6 trades · P&L/)).toBeInTheDocument()
  })

  it('applies the theme to the document element', async () => {
    const user = userEvent.setup()
    renderWithClient(<App />)
    await screen.findByText('M1')
    expect(document.documentElement.dataset.theme).toBe('dark')
    await user.click(screen.getByLabelText('Switch to the light theme'))
    await waitFor(() => expect(document.documentElement.dataset.theme).toBe('light'))
  })

  it('switches to the closed tab and back', async () => {
    const user = userEvent.setup()
    renderWithClient(<App />)
    await screen.findByText('M1')
    await user.click(screen.getByRole('button', { name: 'Closed' }))
    expect(await screen.findByText('LPTA')).toBeInTheDocument()
    expect(useAppStore.getState().tab).toBe('closed')
    await user.click(screen.getByRole('button', { name: 'Positions' }))
    expect(await screen.findByText('Open positions')).toBeInTheDocument()
  })

  it('badges the data-health tab with the warning count and opens it', async () => {
    const user = userEvent.setup()
    renderWithClient(<App />)
    await screen.findByText('M1')
    const health = await screen.findByRole('button', { name: /Data health/ })
    await user.click(health)
    expect(await screen.findByText('duplicate_execution_claim')).toBeInTheDocument()
  })

  it('drops a stale selection when the trade disappears from the book', async () => {
    useAppStore.getState().setSelected(['ghost-1999-01-01'])
    renderWithClient(<App />)
    await screen.findByText('M1')
    await waitFor(() => expect(useAppStore.getState().selected).toEqual([]))
  })
})
