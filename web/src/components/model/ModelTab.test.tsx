/**
 * The Model tab against the mock API, on the demo book's valuation day
 * (2026-08-31): the October strangle and the November condor are 46 and 81
 * days out, as `serve --demo` has them. Only `Date` is faked — TanStack Query
 * and user-event keep their real timers.
 */

import { screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { ApiError, call } from '../../api/client'
import { mockRequest } from '../../api/mock'
import type { TradesResponse } from '../../api/types'
import { MINUS } from '../../lib/model'
import { MAX_MODEL_SECTIONS, useAppStore } from '../../store/appStore'
import { renderWithClient, resetAppStore } from '../../test/utils'
import { ModelTab } from './ModelTab'

vi.mock('../../api/client', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../api/client')>()
  return { ...actual, call: vi.fn(actual.call) }
})

const OCT_TITLE = 'SPX · 16 Oct 2026 · AM · 46 DTE'
const NOV_TITLE = 'SPX · 20 Nov 2026 · AM · 81 DTE'
const STRANGLE = 'oct-strangle-2026-08-24'

let trades: TradesResponse['trades'] = []

beforeEach(() => {
  vi.useFakeTimers({ toFake: ['Date'] })
  vi.setSystemTime(new Date(2026, 7, 31, 11, 0, 0))
  resetAppStore()
  trades = (mockRequest('/trades') as TradesResponse).trades
})

afterEach(() => {
  vi.useRealTimers()
  vi.mocked(call).mockClear()
})

/** A section card, found by its title. */
async function section(title: string): Promise<HTMLElement> {
  const heading = await screen.findByRole('heading', { name: title })
  return heading.closest('section') as HTMLElement
}

/** The Pos cell of one side of a strike inside a section. */
function posCell(card: HTMLElement, strike: number, side: 'call' | 'put'): HTMLElement {
  const row = card.querySelector<HTMLElement>(`tr[data-strike="${strike}"]`)
  if (!row) throw new Error(`no ${strike} row`)
  const pos = Array.from(row.querySelectorAll<HTMLElement>('td[data-col="pos"]'))
  return (side === 'call' ? pos[0] : pos[1]) as HTMLElement
}

