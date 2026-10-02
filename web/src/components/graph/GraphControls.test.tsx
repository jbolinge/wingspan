/**
 * Line identity is the server's `key`, not the date.
 *
 * `/risk-graph` guarantees `key` is unique per line but says nothing of the
 * sort about `date`: a 0-DTE position puts the T+0 line and the expiration
 * line on the same calendar day (the server's own default preset for such a
 * position is `[Today, Expiry]`). Keying the checkboxes on the date makes one
 * tick hide both curves — and gives React duplicate list keys.
 */

import { screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it } from 'vitest'
import type { OptionContract, RiskGraphResponse } from '../../api/types'
import { compareLinesOf, lineLabel, type CompareLines } from '../../lib/chartOption'
import { toModelLegs } from '../../lib/model'
import { useAppStore } from '../../store/appStore'
import { renderWithClient, resetAppStore } from '../../test/utils'
import { GraphControls } from './GraphControls'

const SAME_DAY = '2026-09-18'

const zeroDte: RiskGraphResponse = {
  as_of: `${SAME_DAY}T17:00:00Z`,
  spot: 6200,
  prices: [6000, 6200, 6400],
  lines: [
    { date: SAME_DAY, kind: 't0', key: 't0', label: 'T+0', pnl: [-100, 0, 100] },
    {
      date: SAME_DAY,
      kind: 'expiry',
      key: 'expiry',
      label: `Exp ${SAME_DAY}`,
      pnl: [-200, 0, 200],
    },
  ],
  t0_greeks: null,
  per_trade: [{ trade_id: 'm1', date: SAME_DAY, key: 't0', pnl: [-100, 0, 100] }],
  breakevens: { expiry: [6200] },
  max_profit_on_range: 200,
  max_loss_on_range: -200,
  pop: 0.5,
  sigma_bands: { one: [6100, 6300], two: [6050, 6350] },
  front_atm_iv: 0.16,
  warnings: [],
}

function renderControls({
  selected = ['m1'],
  modelled = false,
  compare = null,
}: { selected?: string[]; modelled?: boolean; compare?: CompareLines | null } = {}) {
  return renderWithClient(
    <GraphControls
      data={zeroDte}
      view={null}
      trades={[]}
      expiries={[SAME_DAY]}
      frontDte={0}
      spot={zeroDte.spot}
      effectiveDates={zeroDte.lines.map((l) => l.date)}
      selected={selected}
      modelLegs={modelled ? toModelLegs(useAppStore.getState().model.legs) : []}
      compare={compare}
      theme="dark"
      baseline={null}
    />,
  )
}

beforeEach(() => resetAppStore())

describe('GraphControls line toggles', () => {
  it('gives two lines sharing a date their own checkbox', () => {
    renderControls()
    expect(screen.getByRole('checkbox', { name: 'Show T+0' })).toBeInTheDocument()
    expect(screen.getByRole('checkbox', { name: /^Show EXP / })).toBeInTheDocument()
  })

  it('hides only the line that was unticked, not its same-date neighbour', async () => {
    const user = userEvent.setup()
    renderControls()
    const t0 = screen.getByRole('checkbox', { name: 'Show T+0' })
    const expiry = screen.getByRole('checkbox', { name: /^Show EXP / })

    await user.click(t0)
    expect(useAppStore.getState().graph.hiddenLines).toEqual(['t0'])
    expect(t0).not.toBeChecked()
    expect(expiry).toBeChecked()
  })
})

describe('GraphControls compare position', () => {
  const PUT: OptionContract = {
    underlying: 'SPX',
    root: 'SPXW',
    expiry: SAME_DAY,
    settlement: 'pm',
    strike: 6000,
    right: 'put',
    multiplier: 100,
    osi: 'SPXW  260918P06000000',
    tos_code: 'SPXW260918P6000',
  }

  beforeEach(() => useAppStore.getState().setModelQty(PUT, -1))

  it('compares a selection against the model, by default, listing every ghost line', () => {
    expect(useAppStore.getState().model.comparePosition).toBe(true)
    renderControls({ modelled: true, compare: compareLinesOf(zeroDte, []) })
    const toggle = screen.getByRole('button', { name: 'compare position' })
    expect(toggle).toBeEnabled()
    expect(toggle).toHaveAttribute('aria-pressed', 'true')
    expect(screen.getByText('without the model')).toBeInTheDocument()
    for (const line of zeroDte.lines) {
      expect(screen.getByText(`${lineLabel(line, SAME_DAY)} (position)`)).toBeInTheDocument()
    }
  })

  it('lists no ghost lines while the chart draws none', () => {
    renderControls({ modelled: true })
    expect(screen.queryByText('without the model')).not.toBeInTheDocument()
    expect(screen.queryByText(/\(position\)$/)).not.toBeInTheDocument()
  })

  // Nothing selected: the graph is the model alone, and "the position
  // without the model" would be nothing — or, on the wire, the whole book.
  it('has nothing to compare against with nothing selected', () => {
    renderControls({ selected: [], modelled: true })
    const toggle = screen.getByRole('button', { name: 'compare position' })
    expect(toggle).toBeDisabled()
    expect(toggle).toHaveAttribute('title', 'Select trades to compare against')
    expect(screen.queryByText(/\(position\)$/)).not.toBeInTheDocument()
  })

  it('says the model is hidden before it says to select trades', () => {
    renderControls({ selected: [], modelled: false })
    expect(screen.getByRole('button', { name: 'compare position' })).toHaveAttribute(
      'title',
      'The model is hidden from the graph — show it to compare',
    )
  })
})
