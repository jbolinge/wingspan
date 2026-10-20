/**
 * Round-3 audit, the risk-graph panel: the header names the line its
 * readouts are measured on (and says when it is not drawn), a null PoP / ATM
 * IV is an em dash, stale persisted dates and hidden-line keys are pruned
 * against the server date, later expiration lines are styled apart, and the
 * chart's description counts its lines by kind.
 */

import { screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { call } from '../../api/client'
import { mockRequest, mockRiskGraph } from '../../api/mock'
import type { RiskGraphRequest, RiskGraphResponse, TradesResponse } from '../../api/types'
import { EM_DASH } from '../../lib/format'
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

beforeEach(() => resetAppStore())
afterEach(() => {
  vi.mocked(call).mockClear()
  riskChartOptions.length = 0
})

const trades = () => (mockRequest('/trades') as TradesResponse).trades
const STRANGLE = 'oct-strangle-2026-08-24'
const CONDOR = 'nov-condor-2026-08-26'

function readoutValue(label: string): string {
  return screen.getByText(label).nextElementSibling?.textContent ?? ''
}

/** Answers every risk-graph call from the mock, edited by `edit`. */
function editRiskGraph(edit: (graph: RiskGraphResponse) => RiskGraphResponse): void {
  const actual = vi.mocked(call).getMockImplementation()!
  vi.mocked(call).mockImplementation(async (command, args, signal) => {
    if (command === 'risk_graph') {
      return edit(mockRiskGraph((args as { request: RiskGraphRequest }).request)) as never
    }
    return actual(command, args, signal)
  })
}

async function renderPanel(): Promise<void> {
  renderWithClient(<RiskGraphPanel trades={trades()} />)
  await screen.findByTestId('risk-chart')
  await waitFor(() => expect(readoutValue('max profit')).not.toBe(''))
}

describe('the header readouts', () => {
  it('name the expiration line they are measured on', async () => {
    useAppStore.getState().setSelected([STRANGLE])
    await renderPanel()
    expect(await screen.findByTestId('terminal-label')).toHaveTextContent('at EXP Oct 16 2026')
    expect(screen.getByTestId('terminal-label')).not.toHaveTextContent('not drawn')
  })

  it('say the expiration line is not drawn when the date list has none', async () => {
    useAppStore.getState().setSelected([STRANGLE])
    useAppStore.getState().patchGraph({ dates: ['today', '+10d'] })
    await renderPanel()
    await waitFor(() =>
      expect(screen.getByTestId('terminal-label')).toHaveTextContent('at EXP Oct 16 2026 · not drawn'),
    )
    expect(screen.getByTestId('terminal-label').parentElement).toHaveAttribute(
      'title',
      expect.stringMatching(/which is not drawn/),
    )
  })

  it('read BE off the terminal line the server names', async () => {
    useAppStore.getState().setSelected([STRANGLE])
    editRiskGraph((graph) => ({
      ...graph,
      breakevens: { ...graph.breakevens, 't+12': [7111] },
      terminal: { key: 't+12', date: '2026-09-12', kind: 'tplus', drawn: true, analysis_breakevens: [] },
    }))
    await renderPanel()
    await waitFor(() => expect(readoutValue('BE')).toBe('7,111'))
    expect(screen.getByTestId('terminal-label')).toHaveTextContent('at T+12')
  })

  it('print a null PoP and ATM IV as an em dash, never 0.0%', async () => {
    useAppStore.getState().setSelected([STRANGLE])
    editRiskGraph((graph) => ({ ...graph, pop: null, front_atm_iv: null }))
    await renderPanel()
    await waitFor(() => expect(readoutValue('PoP')).toBe(EM_DASH))
    expect(readoutValue('ATM IV')).toBe(EM_DASH)
    expect(screen.getByText(/Front ATM IV/)).toHaveTextContent(`Front ATM IV ${EM_DASH}`)
  })
})

describe('stale persisted graph settings', () => {
  it('are pruned against the server date: past expiry: specs and hidden keys that can never match', async () => {
    useAppStore.getState().setSelected([STRANGLE])
    useAppStore.getState().patchGraph({
      dates: ['today', 'expiry:2026-08-21', 'expiry:2026-10-16'],
      // A legacy bare key, a settled date, a T+N this ladder lacks (kept:
      // it still names a day), and a live one.
      hiddenLines: ['expiry', 'expiry:2026-08-21', 't+99', 't0'],
    })
    await renderPanel()
    await waitFor(() => expect(useAppStore.getState().graph.dates).toEqual(['today', 'expiry:2026-10-16']))
    await waitFor(() => expect(useAppStore.getState().graph.hiddenLines).toEqual(['t+99', 't0']))
  })
})

type Series = { name?: string; lineStyle?: { width?: number; type?: unknown } }

describe('several expiration lines', () => {
  it('are styled apart and counted as expirations in the chart description', async () => {
    useAppStore.getState().setSelected([STRANGLE, CONDOR])
    useAppStore.getState().patchGraph({ dates: ['today', 'expiry:2026-10-16', 'expiry:2026-11-20'] })
    await renderPanel()
    await waitFor(() =>
      expect(screen.getByTestId('risk-chart')).toHaveAttribute(
        'aria-label',
        'Risk graph for 2 selected trades: 2 expiration lines plus 1 T+x line',
      ),
    )
    const option = riskChartOptions.at(-1) as { series: Series[] }
    const oct = option.series.find((s) => s.name === 'EXP Oct 16 2026')!
    const nov = option.series.find((s) => s.name === 'EXP Nov 20 2026')!
    expect(oct.lineStyle).toMatchObject({ width: 3 })
    expect(oct.lineStyle?.type).toBeUndefined()
    expect(Array.isArray(nov.lineStyle?.type)).toBe(true)
    expect(nov.lineStyle!.width).toBeLessThan(3)
    // The inset's swatch for the later line carries the same pattern.
    const dashed = screen.getAllByTestId('swatch-dashed')
    expect(dashed.length).toBeGreaterThan(0)
    expect(dashed[0]).toHaveAttribute('data-dash', (nov.lineStyle!.type as number[]).join(','))
  })
})
