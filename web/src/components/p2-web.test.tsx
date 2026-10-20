/**
 * Round-3 audit, the controls around the graph and the leg rows: line
 * toggles hide an expiration by date, later expirations get their own
 * swatch, a dropped date is surfaced, the what-if says what an expiry date
 * prices, every leg-row greek header carries its per-contract units, and the
 * settlement hint matches the engine's valuation rule.
 */

import { render, screen, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it } from 'vitest'
import { mockRequest } from '../api/mock'
import type { RiskGraphResponse, TradesResponse } from '../api/types'
import { useAppStore } from '../store/appStore'
import { renderWithClient, resetAppStore } from '../test/utils'
import { EXPIRED_LEG_HINT } from './DataHealth'
import { GraphControls } from './graph/GraphControls'
import { WHAT_IF_EXPIRY_NOTE, WhatIfPanel } from './graph/WhatIfPanel'
import { TradeDetail } from './TradeDetail'
import { LEG_GREEK_UNITS } from './units'

beforeEach(() => resetAppStore())

const data: RiskGraphResponse = {
  as_of: '2026-09-15T16:00:00Z',
  spot: 6200,
  prices: [6000, 6200, 6400],
  lines: [
    { date: '2026-09-15', kind: 't0', key: 't0', label: 'T+0', pnl: [0, 0, 0] },
    { date: '2026-10-16', kind: 'expiry', key: 'expiry', label: 'Exp 2026-10-16', pnl: [0, 0, 0] },
    { date: '2026-12-18', kind: 'expiry', key: 'expiry:2026-12-18', label: 'Exp 2026-12-18', pnl: [0, 0, 0] },
  ],
  t0_greeks: null,
  per_trade: [],
  breakevens: {},
  max_profit_on_range: 0,
  max_loss_on_range: 0,
  pop: null,
  sigma_bands: { one: [6100, 6300], two: [6050, 6350] },
  front_atm_iv: null,
  terminal: { key: 'expiry', date: '2026-10-16', kind: 'expiry', drawn: true, analysis_breakevens: [] },
  warnings: [
    {
      severity: 'info',
      code: 'stale_graph_date',
      message: 'expiry:2026-09-11 has already closed; the line was dropped from the graph',
      trade_id: null,
      contract: null,
    },
  ],
}

function renderControls() {
  return renderWithClient(
    <GraphControls
      data={data}
      view={null}
      trades={[]}
      expiries={['2026-10-16', '2026-12-18']}
      frontDte={31}
      spot={6200}
      selected={['a']}
      theme="dark"
      baseline={null}
    />,
  )
}

describe('GraphControls line toggles', () => {
  it('hide an expiration by its date, not by the bare key', async () => {
    const user = userEvent.setup()
    renderControls()
    await user.click(screen.getByLabelText('Show EXP Oct 16 2026'))
    expect(useAppStore.getState().graph.hiddenLines).toEqual(['expiry:2026-10-16'])
    expect(screen.getByLabelText('Show EXP Oct 16 2026')).not.toBeChecked()
    expect(screen.getByLabelText('Show EXP Dec 18 2026')).toBeChecked()
  })

  it('draw a later expiration’s swatch with its own dash, the first one solid', () => {
    renderControls()
    const swatchOf = (label: string) => screen.getByLabelText(label).closest('label')!
    expect(within(swatchOf('Show EXP Oct 16 2026')).queryByTestId('swatch-dashed')).toBeNull()
    expect(within(swatchOf('Show EXP Dec 18 2026')).getByTestId('swatch-dashed')).toBeInTheDocument()
  })

  it('surface the server’s dropped-date warning', () => {
    renderControls()
    expect(screen.getByTestId('dropped-date')).toHaveTextContent(/expiry:2026-09-11 has already closed/)
  })
})

describe('WhatIfPanel', () => {
  it('says that a date on an expiry prices at that expiration’s settlement', () => {
    useAppStore.getState().patchWhatIf({ enabled: true, date: '2026-10-16' })
    renderWithClient(
      <WhatIfPanel selected={['a']} spot={6200} baseline={null} today="2026-09-15" expiries={['2026-10-16']} />,
    )
    expect(screen.getByTestId('what-if-expiry-note')).toHaveTextContent(
      /Oct 16 2026 is an expiry: priced at that expiration’s settlement/,
    )
    expect(screen.getByLabelText('What-if date')).toHaveAttribute('min', '2026-09-15')
  })

  it('states the rule on any other date, and shows the server day for a stale stored one', () => {
    useAppStore.getState().patchWhatIf({ enabled: true, date: '2026-09-01' })
    renderWithClient(
      <WhatIfPanel selected={['a']} spot={6200} baseline={null} today="2026-09-15" expiries={['2026-10-16']} />,
    )
    expect(screen.getByTestId('what-if-expiry-note')).toHaveTextContent(WHAT_IF_EXPIRY_NOTE)
    expect(screen.getByLabelText('What-if date')).toHaveValue('2026-09-15')
  })
})

describe('TradeDetail leg-row greek headers', () => {
  it('carry the per-contract, per-share units on all four greeks', () => {
    const trade = (mockRequest('/trades') as TradesResponse).trades.find((t) => t.name === 'Oct Strangle')!
    render(<TradeDetail trade={trade} />)
    for (const [symbol, key] of [
      ['Δ', 'delta'],
      ['Γ', 'gamma'],
      ['Θ', 'theta'],
      ['V', 'vega'],
    ] as const) {
      const header = screen.getAllByRole('columnheader').find((th) => th.textContent?.startsWith(symbol))!
      const tip = header.querySelector('[title]')
      expect(tip).toHaveAttribute('title', LEG_GREEK_UNITS[key])
      expect(LEG_GREEK_UNITS[key]).toMatch(/^Per contract, per share/)
      expect(LEG_GREEK_UNITS[key]).not.toMatch(/position P&L/)
    }
  })
})

describe('the expired_leg_unknown_settlement hint', () => {
  it('describes the intrinsic valuation and asks for a Closed row, as the engine does', () => {
    const hint = EXPIRED_LEG_HINT
    expect(hint).toMatch(/valued at intrinsic against the SPX level recorded for its expiry date/)
    expect(hint).toMatch(/the current SPX level/)
    expect(hint).toMatch(/Closed row in the Legs sheet/)
    expect(hint).not.toMatch(/^No settlement was recorded for an expired leg, so it was valued at zero/)
  })
})
