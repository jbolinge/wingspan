import { fireEvent, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { call } from './api/client'
import { mockRequest } from './api/mock'
import { queryKeys } from './api/queries'
import type { WarningsResponse } from './api/types'
import App from './App'
import { warningKey } from './lib/warnings'
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
  return { ...actual, call: vi.fn(actual.call) }
})

beforeEach(() => resetAppStore())
afterEach(() => {
  vi.mocked(call).mockClear()
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

  it('reaches Settings on the Data health tab when the book cannot be priced yet', async () => {
    // A fresh install: `trades` answers 503 until a market source works, and
    // Settings is where one is configured — so it must not sit behind that
    // very error.
    const actual = await vi.importActual<typeof import('./api/client')>('./api/client')
    vi.mocked(call).mockImplementation((command, args, signal) =>
      command === 'trades'
        ? Promise.reject(new Error('no market snapshot yet'))
        : actual.call(command, args, signal),
    )
    try {
      renderWithClient(<App />)
      expect(await screen.findByText('Request failed.')).toBeInTheDocument()

      await userEvent.click(screen.getByRole('button', { name: /Data health → Settings/ }))
      expect(await screen.findByRole('heading', { name: 'Settings' })).toBeInTheDocument()
      // The failure stays visible above the cards rather than replacing them.
      expect(screen.getByText('Request failed.')).toBeInTheDocument()
    } finally {
      vi.mocked(call).mockImplementation(actual.call)
    }
  })

  it('keeps the last-known book on screen through a background refetch failure', async () => {
    const { client } = renderWithClient(<App />)
    await screen.findByText('Oct Strangle')

    vi.mocked(call).mockRejectedValueOnce(new Error('boom'))
    await client.refetchQueries({ queryKey: queryKeys.trades })

    expect(await screen.findByText('Request failed.')).toBeInTheDocument()
    // The stale-but-valid book is still rendered, not blanked by the error.
    expect(screen.getByText('Oct Strangle')).toBeInTheDocument()
  })

  it('collapses the open-positions table to its header', async () => {
    const user = userEvent.setup()
    renderWithClient(<App />)
    await screen.findByText('Oct Strangle')
    await user.click(screen.getByRole('button', { name: 'Collapse Open positions' }))
    expect(screen.queryByText('Oct Strangle')).not.toBeInTheDocument()
    expect(screen.getByText(/4 open · 0 selected/)).toBeInTheDocument()
    expect(useAppStore.getState().positionsCollapsed).toBe(true)
    // No table, no drag handle to size it with.
    expect(screen.queryByRole('separator')).not.toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Expand Open positions' }))
    expect(await screen.findByText('Oct Strangle')).toBeInTheDocument()
    expect(screen.getByRole('separator')).toBeInTheDocument()
  })

  it('sizes the open-positions table from the drag handle', async () => {
    renderWithClient(<App />)
    await screen.findByText('Oct Strangle')
    const handle = screen.getByRole('separator', { name: 'Resize the open positions table' })
    // jsdom has neither layout nor pointer capture: the drag starts from a
    // 0px table and the capture calls are stubbed.
    Object.assign(handle, {
      setPointerCapture: () => undefined,
      releasePointerCapture: () => undefined,
      hasPointerCapture: () => false,
    })
    fireEvent.pointerDown(handle, { button: 0, clientY: 100, pointerId: 1 })
    fireEvent.pointerMove(handle, { clientY: 400, pointerId: 1 })
    fireEvent.pointerUp(handle, { pointerId: 1 })
    expect(useAppStore.getState().positionsHeight).toBe(300)
    fireEvent.doubleClick(handle)
    expect(useAppStore.getState().positionsHeight).toBeNull()
  })

  it('maximizes the risk graph and brings the dashboard back on Esc', async () => {
    const user = userEvent.setup()
    renderWithClient(<App />)
    await screen.findByText('Oct Strangle')
    await user.click(screen.getByRole('button', { name: /maximize/ }))
    expect(screen.queryByText('Open positions')).not.toBeInTheDocument()
    expect(screen.queryByText('All open trades')).not.toBeInTheDocument()
    expect(screen.getByRole('heading', { name: 'Risk graph' })).toBeInTheDocument()
    await user.keyboard('{Escape}')
    expect(await screen.findByText('Open positions')).toBeInTheDocument()
    expect(useAppStore.getState().graphMaximized).toBe(false)
  })

  it('badges the data-health tab with the warnings the trader has not cleared', async () => {
    const { warnings } = mockRequest('/warnings') as WarningsResponse
    renderWithClient(<App />)
    await screen.findByText('Oct Strangle')
    const badge = () => screen.getByRole('button', { name: /Data health/ }).textContent ?? ''
    await waitFor(() => expect(badge()).toBe(`Data health${warnings.length}`))

    useAppStore.getState().dismissWarnings([warningKey(warnings[0]!)])
    await waitFor(() => expect(badge()).toBe(`Data health${warnings.length - 1}`))
    // A fingerprint that matches nothing changes nothing.
    useAppStore.getState().dismissWarnings(['not-a-real-warning'])
    expect(badge()).toBe(`Data health${warnings.length - 1}`)
  })
})
