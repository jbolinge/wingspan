import { screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { useEffect } from 'react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { call } from '../../api/client'
import { useSetModelContracts } from '../../api/queries'
import type { OptionContract } from '../../api/types'
import { MINUS } from '../../lib/model'
import { useAppStore, type ModelLeg } from '../../store/appStore'
import { renderWithClient, resetAppStore } from '../../test/utils'
import { ModelTicket } from './ModelTicket'

vi.mock('../../api/client', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../api/client')>()
  return { ...actual, call: vi.fn(actual.call) }
})

// The ticket asks for greeks only on legs live on the server's date, read off
// the (mock) snapshot's clock: pin it to the demo's valuation day, so the
// October legs below stay live whatever day the suite runs on.
beforeEach(() => {
  vi.useFakeTimers({ toFake: ['Date'] })
  vi.setSystemTime(new Date('2026-08-31T16:00:00Z'))
  resetAppStore()
})
afterEach(() => {
  vi.useRealTimers()
  vi.mocked(call).mockClear()
})

function contract(strike: number, right: 'call' | 'put'): OptionContract {
  const cp = right === 'call' ? 'C' : 'P'
  return {
    underlying: 'SPX',
    root: 'SPX',
    expiry: '2026-10-16',
    settlement: 'am',
    strike,
    right,
    multiplier: 100,
    osi: `SPX   261016${cp}0${strike}000`,
    tos_code: `SPX261016${cp}${strike}`,
  }
}

const LEGS: ModelLeg[] = [
  { contract: contract(7500, 'put'), qty: -1 },
  { contract: contract(7400, 'put'), qty: 1 },
]

const portfolioCalls = () => vi.mocked(call).mock.calls.filter(([command]) => command === 'portfolio')

