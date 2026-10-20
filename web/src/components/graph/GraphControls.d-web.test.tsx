/** Server-authoritative line labels (D1) and SPX-point price labels (D5). */

import { screen } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { RiskGraphResponse } from '../../api/types'
import { useAppStore } from '../../store/appStore'
import { renderWithClient, resetAppStore } from '../../test/utils'
import { SPX_POINTS, XSP_AXIS_NOTE } from '../units'
import { GraphControls } from './GraphControls'

const data: RiskGraphResponse = {
  as_of: '2026-09-01T00:30:00Z',
  spot: 6200,
  prices: [6000, 6200, 6400],
  lines: [
    { date: '2026-08-31', kind: 't0', key: 't0', label: 'T+0', pnl: [0, 0, 0] },
    { date: '2026-09-15', kind: 'tplus', key: 't+15', label: 'T+15', pnl: [0, 0, 0] },
    { date: '2026-12-18', kind: 'expiry', key: 'expiry', label: 'Exp 2026-12-18', pnl: [0, 0, 0] },
  ],
  t0_greeks: null,
  per_trade: [],
  breakevens: {},
  max_profit_on_range: 0,
  max_loss_on_range: 0,
  pop: 0.5,
  sigma_bands: { one: [6100, 6300], two: [6050, 6350] },
  front_atm_iv: 0.16,
  warnings: [],
}

beforeEach(() => {
  resetAppStore()
  // Local midnight has passed; New York (the server) is still on Aug 31.
  vi.useFakeTimers({ toFake: ['Date'] })
  vi.setSystemTime(new Date(2026, 8, 1, 0, 30))
})
afterEach(() => vi.useRealTimers())

function renderControls(holdsXsp = false) {
  return renderWithClient(
    <GraphControls
      data={data}
      view={null}
      trades={[]}
      expiries={['2026-12-18']}
      frontDte={109}
      spot={6200}
      selected={['m1']}
      theme="dark"
      baseline={null}
      holdsXsp={holdsXsp}
    />,
  )
}

describe('GraphControls', () => {
  it('names the line toggles by the server keys, not the browser day', () => {
    renderControls()
    expect(screen.getByLabelText('Show T+15')).toBeInTheDocument()
    expect(screen.queryByLabelText('Show T+14')).not.toBeInTheDocument()
  })

  it('labels the spot override and the absolute range in SPX points', () => {
    useAppStore.getState().patchGraph({ rangeMode: 'abs' })
    const { container } = renderControls()
    expect(container.querySelector('[data-unit="spot-override"]')?.textContent).toBe(SPX_POINTS)
    expect(container.querySelector('[data-unit="range"]')?.textContent).toBe(SPX_POINTS)
  })

  it('says the axis is SPX points only for a selection holding XSP legs', () => {
    renderControls(false)
    expect(screen.queryByTestId('xsp-axis-note')).not.toBeInTheDocument()
  })

  it('shows the XSP note when the drawn position holds XSP legs', () => {
    renderControls(true)
    expect(screen.getByTestId('xsp-axis-note')).toHaveTextContent(XSP_AXIS_NOTE)
  })
})
