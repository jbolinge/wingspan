/**
 * The Model tab against the mock API, on the demo book's valuation day
 * (2026-08-31): the October strangle and the November condor are 46 and 81
 * days out, as `serve --demo` has them. Only `Date` is faked — TanStack Query
 * and user-event keep their real timers.
 */

import { act, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { ApiError, call } from '../../api/client'
import { mockRequest } from '../../api/mock'
import type { TradesResponse } from '../../api/types'
import { MINUS } from '../../lib/model'
import { AUTO_EXPIRY, useAppStore } from '../../store/appStore'
import { renderWithClient, resetAppStore } from '../../test/utils'
import { ModelTab } from './ModelTab'

// The graph view mounts the real RiskGraphPanel; only ECharts, which needs a
// canvas, is stood in for — by a node carrying the chart's aria-label, which
// says what is drawn.
vi.mock('../graph/RiskChart', () => ({
  RiskChart: ({ ariaLabel }: { ariaLabel: string }) => <div data-testid="risk-chart" aria-label={ariaLabel} />,
}))

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
    // The held monthly is open on its own: the first expiration the position holds.
    expect(oct[0]).toHaveAttribute('aria-pressed', 'true')
    expect(oct[1]).toHaveAttribute('aria-pressed', 'false')
  })

  it('opens the first expiration the position holds, and only that, counting it in Pos', async () => {
    renderWithClient(<ModelTab trades={trades} />)
    expect(screen.getByText('all 4 open trades')).toBeInTheDocument()
    const oct = await section(OCT_TITLE)
    expect(screen.queryByRole('heading', { name: NOV_TITLE })).not.toBeInTheDocument()
    await waitFor(() => expect(posCell(oct, 7200, 'put')).toHaveTextContent(`${MINUS}1`))
    // Off the strike window around spot, but held — so never hidden.
    expect(posCell(oct, 8200, 'call')).toHaveTextContent(`${MINUS}1`)
    expect(posCell(oct, 7200, 'call')).toHaveTextContent('')
  })

  // Pos still counts the whole book with nothing selected, but the graph
  // then draws the model by itself — the toolbar keeps the two apart.
  it('says the graph shows the model alone while nothing is selected', async () => {
    const user = userEvent.setup()
    renderWithClient(<ModelTab trades={trades} />)
    const note = /the graph shows the model alone/
    expect(screen.queryByText(note)).not.toBeInTheDocument()
    const cell = await screen.findByRole('spinbutton', { name: 'Model quantity, SPX 16 Oct 26 7700 put' })
    await user.click(cell)
    await user.keyboard('-1{Enter}')
    expect(screen.getByText('all 4 open trades')).toBeInTheDocument()
    expect(screen.getByText(note)).toBeInTheDocument()

    // Hidden from the graph, the model is not what the graph shows.
    act(() => useAppStore.getState().setModelInclude(false))
    expect(screen.queryByText(note)).not.toBeInTheDocument()
    act(() => useAppStore.getState().setModelInclude(true))
    // And a selection puts trades under it.
    act(() => useAppStore.getState().setSelected([STRANGLE]))
    expect(screen.getByText('1 selected trade')).toBeInTheDocument()
    expect(screen.queryByText(note)).not.toBeInTheDocument()
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

  // The ticket prices what the graph draws: the ticked trades (never the
  // whole book) and, once something is modeled, the model on top.
  it('gives the ticket the selected trades and a stale id never reaches it', async () => {
    const user = userEvent.setup()
    useAppStore.getState().setSelected([STRANGLE, 'gone-2020-01-01'])
    renderWithClient(<ModelTab trades={trades} />)
    const ticket = await screen.findByRole('region', { name: 'Model ticket' })
    // Trades selected, nothing modeled: the Position row alone.
    const table = await within(ticket).findByRole('table', { name: 'Greeks' })
    expect(within(table).getAllByRole('rowheader').map((h) => h.textContent)).toEqual(['Position'])
    await screen.findByRole('heading', { name: OCT_TITLE })
    await user.click(await screen.findByRole('spinbutton', { name: 'Model quantity, SPX 16 Oct 26 7700 put' }))
    await user.keyboard('-1{Enter}')
    await waitFor(() =>
      expect(within(table).getAllByRole('rowheader').map((h) => h.textContent)).toEqual([
        'Model',
        'Position',
        'Position + model',
      ]),
    )
    const sent = vi
      .mocked(call)
      .mock.calls.filter(([command]) => command === 'portfolio')
      .flatMap(([, args]) => (args as { request: { ids: string[] } }).request.ids)
    expect(sent).not.toContain('gone-2020-01-01')
    expect(sent).toContain(STRANGLE)
  })

  it('shows only the Model row with nothing selected', async () => {
    const user = userEvent.setup()
    renderWithClient(<ModelTab trades={trades} />)
    await user.click(await screen.findByRole('spinbutton', { name: 'Model quantity, SPX 16 Oct 26 7700 put' }))
    await user.keyboard('-1{Enter}')
    const table = await screen.findByRole('table', { name: 'Greeks' })
    expect(within(table).getAllByRole('rowheader').map((h) => h.textContent)).toEqual(['Model'])
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

  it('replaces the chain on screen when another chip is picked', async () => {
    const user = userEvent.setup()
    useAppStore.getState().setSelected([STRANGLE])
    renderWithClient(<ModelTab trades={trades} />)
    const strip = await screen.findByRole('toolbar', { name: 'Expirations' })
    await section(OCT_TITLE)
    await user.click(within(strip).getByRole('button', { name: /^20 NOV 26 · 81d · AM/ }))
    expect(useAppStore.getState().model.expiry).toBe('2026-11-20|SPX')
    await section(NOV_TITLE)
    expect(screen.queryByRole('heading', { name: OCT_TITLE })).not.toBeInTheDocument()
    expect(within(strip).getAllByRole('button', { pressed: true })).toHaveLength(1)
  })

  it('closes the chain from its chip, and says so', async () => {
    const user = userEvent.setup()
    useAppStore.getState().setSelected([STRANGLE])
    renderWithClient(<ModelTab trades={trades} />)
    const strip = await screen.findByRole('toolbar', { name: 'Expirations' })
    await section(OCT_TITLE)
    await user.click(within(strip).getByRole('button', { name: /^16 OCT 26 · 46d · AM/ }))
    expect(useAppStore.getState().model.expiry).toBeNull()
    expect(screen.queryByRole('heading', { name: OCT_TITLE })).not.toBeInTheDocument()
    expect(screen.getByText('No chain open — pick an expiration above.')).toBeInTheDocument()
    expect(within(strip).queryAllByRole('button', { pressed: true })).toHaveLength(0)
    // No hint, no status: closing the only chain is simply allowed.
    expect(screen.queryByRole('status')).not.toBeInTheDocument()

    // The empty state is stable (the automatic pick does not come back) and
    // a chip reopens a chain.
    await user.click(within(strip).getByRole('button', { name: /^16 OCT 26 · 46d · AM/ }))
    await section(OCT_TITLE)
    expect(screen.queryByText(/No chain open/)).not.toBeInTheDocument()
  })

  it('closes the chain from its own hide button', async () => {
    const user = userEvent.setup()
    useAppStore.getState().setSelected([STRANGLE])
    renderWithClient(<ModelTab trades={trades} />)
    const card = await section(OCT_TITLE)
    await user.click(within(card).getByRole('button', { name: `Hide ${OCT_TITLE}` }))
    expect(useAppStore.getState().model.expiry).toBeNull()
    expect(screen.getByText(/No chain open/)).toBeInTheDocument()
  })

  it('falls back to the automatic pick when the stored expiration is no longer listed', async () => {
    useAppStore.getState().setSelected([STRANGLE])
    useAppStore.getState().setModelExpiry('2020-01-17|SPX')
    renderWithClient(<ModelTab trades={trades} />)
    await section(OCT_TITLE)
    expect(useAppStore.getState().model.expiry).not.toBe(AUTO_EXPIRY)
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

  /** A per-symbol provider quotes only the window around spot unless asked for
   *  everything: the on-screen chain asks for `all` exactly while All is
   *  pressed, and a chain fetched only for the ticket's marks never does. */
  it('asks the server for every strike only while All is pressed, and only for the chain on screen', async () => {
    const user = userEvent.setup()
    useAppStore.getState().setSelected([STRANGLE])
    useAppStore.getState().setModelExpiry('2026-10-16|SPX')
    const nov = {
      underlying: 'SPX',
      root: 'SPX',
      expiry: '2026-11-20',
      settlement: 'am',
      strike: 7000,
      right: 'put',
      multiplier: 100,
      osi: 'SPX   261120P07000000',
      tos_code: 'SPX261120P7000',
    } as const
    useAppStore.getState().setModelQty(nov, -1)
    renderWithClient(<ModelTab trades={trades} />)
    await section(OCT_TITLE)
    const scopes = () =>
      vi
        .mocked(call)
        .mock.calls.filter(([command]) => command === 'option_chain')
        .map(([, args]) => {
          const { expiry, scope } = (args as { request: { expiry: string; scope?: string } }).request
          return `${expiry}:${String(scope)}`
        })
    await waitFor(() => expect(scopes()).toContain('2026-11-20:near'))
    expect(scopes()).toContain('2026-10-16:near')
    expect(scopes().some((s) => s.endsWith(':all'))).toBe(false)

    const group = screen.getByRole('group', { name: 'Strikes around spot' })
    await user.click(within(group).getByRole('button', { name: 'All' }))
    await waitFor(() => expect(scopes()).toContain('2026-10-16:all'))
    expect(scopes()).not.toContain('2026-11-20:all')

    // Back to a band: the window is enough again.
    vi.mocked(call).mockClear()
    await user.click(within(group).getByRole('button', { name: '16' }))
    await waitFor(() => expect(scopes()).toContain('2026-10-16:near'))
    expect(scopes().some((s) => s.endsWith(':all'))).toBe(false)
  })

  it('still jumps to Positions from "Position from", where trades are selected', async () => {
    const user = userEvent.setup()
    useAppStore.getState().setTab('model')
    renderWithClient(<ModelTab trades={trades} />)
    await user.click(screen.getByRole('button', { name: 'all 4 open trades' }))
    expect(useAppStore.getState().tab).toBe('positions')
  })

  describe('the Chain / Risk graph switch', () => {
    const viewGroup = () => screen.getByRole('group', { name: 'Model view' })

    it('leads the toolbar, on the chain to begin with', async () => {
      renderWithClient(<ModelTab trades={trades} />)
      await screen.findByRole('toolbar', { name: 'Expirations' })
      const group = viewGroup()
      expect(within(group).getByRole('button', { name: 'Chain' })).toHaveAttribute('aria-pressed', 'true')
      expect(within(group).getByRole('button', { name: 'Risk graph' })).toHaveAttribute('aria-pressed', 'false')
      // First in the toolbar: the tab's primary switch.
      const toolbar = group.parentElement as HTMLElement
      expect(toolbar.firstElementChild).toBe(group)
      expect(screen.queryByRole('button', { name: 'Show in Positions' })).not.toBeInTheDocument()
    })

    it('swaps the strip and the chain for the risk graph, keeping the ticket', async () => {
      const user = userEvent.setup()
      useAppStore.getState().setSelected([STRANGLE])
      renderWithClient(<ModelTab trades={trades} />)
      await section(OCT_TITLE)

      await user.click(within(viewGroup()).getByRole('button', { name: 'Risk graph' }))
      expect(useAppStore.getState().model.view).toBe('graph')
      expect(within(viewGroup()).getByRole('button', { name: 'Risk graph' })).toHaveAttribute('aria-pressed', 'true')
      expect(screen.getByRole('heading', { name: 'Risk graph' })).toBeInTheDocument()
      expect(screen.queryByRole('toolbar', { name: 'Expirations' })).not.toBeInTheDocument()
      expect(screen.queryByTestId('chain-scroll')).not.toBeInTheDocument()
      expect(screen.queryByRole('heading', { name: OCT_TITLE })).not.toBeInTheDocument()
      // The strike window shapes the chain alone.
      expect(screen.queryByRole('group', { name: 'Strikes around spot' })).not.toBeInTheDocument()
      // The ticket stays beside the graph.
      expect(screen.getByRole('region', { name: 'Model ticket' })).toBeInTheDocument()
      // The selected strangle, exactly as Positions would draw it.
      expect(await screen.findByTestId('risk-chart')).toHaveAttribute(
        'aria-label',
        expect.stringMatching(/^Risk graph for 1 selected trades:/),
      )

      await user.click(within(viewGroup()).getByRole('button', { name: 'Chain' }))
      expect(useAppStore.getState().model.view).toBe('chain')
      expect(screen.getByRole('toolbar', { name: 'Expirations' })).toBeInTheDocument()
      await section(OCT_TITLE)
      expect(screen.queryByRole('heading', { name: 'Risk graph' })).not.toBeInTheDocument()
    })

    it('draws the model alone when nothing is selected', async () => {
      const user = userEvent.setup()
      renderWithClient(<ModelTab trades={trades} />)
      await user.click(await screen.findByRole('spinbutton', { name: 'Model quantity, SPX 16 Oct 26 7700 put' }))
      await user.keyboard('-1{Enter}')
      await user.click(within(viewGroup()).getByRole('button', { name: 'Risk graph' }))
      expect(await screen.findByTestId('risk-chart')).toHaveAttribute(
        'aria-label',
        expect.stringMatching(/^Risk graph for the model alone:/),
      )
      expect(screen.getByText('Model · 1 leg')).toBeInTheDocument()
    })

    it('says how to give the graph something to draw, from this tab', async () => {
      useAppStore.getState().setTab('model')
      useAppStore.getState().setModelView('graph')
      renderWithClient(<ModelTab trades={trades} />)
      expect(screen.getByText(/Type a quantity into a Model cell on the Chain/)).toBeInTheDocument()
      // The Positions wording points at a table that is not on this tab.
      expect(screen.queryByText(/Select trades in the table/)).not.toBeInTheDocument()
    })

    it('goes back to the chain from the Model pill\'s edit', async () => {
      const user = userEvent.setup()
      renderWithClient(<ModelTab trades={trades} />)
      await user.click(await screen.findByRole('spinbutton', { name: 'Model quantity, SPX 16 Oct 26 7700 put' }))
      await user.keyboard('-1{Enter}')
      await user.click(within(viewGroup()).getByRole('button', { name: 'Risk graph' }))
      await user.click(await screen.findByRole('button', { name: 'edit' }))
      expect(useAppStore.getState().model.view).toBe('chain')
      expect(await screen.findByRole('toolbar', { name: 'Expirations' })).toBeInTheDocument()
    })

    // Editing means leaving the graph: a maximize must not outlive it, or
    // the next "Risk graph" click would land on a toolbar-less tab again.
    it('drops the maximize on the pill\'s edit', async () => {
      const user = userEvent.setup()
      renderWithClient(<ModelTab trades={trades} />)
      await user.click(await screen.findByRole('spinbutton', { name: 'Model quantity, SPX 16 Oct 26 7700 put' }))
      await user.keyboard('-1{Enter}')
      await user.click(within(viewGroup()).getByRole('button', { name: 'Risk graph' }))
      await user.click(await screen.findByRole('button', { name: /maximize/ }))
      expect(screen.queryByRole('group', { name: 'Model view' })).not.toBeInTheDocument()
      await user.click(screen.getByRole('button', { name: 'edit' }))
      expect(useAppStore.getState().graphMaximized).toBe(false)
      await user.click(within(viewGroup()).getByRole('button', { name: 'Risk graph' }))
      expect(screen.getByRole('region', { name: 'Model ticket' })).toBeInTheDocument()
    })

    // The open section is not on screen in graph view: observing it would
    // refetch it on every snapshot for nothing. The legs' chains, which
    // price the ticket, stay.
    it('fetches no chain for the hidden section, only the legs\' expiries', async () => {
      const user = userEvent.setup()
      const nov = {
        underlying: 'SPX',
        root: 'SPX',
        expiry: '2026-11-20',
        settlement: 'am',
        strike: 7000,
        right: 'put',
        multiplier: 100,
        osi: 'SPX   261120P07000000',
        tos_code: 'SPX261120P7000',
      } as const
      useAppStore.getState().setModelQty(nov, -1)
      useAppStore.getState().setModelExpiry('2026-10-16|SPX')
      useAppStore.getState().setModelView('graph')
      renderWithClient(<ModelTab trades={trades} />)
      await screen.findByTestId('risk-chart')
      const chains = () =>
        vi
          .mocked(call)
          .mock.calls.filter(([command]) => command === 'option_chain')
          .map(([, args]) => (args as { request: { expiry: string } }).request.expiry)
      await waitFor(() => expect(chains()).toContain('2026-11-20'))
      expect(chains()).not.toContain('2026-10-16')

      await user.click(within(viewGroup()).getByRole('button', { name: 'Chain' }))
      await section(OCT_TITLE)
      expect(chains()).toContain('2026-10-16')
    })

    it('gives a maximized graph the whole tab, and only in graph view', async () => {
      const user = userEvent.setup()
      useAppStore.getState().setTab('model')
      useAppStore.getState().setSelected([STRANGLE])
      useAppStore.getState().setModelView('graph')
      renderWithClient(<ModelTab trades={trades} />)
      await screen.findByTestId('risk-chart')
      await user.click(screen.getByRole('button', { name: /maximize/ }))
      expect(useAppStore.getState().graphMaximized).toBe(true)
      expect(screen.getByRole('heading', { name: 'Risk graph' })).toBeInTheDocument()
      expect(screen.queryByRole('group', { name: 'Model view' })).not.toBeInTheDocument()
      expect(screen.queryByRole('region', { name: 'Model ticket' })).not.toBeInTheDocument()
      const restore = screen.getByRole('button', { name: /^\W*restore$/ })
      expect(restore).toHaveAttribute('title', 'Bring the toolbar and the model ticket back (Esc)')

      await user.click(restore)
      expect(screen.getByRole('group', { name: 'Model view' })).toBeInTheDocument()
      expect(screen.getByRole('region', { name: 'Model ticket' })).toBeInTheDocument()

      // Maximized from Positions, the chain view is still the chain.
      act(() => {
        useAppStore.getState().setGraphMaximized(true)
        useAppStore.getState().setModelView('chain')
      })
      expect(screen.getByRole('group', { name: 'Model view' })).toBeInTheDocument()
      expect(await screen.findByRole('toolbar', { name: 'Expirations' })).toBeInTheDocument()

      // And the switch to the graph opens it beside its toolbar and ticket,
      // not maximized by a flag left over from another tab.
      await user.click(within(viewGroup()).getByRole('button', { name: 'Risk graph' }))
      expect(useAppStore.getState().graphMaximized).toBe(false)
      expect(screen.getByRole('group', { name: 'Model view' })).toBeInTheDocument()
      expect(screen.getByRole('region', { name: 'Model ticket' })).toBeInTheDocument()
    })
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
      const oct = await section(OCT_TITLE)
      await waitFor(() => expect(posCell(oct, 7200, 'put')).toHaveTextContent(`${MINUS}1`))
      expect(within(oct).queryByText('provider timed out')).not.toBeInTheDocument()
      // November's chain fails inside its own card, not the tab.
      act(() => useAppStore.getState().setModelExpiry('2026-11-20|SPX'))
      const nov = await section(NOV_TITLE)
      expect(await within(nov).findByText('provider timed out')).toBeInTheDocument()
      expect(screen.getByRole('toolbar', { name: 'Expirations' })).toBeInTheDocument()
    } finally {
      vi.mocked(call).mockImplementation(actual.call)
    }
  })

  /**
   * The chain opens on the money. jsdom has no layout, so the geometry is
   * stubbed: every spot row sits 800px below the top of the scroll
   * container's content and the container shows 400px, which puts the
   * centred `scrollTop` at 800 − (400 − 20) / 2 = 610.
   */
  describe('centring on spot', () => {
    const CENTRED = 610

    const spies: { mockRestore: () => void }[] = []

    beforeEach(() => {
      const rects = vi.spyOn(Element.prototype, 'getBoundingClientRect').mockImplementation(function (this: Element) {
        const id = this instanceof HTMLElement ? this.dataset.testid : undefined
        const scroll = this.closest<HTMLElement>('[data-testid="chain-scroll"]')
        // Content coordinates move up as the container scrolls, as in a
        // browser: the row's viewport top is its content top minus scrollTop.
        const top =
          id === 'chain-scroll' ? 100 : id === 'spot-row' ? 100 + 800 - (scroll?.scrollTop ?? 0) : 0
        const height = id === 'chain-scroll' ? 400 : id === 'spot-row' ? 20 : 0
        return { top, bottom: top + height, height, left: 0, right: 0, width: 0, x: 0, y: top } as DOMRect
      })
      const heights = vi
        .spyOn(HTMLElement.prototype, 'clientHeight', 'get')
        .mockImplementation(function (this: HTMLElement) {
          return this.dataset.testid === 'chain-scroll' ? 400 : 0
        })
      spies.push(rects, heights)
    })
    // Only these two: the module-wide `call` mock must keep its implementation.
    afterEach(() => spies.splice(0).forEach((spy) => spy.mockRestore()))

    const chainFetches = (): number =>
      vi.mocked(call).mock.calls.filter(([command]) => command === 'option_chain').length

    it('centres the section once, not on a refetch or an edit', async () => {
      const user = userEvent.setup()
      useAppStore.getState().setSelected([STRANGLE])
      renderWithClient(<ModelTab trades={trades} />)
      const scroll = await screen.findByTestId('chain-scroll')
      await waitFor(() => expect(scroll.scrollTop).toBe(CENTRED))

      // The trader scrolls away; a refetch brings new data for the same
      // section, which must not pull them back.
      scroll.scrollTop = 0
      const before = chainFetches()
      await user.click(screen.getByRole('button', { name: '↻ refresh' }))
      await waitFor(() => expect(chainFetches()).toBeGreaterThan(before))
      await waitFor(() => expect(screen.queryByRole('status')).not.toBeInTheDocument())
      expect(scroll.scrollTop).toBe(0)

      // Nor does a Model-cell edit.
      const cell = screen.getByRole('spinbutton', { name: 'Model quantity, SPX 16 Oct 26 7700 put' })
      await user.click(cell)
      await user.keyboard('-1{Enter}')
      expect(useAppStore.getState().model.legs).toHaveLength(1)
      expect(scroll.scrollTop).toBe(0)

      // "⌖ spot" brings it back on demand.
      const oct = await section(OCT_TITLE)
      await user.click(within(oct).getByRole('button', { name: `Centre ${OCT_TITLE} on spot` }))
      expect(scroll.scrollTop).toBe(CENTRED)
    })

    it('centres again when a different section opens', async () => {
      const user = userEvent.setup()
      useAppStore.getState().setModelExpiry('2026-11-20|SPX')
      renderWithClient(<ModelTab trades={trades} />)
      const scroll = await screen.findByTestId('chain-scroll')
      await section(NOV_TITLE)
      await waitFor(() => expect(scroll.scrollTop).toBe(CENTRED))
      scroll.scrollTop = 0

      // October replaces November: a new section, centred once its chain has
      // loaded.
      const strip = screen.getByRole('toolbar', { name: 'Expirations' })
      await user.click(within(strip).getByRole('button', { name: /^16 OCT 26 · 46d · AM/ }))
      await section(OCT_TITLE)
      await waitFor(() => expect(scroll.scrollTop).toBe(CENTRED))
    })

    it('centres again when the chain comes back from the graph', async () => {
      const user = userEvent.setup()
      useAppStore.getState().setSelected([STRANGLE])
      renderWithClient(<ModelTab trades={trades} />)
      const scroll = await screen.findByTestId('chain-scroll')
      await section(OCT_TITLE)
      await waitFor(() => expect(scroll.scrollTop).toBe(CENTRED))

      const group = screen.getByRole('group', { name: 'Model view' })
      await user.click(within(group).getByRole('button', { name: 'Risk graph' }))
      await user.click(within(group).getByRole('button', { name: 'Chain' }))
      // A new scroll container, so a fresh scrollTop: centred again, not 0.
      const back = await screen.findByTestId('chain-scroll')
      expect(back).not.toBe(scroll)
      await waitFor(() => expect(back.scrollTop).toBe(CENTRED))
    })

    it('centres again when the same chain is closed and reopened', async () => {
      const user = userEvent.setup()
      useAppStore.getState().setSelected([STRANGLE])
      renderWithClient(<ModelTab trades={trades} />)
      const scroll = await screen.findByTestId('chain-scroll')
      await section(OCT_TITLE)
      await waitFor(() => expect(scroll.scrollTop).toBe(CENTRED))

      const strip = screen.getByRole('toolbar', { name: 'Expirations' })
      const chip = within(strip).getByRole('button', { name: /^16 OCT 26 · 46d · AM/ })
      await user.click(chip)
      scroll.scrollTop = 0
      await user.click(chip)
      await section(OCT_TITLE)
      await waitFor(() => expect(scroll.scrollTop).toBe(CENTRED))
    })
  })
})