describe('ModelTicket', () => {
  it('says how to start when there is nothing modeled', () => {
    renderWithClient(<ModelTicket legs={[]} marks={new Map()} include selectedIds={[]} />)
    expect(screen.getByText(/No modeled legs/)).toBeInTheDocument()
    expect(portfolioCalls()).toHaveLength(0)
  })

  // The rail is narrow: the contract label is the part that truncates, so
  // it carries its full text as a tooltip, and the remove button never
  // shrinks out of reach.
  it('lets a long label truncate without losing it or the remove button', () => {
    renderWithClient(<ModelTicket legs={LEGS} marks={new Map()} include selectedIds={[]} />)
    const label = screen.getByTitle('SPX 16 OCT 26 7500 P')
    expect(label).toHaveClass('truncate', 'min-w-0')
    const remove = screen.getByRole('button', { name: 'Remove SPX 16 OCT 26 7500 P from the model' })
    expect(remove).toHaveClass('shrink-0')
  })

  it('lists each leg as an order line and nets the premium', async () => {
    const marks = new Map<string, number | null>([
      ['SPX261016P7500', 42.5],
      ['SPX261016P7400', 30.25],
    ])
    renderWithClient(<ModelTicket legs={LEGS} marks={marks} include selectedIds={[]} />)
    const ticket = screen.getByRole('region', { name: 'Model ticket' })
    const lines = within(ticket).getAllByRole('listitem')
    expect(lines.map((line) => line.textContent)).toEqual([
      `${MINUS}1×SPX 16 OCT 26 7500 P@ 42.50✕`,
      '+1×SPX 16 OCT 26 7400 P@ 30.25✕',
    ])
    // Sold 42.50, bought 30.25: a $1,225 credit.
    expect(within(ticket).getByText('net -$1,225 credit')).toBeInTheDocument()
    await waitFor(() => expect(within(ticket).queryByText('…')).not.toBeInTheDocument())
    const [, args] = portfolioCalls().at(-1)!
    expect((args as { request: unknown }).request).toMatchObject({ ids: [], model_legs: [{ qty: 1 }, { qty: -1 }] })
  })

  it('shows a dash rather than a partial total while a mark is missing', () => {
    renderWithClient(<ModelTicket legs={LEGS} marks={new Map([['SPX261016P7500', 42.5]])} include selectedIds={[]} />)
    expect(screen.getByText('@ —')).toBeInTheDocument()
    expect(screen.getByText('net —')).toBeInTheDocument()
  })

  // A leg that expired before the server's date would get the whole request
  // refused; the ticket asks only about the live ones.
  it('leaves an expired leg out of the greeks request', async () => {
    const expired: ModelLeg = {
      contract: { ...contract(7600, 'put'), expiry: '2026-08-28', tos_code: 'SPX260828P7600' },
      qty: 1,
    }
    renderWithClient(<ModelTicket legs={[...LEGS, expired]} marks={new Map()} include selectedIds={[]} />)
    await waitFor(() => expect(portfolioCalls()).not.toHaveLength(0))
    const [, args] = portfolioCalls().at(-1)!
    const sent = (args as { request: { model_legs: { contract: { expiry: string } }[] } }).request.model_legs
    expect(sent.map((leg) => leg.contract.expiry)).toEqual(['2026-10-16', '2026-10-16'])
  })

  it('computes no greeks for a model hidden from the graph', () => {
    renderWithClient(<ModelTicket legs={LEGS} marks={new Map()} include={false} selectedIds={[]} />)
    expect(screen.getByText(/Hidden from the risk graph/)).toBeInTheDocument()
    expect(portfolioCalls()).toHaveLength(0)
  })

  describe('greeks table', () => {
    const rows = () => within(screen.getByRole('table', { name: 'Greeks' })).getAllByRole('row').slice(1)
    const rowNames = () => rows().map((row) => within(row).getByRole('rowheader').textContent)
    const requests = () =>
      portfolioCalls().map(([, args]) => (args as { request: { ids: string[]; model_legs: unknown[] } }).request)

    it('shows the model alone with legs and nothing selected', async () => {
      renderWithClient(<ModelTicket legs={LEGS} marks={new Map()} include selectedIds={[]} />)
      await waitFor(() => expect(screen.queryByText('…')).not.toBeInTheDocument())
      expect(rowNames()).toEqual(['Model'])
      expect(requests().every((r) => r.ids.length === 0)).toBe(true)
      // Δ Γ Θ V W.V
      const table = screen.getByRole('table', { name: 'Greeks' })
      for (const name of ['Δ', 'Γ', 'Θ', 'V', 'W.V']) {
        expect(within(table).getAllByRole('columnheader', { name }).length).toBeGreaterThan(0)
      }
      expect(within(rows()[0]!).getAllByRole('cell')).toHaveLength(5)
    })

    // Columns are lined up by position: a header row one short shifts every
    // value under the wrong greek for a screen reader.
    it('has a header for every column, the row labels\' included', async () => {
      renderWithClient(<ModelTicket legs={LEGS} marks={new Map()} include selectedIds={['t1']} />)
      const table = screen.getByRole('table', { name: 'Greeks' })
      const [header, ...body] = within(table).getAllByRole('row')
      const headers = within(header!).getAllByRole('columnheader')
      expect(headers.map((h) => h.textContent)).toEqual(['Line', 'Δ', 'Γ', 'Θ', 'V', 'W.V'])
      for (const row of body) {
        expect(within(row).getAllByRole('rowheader').length + within(row).getAllByRole('cell').length).toBe(
          headers.length,
        )
      }
      await waitFor(() => expect(screen.queryByText('…')).not.toBeInTheDocument())
    })

    // The 18rem rail is short of width: five greek columns of a real book's
    // vega overlap at 50 px each, so there the table turns — a row per greek,
    // a column per line, labelled short with the full wording on hover.
    describe('in the rail', () => {
      beforeEach(() => {
        vi.spyOn(window, 'matchMedia').mockImplementation(
          (query: string) =>
            ({
              matches: query === '(width >= 1400px)',
              media: query,
              onchange: null,
              addEventListener: () => undefined,
              removeEventListener: () => undefined,
              addListener: () => undefined,
              removeListener: () => undefined,
              dispatchEvent: () => false,
            }) as MediaQueryList,
        )
      })

      it('puts a greek on each row and Model / Position / Total across', async () => {
        renderWithClient(<ModelTicket legs={LEGS} marks={new Map()} include selectedIds={['t1', 't2']} />)
        // The model's lines wait for the server's date (the first snapshot).
        const headerTexts = () =>
          within(screen.getByRole('table', { name: 'Greeks' }))
            .getAllByRole('columnheader')
            .map((h) => h.textContent)
        await waitFor(() => expect(headerTexts()).toEqual(['Greek', 'Model', 'Position', 'Total']))
        const table = screen.getByRole('table', { name: 'Greeks' })
        const [header, ...body] = within(table).getAllByRole('row')
        expect(within(header!).getByText('Total')).toHaveAttribute(
          'title',
          'Position + model — what the risk graph draws',
        )
        expect(body.map((row) => within(row).getByRole('rowheader').textContent)).toEqual([
          'Δ',
          'Γ',
          'Θ',
          'V',
          'W.V',
        ])
        for (const row of body) expect(within(row).getAllByRole('cell')).toHaveLength(3)
        await waitFor(() => expect(screen.queryByText('…')).not.toBeInTheDocument())
        // A value never spills into its neighbour: clipped, the number on hover.
        const cell = within(body[3]!).getAllByRole('cell')[0]!
        expect(cell).toHaveClass('truncate')
        expect(cell).toHaveAttribute('title', cell.textContent)
      })

      it('has one column per line it shows', () => {
        renderWithClient(<ModelTicket legs={[]} marks={new Map()} include selectedIds={['t1']} />)
        const table = screen.getByRole('table', { name: 'Greeks' })
        expect(within(table).getAllByRole('columnheader').map((h) => h.textContent)).toEqual(['Greek', 'Position'])
        expect(table.style.gridTemplateColumns).toBe('2rem repeat(1, minmax(0, 1fr))')
      })
    })

    it('shows Position, Model and Position + model with both', async () => {
      renderWithClient(<ModelTicket legs={LEGS} marks={new Map()} include selectedIds={['t1', 't2']} />)
      await waitFor(() => expect(screen.queryByText('…')).not.toBeInTheDocument())
      expect(rowNames()).toEqual(['Model', 'Position', 'Position + model'])
      expect(screen.getByTitle(/what the risk graph draws/)).toBeInTheDocument()
      const sent = requests()
      expect(sent).toContainEqual(expect.objectContaining({ ids: [], model_legs: [expect.anything(), expect.anything()] }))
      expect(sent).toContainEqual({ ids: ['t1', 't2'], model_legs: [] })
      expect(sent).toContainEqual(
        expect.objectContaining({ ids: ['t1', 't2'], model_legs: [expect.anything(), expect.anything()] }),
      )
    })

    it('shows only the Position row with trades selected and no legs', async () => {
      renderWithClient(<ModelTicket legs={[]} marks={new Map()} include selectedIds={['t1']} />)
      expect(screen.getByText(/No modeled legs/)).toBeInTheDocument()
      expect(rowNames()).toEqual(['Position'])
      expect(screen.queryByText('Net premium')).not.toBeInTheDocument()
      await waitFor(() => expect(requests()).toContainEqual({ ids: ['t1'], model_legs: [] }))
    })

    it('has no table with nothing to price', () => {
      renderWithClient(<ModelTicket legs={[]} marks={new Map()} include selectedIds={[]} />)
      expect(screen.queryByRole('table', { name: 'Greeks' })).not.toBeInTheDocument()
    })

    it('hides Model and Total, keeping Position, when the model is hidden', async () => {
      renderWithClient(<ModelTicket legs={LEGS} marks={new Map()} include={false} selectedIds={['t1']} />)
      expect(screen.getByText(/Hidden from the risk graph/)).toBeInTheDocument()
      expect(rowNames()).toEqual(['Position'])
      await waitFor(() => expect(requests()).toContainEqual({ ids: ['t1'], model_legs: [] }))
      expect(requests().every((r) => r.model_legs.length === 0)).toBe(true)
    })
  })

  describe('orders', () => {
    const orderCalls = () => vi.mocked(call).mock.calls.filter(([command]) => command === 'model_orders')
    const sentLegs = () =>
      (orderCalls().at(-1)![1] as { request: { model_legs: { contract: { strike: number; expiry: string } }[] } })
        .request.model_legs

    it('lists the live legs as TOS orders under the greeks', async () => {
      renderWithClient(<ModelTicket legs={LEGS} marks={new Map()} include selectedIds={[]} />)
      // Like the greeks, they wait for the server's date (the first snapshot).
      const orders = await screen.findByRole('group', { name: 'Orders' })
      await waitFor(() => expect(within(orders).getAllByRole('listitem')).toHaveLength(2))
      // After the greeks, inside the footer.
      const table = screen.getByRole('table', { name: 'Greeks' })
      expect(table.compareDocumentPosition(orders) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy()
      expect(orders.closest('footer')).not.toBeNull()
      expect(sentLegs().map((leg) => leg.contract.strike)).toEqual([7400, 7500])
    })

    // The orders are the ticket, not the graph: a model hidden from the
    // graph still has orders to copy.
    it('lists them with the model hidden from the graph too', async () => {
      renderWithClient(<ModelTicket legs={LEGS} marks={new Map()} include={false} selectedIds={[]} />)
      const orders = await screen.findByRole('group', { name: 'Orders' })
      await waitFor(() => expect(within(orders).getAllByRole('listitem')).toHaveLength(2))
      expect(portfolioCalls()).toHaveLength(0)
    })

    it('leaves an expired leg out of the orders request', async () => {
      const expired: ModelLeg = {
        contract: { ...contract(7600, 'put'), expiry: '2026-08-28', tos_code: 'SPX260828P7600' },
        qty: 1,
      }
      renderWithClient(<ModelTicket legs={[...LEGS, expired]} marks={new Map()} include selectedIds={[]} />)
      await waitFor(() => expect(orderCalls()).not.toHaveLength(0))
      expect(sentLegs().map((leg) => leg.contract.expiry)).toEqual(['2026-10-16', '2026-10-16'])
    })

    it('has no orders block with only trades selected', async () => {
      renderWithClient(<ModelTicket legs={[]} marks={new Map()} include selectedIds={['t1']} />)
      expect(screen.getByRole('table', { name: 'Greeks' })).toBeInTheDocument()
      expect(screen.queryByRole('group', { name: 'Orders' })).not.toBeInTheDocument()
      await waitFor(() => expect(portfolioCalls()).not.toHaveLength(0))
      expect(orderCalls()).toHaveLength(0)
    })
  })

  it('removes one leg', async () => {
    const user = userEvent.setup()
    for (const leg of LEGS) useAppStore.getState().setModelQty(leg.contract, leg.qty)
    renderWithClient(<ModelTicket legs={LEGS} marks={new Map()} include={false} selectedIds={[]} />)
    await user.click(screen.getByRole('button', { name: 'Remove SPX 16 OCT 26 7500 P from the model' }))
    expect(useAppStore.getState().model.legs.map((leg) => leg.contract.tos_code)).toEqual(['SPX261016P7400'])
  })

  it('warns when the last registration left a modeled contract without a quote', async () => {
    const actual = await vi.importActual<typeof import('../../api/client')>('../../api/client')
    vi.mocked(call).mockImplementation(async (command) => {
      if (command === 'set_model_contracts') {
        return { contracts: 2, quoted: 1, missing: ['SPX261016P7400'], refreshed: true, detail: null } as never
      }
      return { count: 0, pnl_open: 0, pnl_total: 0, greeks: { delta: 0, gamma: 0, theta: 0, vega: 0 }, weighted_vega: 0, weighted_vega_reference: '30d' } as never
    })
    function Registers() {
      const { mutate } = useSetModelContracts()
      useEffect(() => mutate([]), [mutate])
      return <ModelTicket legs={LEGS} marks={new Map()} include selectedIds={[]} />
    }
    try {
      renderWithClient(<Registers />)
      const note = await screen.findByRole('status')
      expect(note).toHaveTextContent('No quote for 1 modeled contract — priced from neighbouring strikes')
      expect(note).toHaveAttribute('title', expect.stringContaining('SPX261016P7400'))
      expect(note).toHaveClass('text-warn', 'truncate')
      // Its own line under the header, never squeezing the heading.
      expect(screen.getByRole('heading', { name: 'Model ticket' }).parentElement).not.toContainElement(note)
    } finally {
      vi.mocked(call).mockImplementation(actual.call)
    }
  })

  // A refused registration priced nothing from anywhere: say what the server
  // said, in red — and the greeks, refused for the same legs, say so too
  // rather than sitting on '…' for good.
  it('shows a refusal and a failed greeks request as what they are', async () => {
    const actual = await vi.importActual<typeof import('../../api/client')>('../../api/client')
    const refusal = '2026-08-28 expired before 2026-08-31; a modeled leg must be live'
    vi.mocked(call).mockImplementation(async (command) => {
      if (command === 'market_snapshot') return { as_of: '2026-08-31T16:00:00Z' } as never
      throw new actual.ApiError(refusal, 422, null)
    })
    function Registers() {
      const { mutate } = useSetModelContracts()
      useEffect(() => mutate([]), [mutate])
      return <ModelTicket legs={LEGS} marks={new Map()} include selectedIds={[]} />
    }
    try {
      renderWithClient(<Registers />)
      const note = await screen.findByRole('status')
      expect(note).toHaveTextContent(`Model refused: ${refusal}`)
      expect(note).toHaveClass('text-neg')
      expect(await screen.findByText(`Model greeks unavailable: ${refusal}`)).toBeInTheDocument()
      expect(screen.queryByText('…')).not.toBeInTheDocument()
    } finally {
      vi.mocked(call).mockImplementation(actual.call)
    }
  })
})
