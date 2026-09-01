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
import type { RiskGraphResponse } from '../../api/types'
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
  per_trade: [{ trade_id: 'm1', date: SAME_DAY, key: 't0', pnl: [-100, 0, 100] }],
  breakevens: { expiry: [6200] },
  max_profit_on_range: 200,
  max_loss_on_range: -200,
  pop: 0.5,
  sigma_bands: { one: [6100, 6300], two: [6050, 6350] },
  front_atm_iv: 0.16,
  warnings: [],
}

function renderControls() {
  return renderWithClient(
    <GraphControls
      data={zeroDte}
      trades={[]}
      expiries={[SAME_DAY]}
      frontDte={0}
      spot={zeroDte.spot}
      effectiveDates={zeroDte.lines.map((l) => l.date)}
      selected={['m1']}
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
