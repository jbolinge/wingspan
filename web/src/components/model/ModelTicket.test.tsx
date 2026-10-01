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
    renderWithClient(<ModelTicket legs={[]} marks={new Map()} include />)
    expect(screen.getByText(/No modeled legs/)).toBeInTheDocument()
    expect(portfolioCalls()).toHaveLength(0)
  })

  // The rail is narrow: the contract label is the part that truncates, so
  // it carries its full text as a tooltip, and the remove button never
  // shrinks out of reach.
  it('lets a long label truncate without losing it or the remove button', () => {
    renderWithClient(<ModelTicket legs={LEGS} marks={new Map()} include />)
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
    renderWithClient(<ModelTicket legs={LEGS} marks={marks} include />)
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
    renderWithClient(<ModelTicket legs={LEGS} marks={new Map([['SPX261016P7500', 42.5]])} include />)
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
    renderWithClient(<ModelTicket legs={[...LEGS, expired]} marks={new Map()} include />)
    await waitFor(() => expect(portfolioCalls()).not.toHaveLength(0))
    const [, args] = portfolioCalls().at(-1)!
    const sent = (args as { request: { model_legs: { contract: { expiry: string } }[] } }).request.model_legs
    expect(sent.map((leg) => leg.contract.expiry)).toEqual(['2026-10-16', '2026-10-16'])
  })

  it('computes no greeks for a model hidden from the graph', () => {
    renderWithClient(<ModelTicket legs={LEGS} marks={new Map()} include={false} />)
    expect(screen.getByText(/Hidden from the risk graph/)).toBeInTheDocument()
    expect(portfolioCalls()).toHaveLength(0)
  })

  it('removes one leg', async () => {
    const user = userEvent.setup()
    for (const leg of LEGS) useAppStore.getState().setModelQty(leg.contract, leg.qty)
    renderWithClient(<ModelTicket legs={LEGS} marks={new Map()} include={false} />)
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
      return <ModelTicket legs={LEGS} marks={new Map()} include />
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
      return <ModelTicket legs={LEGS} marks={new Map()} include />
    }
    try {
      renderWithClient(<Registers />)
      const note = await screen.findByRole('status')
      expect(note).toHaveTextContent(`Model refused: ${refusal}`)
      expect(note).toHaveClass('text-neg')
      expect(await screen.findByText(`Greeks unavailable: ${refusal}`)).toBeInTheDocument()
      expect(screen.queryByText('…')).not.toBeInTheDocument()
    } finally {
      vi.mocked(call).mockImplementation(actual.call)
    }
  })
})