describe('ModelTab', () => {
  it('lists the expirations, AM and PM apart on a third Friday', async () => {
    useAppStore.getState().setSelected([STRANGLE])
    renderWithClient(<ModelTab trades={trades} />)
    const strip = await screen.findByRole('toolbar', { name: 'Expirations' })
    const chips = within(strip).getAllByRole('button')
    expect(chips.length).toBeGreaterThan(40)
    const oct = within(strip).getAllByRole('button', { name: /^16 OCT 26 · 46d/ })
    expect(oct.map((chip) => chip.getAttribute('aria-label'))).toEqual([
      '16 OCT 26 · 46d · AM, SPX, monthly, position held',
      '16 OCT 26 · 46d · PM, SPXW, weekly',
    ])
    // The held monthly is open on its own: the selection only holds October.
    expect(oct[0]).toHaveAttribute('aria-pressed', 'true')
    expect(oct[1]).toHaveAttribute('aria-pressed', 'false')
  })

  it('opens every expiration the position holds and counts it in Pos', async () => {
    renderWithClient(<ModelTab trades={trades} />)
    expect(screen.getByText('all 4 open trades')).toBeInTheDocument()
    const oct = await section(OCT_TITLE)
    await section(NOV_TITLE)
    await waitFor(() => expect(posCell(oct, 7200, 'put')).toHaveTextContent(`${MINUS}1`))
    // Off the strike window around spot, but held — so never hidden.
    expect(posCell(oct, 8200, 'call')).toHaveTextContent(`${MINUS}1`)
    expect(posCell(oct, 7200, 'call')).toHaveTextContent('')
  })

  it('counts only the selected trades when there is a selection', async () => {
    useAppStore.getState().setSelected(['nov-condor-2026-08-26'])
    renderWithClient(<ModelTab trades={trades} />)
    expect(screen.getByText('1 selected trade')).toBeInTheDocument()
    const nov = await section(NOV_TITLE)
    await waitFor(() => expect(posCell(nov, 7000, 'put')).toHaveTextContent(`${MINUS}2`))
    expect(screen.queryByRole('heading', { name: OCT_TITLE })).not.toBeInTheDocument()
  })

  it('models a leg: the store, the ticket and the toolbar all follow', async () => {
    const user = userEvent.setup()
    useAppStore.getState().setSelected([STRANGLE])
    renderWithClient(<ModelTab trades={trades} />)
    await section(OCT_TITLE)
    const cell = await screen.findByRole('spinbutton', { name: 'Model quantity, SPX 16 Oct 26 7700 put' })
    await user.click(cell)
    await user.keyboard('-1{Enter}')

    expect(useAppStore.getState().model.legs).toHaveLength(1)
    const ticket = screen.getByRole('region', { name: 'Model ticket' })
    expect(within(ticket).getByText('SPX 16 OCT 26 7700 P')).toBeInTheDocument()
    expect(within(ticket).getByText(/^@ \d+\.\d\d$/)).toBeInTheDocument()
    expect(within(ticket).getByText(/^net -\$[\d,]+ credit$/)).toBeInTheDocument()
    expect(screen.getByText(/· 1 leg · net -\$[\d,]+ credit/)).toBeInTheDocument()
    // The model's own greeks, from `portfolio` with no trades.
    await waitFor(() => expect(within(ticket).queryByText('…')).not.toBeInTheDocument())
    expect(
      vi
        .mocked(call)
        .mock.calls.some(
          ([command, args]) =>
            command === 'portfolio' &&
            (args as { request: { ids: string[]; model_legs: unknown[] } }).request.ids.length === 0 &&
            (args as { request: { model_legs: unknown[] } }).request.model_legs.length === 1,
        ),
    ).toBe(true)

    await user.click(within(ticket).getByRole('button', { name: 'Remove SPX 16 OCT 26 7700 P from the model' }))
    expect(useAppStore.getState().model.legs).toEqual([])
  })

  // The ticket's strip is reserved before the first leg exists (on a narrow
  // window as in the rail), so the first quantity typed does not push the
  // chain down under the cursor.
  it('shows the ticket before there is anything on it, at a fixed floor', async () => {
    renderWithClient(<ModelTab trades={trades} />)
    await screen.findByRole('toolbar', { name: 'Expirations' })
    const ticket = screen.getByRole('region', { name: 'Model ticket' })
    expect(within(ticket).getByText(/No modeled legs/)).toBeInTheDocument()
    const strip = ticket.parentElement as HTMLElement
    expect(strip).not.toHaveClass('hidden')
    expect(strip).toHaveClass('flex', 'min-h-[6rem]')
  })

  it('clears the model, and cannot clear an empty one', async () => {
    const user = userEvent.setup()
    useAppStore.getState().setSelected([STRANGLE])
    renderWithClient(<ModelTab trades={trades} />)
    const clear = screen.getByRole('button', { name: 'Clear modeled trades' })
    expect(clear).toBeDisabled()
    const cell = await screen.findByRole('spinbutton', { name: 'Model quantity, SPX 16 Oct 26 7800 call' })
    await user.click(cell)
    await user.keyboard('2{Enter}')
    expect(clear).toBeEnabled()
    await user.click(clear)
    expect(useAppStore.getState().model.legs).toEqual([])
    expect(cell).toHaveValue('')
    // The layout survives a clear.
    expect(useAppStore.getState().model.strikeCount).toBe(32)
  })

  it('materialises the automatic sections on the first chip click', async () => {
    const user = userEvent.setup()
    useAppStore.getState().setSelected([STRANGLE])
    renderWithClient(<ModelTab trades={trades} />)
    const strip = await screen.findByRole('toolbar', { name: 'Expirations' })
    await user.click(within(strip).getByRole('button', { name: /^20 NOV 26 · 81d · AM/ }))
    expect(useAppStore.getState().model.expiries).toEqual(['2026-10-16|SPX', '2026-11-20|SPX'])
    await section(NOV_TITLE)
    await section(OCT_TITLE)
  })

  it(`keeps at most ${MAX_MODEL_SECTIONS} sections, saying which one made room`, async () => {
    const user = userEvent.setup()
    useAppStore.getState().setModelExpiries([
      '2026-09-01|SPXW',
      '2026-09-02|SPXW',
      '2026-09-03|SPXW',
      '2026-09-04|SPXW',
      '2026-09-08|SPXW',
      '2026-09-09|SPXW',
    ])
    renderWithClient(<ModelTab trades={trades} />)
    const strip = await screen.findByRole('toolbar', { name: 'Expirations' })
    await user.click(within(strip).getByRole('button', { name: /^16 OCT 26 · 46d · AM/ }))
    expect(useAppStore.getState().model.expiries).toHaveLength(MAX_MODEL_SECTIONS)
    expect(useAppStore.getState().model.expiries).not.toContain('2026-09-01|SPXW')
    expect(screen.getByRole('status')).toHaveTextContent(
      `At most ${MAX_MODEL_SECTIONS} expirations at once — 1 SEP 26 SPXW made room.`,
    )
  })

  it('keeps the last section open', async () => {
    const user = userEvent.setup()
    useAppStore.getState().setSelected([STRANGLE])
    renderWithClient(<ModelTab trades={trades} />)
    const card = await section(OCT_TITLE)
    await user.click(within(card).getByRole('button', { name: `Hide ${OCT_TITLE}` }))
    expect(screen.getByRole('heading', { name: OCT_TITLE })).toBeInTheDocument()
    expect(screen.getByRole('status')).toHaveTextContent('At least one expiration stays open')
  })

  it('sets the strike window from the toolbar', async () => {
    const user = userEvent.setup()
    useAppStore.getState().setSelected([STRANGLE])
    renderWithClient(<ModelTab trades={trades} />)
    const group = screen.getByRole('group', { name: 'Strikes around spot' })
    await user.click(within(group).getByRole('button', { name: '64' }))
    expect(useAppStore.getState().model.strikeCount).toBe(64)
    await user.click(within(group).getByRole('button', { name: 'All' }))
    expect(useAppStore.getState().model.strikeCount).toBeNull()
    expect(within(group).getByRole('button', { name: 'All' })).toHaveAttribute('aria-pressed', 'true')
  })

  it('goes back to Positions from the toolbar', async () => {
    const user = userEvent.setup()
    useAppStore.getState().setTab('model')
    renderWithClient(<ModelTab trades={trades} />)
    await user.click(screen.getByRole('button', { name: 'Show in Positions' }))
    expect(useAppStore.getState().tab).toBe('positions')
  })

  it('explains a chain no source can list, and where to fix it', async () => {
    const actual = await vi.importActual<typeof import('../../api/client')>('../../api/client')
    vi.mocked(call).mockImplementation((command, args, signal) =>
      command === 'chain_expirations'
        ? Promise.reject(
            new ApiError('cache: cannot list option chains', 503, {
              type: 'https://wingspan.invalid/problems/chain-unavailable',
              status: 503,
              detail: 'cache: cannot list option chains',
            }),
          )
        : actual.call(command, args, signal),
    )
    try {
      renderWithClient(<ModelTab trades={trades} />)
      expect(await screen.findByText('cache: cannot list option chains')).toBeInTheDocument()
      expect(screen.getByText(/source cannot list an option chain/)).toBeInTheDocument()
      await userEvent.click(screen.getByRole('button', { name: 'Data health → Settings' }))
      expect(useAppStore.getState().tab).toBe('health')
    } finally {
      vi.mocked(call).mockImplementation(actual.call)
    }
  })

  it('keeps a failing chain inside its own card', async () => {
    const actual = await vi.importActual<typeof import('../../api/client')>('../../api/client')
    vi.mocked(call).mockImplementation((command, args, signal) => {
      const request = (args as { request?: { expiry?: string } } | undefined)?.request
      return command === 'option_chain' && request?.expiry === '2026-11-20'
        ? Promise.reject(new Error('provider timed out'))
        : actual.call(command, args, signal)
    })
    try {
      renderWithClient(<ModelTab trades={trades} />)
      const nov = await section(NOV_TITLE)
      expect(await within(nov).findByText('provider timed out')).toBeInTheDocument()
      const oct = await section(OCT_TITLE)
      await waitFor(() => expect(posCell(oct, 7200, 'put')).toHaveTextContent(`${MINUS}1`))
      expect(within(oct).queryByText('provider timed out')).not.toBeInTheDocument()
    } finally {
      vi.mocked(call).mockImplementation(actual.call)
    }
  })
})
