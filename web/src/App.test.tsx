import { screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { apiGet } from './api/client'
import { queryKeys } from './api/queries'
import App from './App'
import { useAppStore } from './store/appStore'
import { renderWithClient, resetAppStore } from './test/utils'

vi.mock('./components/graph/RiskChart', () => ({
  RiskChart: ({ ariaLabel }: { ariaLabel: string }) => (
    <div data-testid="risk-chart" aria-label={ariaLabel} />
  ),
}))

// Wraps the real implementation so a test can fail one call without losing
// the mock API responses underneath.
vi.mock('./api/client', async (importOriginal) => {
  const actual = await importOriginal<typeof import('./api/client')>()
  return { ...actual, apiGet: vi.fn(actual.apiGet) }
})

beforeEach(() => resetAppStore())
afterEach(() => {
  vi.mocked(apiGet).mockClear()
})

describe('App', () => {
  it('boots into the positions tab against the mock API', async () => {
    renderWithClient(<App />)
    expect(await screen.findByText('Oct Strangle')).toBeInTheDocument()
    expect(screen.getByText('All open trades')).toBeInTheDocument()
    expect(screen.getByText(/Nothing selected/)).toBeInTheDocument()
  })

  it('drives the portfolio card and the risk graph from the selection', async () => {
    const user = userEvent.setup()
    renderWithClient(<App />)
    await screen.findByText('Oct Strangle')
    await user.click(screen.getByRole('button', { name: 'all' }))
    await waitFor(() => expect(screen.queryByText(/Nothing selected/)).not.toBeInTheDocument())
    expect(await screen.findByTestId('risk-chart')).toBeInTheDocument()
    expect(screen.getByText(/4 trades · P&L/)).toBeInTheDocument()
  })

  it('applies the theme to the document element', async () => {
    const user = userEvent.setup()
    renderWithClient(<App />)
    await screen.findByText('Oct Strangle')
    expect(document.documentElement.dataset.theme).toBe('dark')
    await user.click(screen.getByLabelText('Switch to the light theme'))
    await waitFor(() => expect(document.documentElement.dataset.theme).toBe('light'))
  })

  it('switches to the closed tab and back', async () => {
    const user = userEvent.setup()
    renderWithClient(<App />)
    await screen.findByText('Oct Strangle')
    await user.click(screen.getByRole('button', { name: 'Closed' }))
    expect(await screen.findByText('Aug Hedge')).toBeInTheDocument()
    expect(useAppStore.getState().tab).toBe('closed')
    await user.click(screen.getByRole('button', { name: 'Positions' }))
    expect(await screen.findByText('Open positions')).toBeInTheDocument()
  })

  it('badges the data-health tab with the warning count and opens it', async () => {
    const user = userEvent.setup()
    renderWithClient(<App />)
    await screen.findByText('Oct Strangle')
    const health = await screen.findByRole('button', { name: /Data health/ })
    await user.click(health)
    expect(await screen.findByText('duplicate_execution_claim')).toBeInTheDocument()
  })

  it('drops a stale selection when the trade disappears from the book', async () => {
    useAppStore.getState().setSelected(['ghost-1999-01-01'])
    renderWithClient(<App />)
    await screen.findByText('Oct Strangle')
    await waitFor(() => expect(useAppStore.getState().selected).toEqual([]))
  })

  it('keeps the last-known book on screen through a background refetch failure', async () => {
    const { client } = renderWithClient(<App />)
    await screen.findByText('Oct Strangle')

    vi.mocked(apiGet).mockRejectedValueOnce(new Error('boom'))
    await client.refetchQueries({ queryKey: queryKeys.trades })

    expect(await screen.findByText('Request failed.')).toBeInTheDocument()
    // The stale-but-valid book is still rendered, not blanked by the error.
    expect(screen.getByText('Oct Strangle')).toBeInTheDocument()
  })
})
