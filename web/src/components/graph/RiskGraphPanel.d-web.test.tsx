/**
 * The risk graph over a combined selection — one SPX trade plus the XSP
 * Hedge (D8): the header and the greek strip read the combined response,
 * the breakeven markers are the canonical expiration line's alone, and the
 * header says the axis is SPX points for a selection holding XSP.
 */

import { screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { call } from '../../api/client'
import { mockRequest, mockRiskGraph } from '../../api/mock'
import type { RiskGraphRequest, TradesResponse } from '../../api/types'
import { fmtNumber, fmtPnl } from '../../lib/format'
import { useAppStore } from '../../store/appStore'
import { renderWithClient, resetAppStore } from '../../test/utils'
import { RiskGraphPanel } from './RiskGraphPanel'

const { riskChartOptions } = vi.hoisted(() => ({ riskChartOptions: [] as unknown[] }))
vi.mock('./RiskChart', () => ({
  RiskChart: (props: { ariaLabel: string; option: unknown }) => {
    riskChartOptions.push(props.option)
    return <div data-testid="risk-chart" aria-label={props.ariaLabel} />
  },
}))
vi.mock('../../api/client', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../api/client')>()
  return { ...actual, call: vi.fn(actual.call) }
})

beforeEach(() => {
  resetAppStore()
  vi.useFakeTimers({ toFake: ['Date'] })
  vi.setSystemTime(new Date(2026, 7, 31, 11, 0, 0))
})
afterEach(() => {
  vi.useRealTimers()
  vi.mocked(call).mockClear()
  riskChartOptions.length = 0
})

function lastRiskGraphRequest(): RiskGraphRequest {
  const calls = vi.mocked(call).mock.calls.filter((args) => args[0] === 'risk_graph')
  return (calls.at(-1)![1] as { request: RiskGraphRequest }).request
}

function readoutValue(label: string): string {
  return screen.getByText(label).nextElementSibling?.textContent ?? ''
}

type Series = { name?: string; type?: string; data?: unknown[] }

describe('RiskGraphPanel over SPX + XSP', () => {
  it('reads the header and the strip off the combined response, marking only the canonical breakevens', async () => {
    const trades = (mockRequest('/trades') as TradesResponse).trades
    const spx = trades.find((t) => t.name === 'Dec Fly')!
    const xsp = trades.find((t) => t.name === 'XSP Hedge')!
    useAppStore.getState().setSelected([spx.id, xsp.id])
    renderWithClient(<RiskGraphPanel trades={trades} />)
    await screen.findByTestId('risk-chart')
    await waitFor(() => expect(readoutValue('max profit')).not.toBe(''))

    const request = lastRiskGraphRequest()
    expect([...request.trade_ids].sort()).toEqual([spx.id, xsp.id].sort())
    const expected = mockRiskGraph(request)

    expect(readoutValue('max profit')).toBe(fmtPnl(expected.max_profit_on_range, 0))
    expect(readoutValue('max loss')).toBe(fmtPnl(expected.max_loss_on_range, 0))
    expect(readoutValue('BE')).toBe(
      (expected.breakevens.expiry ?? []).map((be) => fmtNumber(be, 0)).join(' / ') || '—',
    )
    expect(screen.getByText('BE').parentElement).toHaveTextContent('SPX pts')
    expect(screen.getByTestId('xsp-header-note')).toBeInTheDocument()

    const option = riskChartOptions.at(-1) as { series: Series[] }
    // The breakeven diamonds: the canonical expiration line's, nothing else.
    const first = expected.prices[0]!
    const last = expected.prices.at(-1)!
    const canonical = (expected.breakevens.expiry ?? []).filter((be) => be >= first && be <= last)
    const scatter = option.series.find((s) => s.name === 'breakeven')
    expect((scatter?.data ?? []) as [number, number][]).toEqual(canonical.map((be) => [be, 0]))

    // The strip's P&L row reads the combined T+0 line at its sampled prices.
    const t0 = expected.lines.find((line) => line.key === 't0')!
    const pnlBars = option.series.find((s) => s.name === 'strip-pnl')!
    for (const { value } of pnlBars.data as { value: [number, number] }[]) {
      const index = expected.prices.indexOf(value[0])
      expect(index).toBeGreaterThanOrEqual(0)
      expect(value[1]).toBe(t0.pnl[index])
    }
    // …and the strip's delta row the combined T+0 greeks.
    const deltaBars = option.series.find((s) => s.name === 'strip-delta')!
    for (const { value } of deltaBars.data as { value: [number, number] }[]) {
      expect(value[1]).toBe(expected.t0_greeks!.delta[expected.prices.indexOf(value[0])])
    }
  })
})
