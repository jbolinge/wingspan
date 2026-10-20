/**
 * Round-3 review fixes in the risk-graph panel: a hidden expiration line
 * survives a look at another trade, and the BE readout shows the analysis
 * axis's crossings when the drawn window misses them.
 */

import { act, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { call } from '../../api/client'
import { mockRequest, mockRiskGraph } from '../../api/mock'
import type { RiskGraphRequest, RiskGraphResponse, TradesResponse } from '../../api/types'
import { useAppStore } from '../../store/appStore'
import { renderWithClient, resetAppStore } from '../../test/utils'
import { RiskGraphPanel } from './RiskGraphPanel'

vi.mock('./RiskChart', () => ({
  RiskChart: (props: { ariaLabel: string }) => <div data-testid="risk-chart" aria-label={props.ariaLabel} />,
}))
vi.mock('../../api/client', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../api/client')>()
  return { ...actual, call: vi.fn(actual.call) }
})

beforeEach(() => resetAppStore())
afterEach(() => vi.mocked(call).mockClear())

const trades = () => (mockRequest('/trades') as TradesResponse).trades
const STRANGLE = 'oct-strangle-2026-08-24'
const CONDOR = 'nov-condor-2026-08-26'

function readoutValue(label: string): string {
  return screen.getByText(label).nextElementSibling?.textContent ?? ''
}

function riskGraphCalls(): RiskGraphRequest[] {
  return vi
    .mocked(call)
    .mock.calls.filter(([command]) => command === 'risk_graph')
    .map(([, args]) => (args as { request: RiskGraphRequest }).request)
}

describe('a hidden expiration line', () => {
  it('stays hidden after the trader looks at another trade and comes back', async () => {
    useAppStore.getState().setSelected([STRANGLE])
    useAppStore.getState().patchGraph({ hiddenLines: ['expiry:2026-10-16'] })
    renderWithClient(<RiskGraphPanel trades={trades()} />)
    await screen.findByTestId('risk-chart')
    await waitFor(() => expect(readoutValue('max profit')).not.toBe(''))

    act(() => useAppStore.getState().setSelected([CONDOR]))
    await waitFor(() => expect(riskGraphCalls().some((r) => r.trade_ids.includes(CONDOR))).toBe(true))
    await waitFor(() => expect(screen.getByTestId('terminal-label')).toHaveTextContent(/Nov 20 2026/))
    expect(useAppStore.getState().graph.hiddenLines).toEqual(['expiry:2026-10-16'])

    act(() => useAppStore.getState().setSelected([STRANGLE]))
    await waitFor(() => expect(screen.getByTestId('terminal-label')).toHaveTextContent(/Oct 16 2026/))
    expect(useAppStore.getState().graph.hiddenLines).toEqual(['expiry:2026-10-16'])
  })
})

describe('the BE readout', () => {
  it('shows the analysis-axis breakevens, marked off-range, when the window has none', async () => {
    const actual = vi.mocked(call).getMockImplementation()!
    vi.mocked(call).mockImplementation(async (command, args, signal) => {
      if (command === 'risk_graph') {
        const graph: RiskGraphResponse = mockRiskGraph((args as { request: RiskGraphRequest }).request)
        const lo = graph.prices[0]!
        const hi = graph.prices[graph.prices.length - 1]!
        return {
          ...graph,
          breakevens: { ...graph.breakevens, [graph.terminal!.key]: [] },
          terminal: { ...graph.terminal!, analysis_breakevens: [lo - 100, hi + 100] },
        } as never
      }
      return actual(command, args, signal)
    })
    useAppStore.getState().setSelected([STRANGLE])
    renderWithClient(<RiskGraphPanel trades={trades()} />)
    await screen.findByTestId('risk-chart')
    await waitFor(() => expect(readoutValue('BE')).toMatch(/\(off-range\) \/ .*\(off-range\)/))
  })
})
