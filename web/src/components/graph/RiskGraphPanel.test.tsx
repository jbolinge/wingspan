import { fireEvent, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { mockRequest } from '../../api/mock'
import type { TradesResponse } from '../../api/types'
import { useAppStore } from '../../store/appStore'
import { renderWithClient, resetAppStore } from '../../test/utils'
import { RiskGraphPanel } from './RiskGraphPanel'

// ECharts needs a real canvas; the option it receives is unit-tested separately
// in src/lib/chartOption.test.ts.
vi.mock('./RiskChart', () => ({
  RiskChart: ({ ariaLabel }: { ariaLabel: string }) => (
    <div data-testid="risk-chart" aria-label={ariaLabel} />
  ),
}))

const trades = (mockRequest('/trades') as TradesResponse).trades

beforeEach(() => resetAppStore())

describe('RiskGraphPanel', () => {
  it('asks for a selection before drawing anything', () => {
    renderWithClient(<RiskGraphPanel trades={trades} />)
    expect(screen.getByText(/Select trades in the table/)).toBeInTheDocument()
    expect(screen.queryByTestId('risk-chart')).not.toBeInTheDocument()
  })

  it('draws the expiration line plus the T+x ladder for the selected set', async () => {
    useAppStore.getState().setSelected([trades[1]!.id])
    renderWithClient(<RiskGraphPanel trades={trades} />)
    const chart = await screen.findByTestId('risk-chart')
    expect(chart.getAttribute('aria-label')).toMatch(/expiration line plus \d+ T\+x lines/)
  })

  it('reports max profit, max loss, PoP and the breakevens', async () => {
    useAppStore.getState().setSelected([trades[1]!.id])
    renderWithClient(<RiskGraphPanel trades={trades} />)
    expect(await screen.findByText('max profit')).toBeInTheDocument()
    expect(screen.getByText('max loss')).toBeInTheDocument()
    expect(screen.getByText('PoP')).toBeInTheDocument()
    expect(screen.getByText('ATM IV')).toBeInTheDocument()
  })

  it('reads out every line at spot before the cursor has moved', async () => {
    useAppStore.getState().setSelected([trades[1]!.id])
    renderWithClient(<RiskGraphPanel trades={trades} />)
    expect(await screen.findByText('At spot')).toBeInTheDocument()
    expect(screen.getAllByText('T+0').length).toBeGreaterThan(0)
  })

  it('exposes the per-expiry vol sliders of the selected set', async () => {
    useAppStore.getState().setSelected([trades[1]!.id])
    renderWithClient(<RiskGraphPanel trades={trades} />)
    expect(await screen.findByLabelText('global')).toBeInTheDocument()
    expect(screen.getByLabelText('FEB27')).toBeInTheDocument()
    expect(screen.getByLabelText('MAR27')).toBeInTheDocument()
  })

  it('records a per-expiry vol shift in the store, keyed by expiry', async () => {
    useAppStore.getState().setSelected([trades[1]!.id])
    renderWithClient(<RiskGraphPanel trades={trades} />)
    fireEvent.change(await screen.findByLabelText('FEB27'), { target: { value: '0.02' } })
    await waitFor(() =>
      expect(useAppStore.getState().graph.volShiftByExpiry).toEqual({ '2027-02-19': 0.02 }),
    )
  })

  it('toggles include-realised, which changes the request the server sees', async () => {
    const user = userEvent.setup()
    useAppStore.getState().setSelected([trades[1]!.id])
    renderWithClient(<RiskGraphPanel trades={trades} />)
    await screen.findByTestId('risk-chart')
    await user.click(screen.getByRole('button', { name: 'include realised' }))
    expect(useAppStore.getState().graph.includeRealized).toBe(false)
  })

  it('hides a curve when its line toggle is unticked', async () => {
    const user = userEvent.setup()
    useAppStore.getState().setSelected([trades[1]!.id])
    renderWithClient(<RiskGraphPanel trades={trades} />)
    const toggle = (await screen.findAllByRole('checkbox')).at(0) as HTMLInputElement
    await user.click(toggle)
    expect(useAppStore.getState().graph.hiddenLines).toHaveLength(1)
  })

  it('locks the y-axis to the current extent', async () => {
    const user = userEvent.setup()
    useAppStore.getState().setSelected([trades[1]!.id])
    renderWithClient(<RiskGraphPanel trades={trades} />)
    await screen.findByTestId('risk-chart')
    await user.click(screen.getByRole('button', { name: 'lock y-axis' }))
    const graph = useAppStore.getState().graph
    expect(graph.yLock).toBe(true)
    expect(graph.yMin).toBeLessThan(graph.yMax ?? 0)
  })

  it('shows the price-slice table on demand', async () => {
    const user = userEvent.setup()
    useAppStore.getState().setSelected([trades[1]!.id])
    renderWithClient(<RiskGraphPanel trades={trades} />)
    await screen.findByTestId('risk-chart')
    await user.click(screen.getByRole('button', { name: 'price slices' }))
    expect(await screen.findByText('Price slices')).toBeInTheDocument()
    expect(screen.getByText('vs spot')).toBeInTheDocument()
  })

  it('computes the what-if answer from the server, not from the chart', async () => {
    const user = userEvent.setup()
    useAppStore.getState().setSelected([trades[1]!.id])
    renderWithClient(<RiskGraphPanel trades={trades} />)
    await screen.findByTestId('risk-chart')
    await user.click(screen.getByRole('button', { name: /what-if off/ }))
    expect(await screen.findByLabelText('What-if price')).toBeInTheDocument()
    await waitFor(() => expect(screen.getByText(/vs now/)).toBeInTheDocument())
  })

  it('lets the date list be edited away from the server preset', async () => {
    const user = userEvent.setup()
    useAppStore.getState().setSelected([trades[1]!.id])
    renderWithClient(<RiskGraphPanel trades={trades} />)
    await screen.findByTestId('risk-chart')
    expect(screen.getByText(/Server preset in use/)).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'edit preset' }))
    await waitFor(() => expect(useAppStore.getState().graph.dates).not.toBeNull())
    expect(screen.getAllByText('T+0').length).toBeGreaterThan(0)
  })
})
