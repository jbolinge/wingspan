import { screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { ApiError, call } from '../../api/client'
import type { ModelLeg, ModelOrdersResponse, OptionContract, TosOrderDto } from '../../api/types'
import { copyText } from '../../lib/clipboard'
import { renderWithClient } from '../../test/utils'
import { COPIED_MS, ModelOrders } from './ModelOrders'

vi.mock('../../api/client', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../api/client')>()
  return { ...actual, call: vi.fn(actual.call) }
})

vi.mock('../../lib/clipboard', () => ({ copyText: vi.fn(async () => {}) }))

function contract(strike: number, right: 'call' | 'put'): OptionContract {
  const cp = right === 'call' ? 'C' : 'P'
  return {
    underlying: 'SPX',
    root: 'SPX',
    expiry: '2026-11-20',
    settlement: 'am',
    strike,
    right,
    multiplier: 100,
    osi: `SPX   261120${cp}0${strike}000`,
    tos_code: `SPX261120${cp}${strike}`,
  }
}

/** What the ticket hands the block: the live, sorted request legs. */
const LEGS: ModelLeg[] = [
  { contract: contract(8400, 'call'), qty: -2 },
  { contract: contract(8500, 'call'), qty: 2 },
  { contract: contract(7000, 'put'), qty: -2 },
  { contract: contract(6900, 'put'), qty: 2 },
  { contract: contract(5050, 'put'), qty: 1 },
]

const CONDOR = 'SELL -2 IRON CONDOR SPX 100 20 NOV 26 [AM] 8400/8500/7000/6900 CALL/PUT @22.20 LMT'
const SINGLE = 'BUY +1 SPX 100 20 NOV 26 [AM] 5050 PUT @4.90 LMT'

function order(partial: Partial<TosOrderDto> & Pick<TosOrderDto, 'line'>): TosOrderDto {
  return { strategy: 'single', side: 'buy', qty: 1, net_price: null, legs: [], unquoted: [], ...partial }
}

const RESPONSE: ModelOrdersResponse = {
  orders: [
    order({ strategy: 'iron_condor', side: 'sell', qty: 2, line: CONDOR, net_price: 22.2 }),
    order({ line: SINGLE, net_price: 4.9 }),
  ],
  text: `${CONDOR}\n${SINGLE}\n`,
}

/** Answer `model_orders` with `answer`; everything else goes to the mock. */
async function answerOrders(answer: () => Promise<unknown>): Promise<void> {
  const actual = await vi.importActual<typeof import('../../api/client')>('../../api/client')
  vi.mocked(call).mockImplementation((command, args, signal) =>
    command === 'model_orders' ? (answer() as never) : actual.call(command, args, signal),
  )
}

const block = () => screen.getByRole('group', { name: 'Orders' })
const copyAll = () => within(block()).getByRole('button', { name: 'Copy all orders' })

beforeEach(async () => {
  await answerOrders(async () => RESPONSE)
})

afterEach(async () => {
  const actual = await vi.importActual<typeof import('../../api/client')>('../../api/client')
  vi.mocked(call).mockImplementation(actual.call)
  vi.mocked(copyText).mockReset()
})

describe('ModelOrders', () => {
  it('asks for the legs it is given, in the request envelope', async () => {
    renderWithClient(<ModelOrders legs={LEGS} />)
    await screen.findByText(CONDOR)
    const calls = vi.mocked(call).mock.calls.filter(([command]) => command === 'model_orders')
    expect(calls).toEqual([['model_orders', { request: { model_legs: LEGS } }]])
  })

  it('lists one row per order: the strategy, the whole TOS line, and its own Copy button', async () => {
    renderWithClient(<ModelOrders legs={LEGS} />)
    await screen.findByText(CONDOR)
    expect(within(block()).getByText('2 orders')).toBeInTheDocument()
    const rows = within(block()).getAllByRole('listitem')
    expect(rows).toHaveLength(2)
    expect(within(rows[0]!).getByText('Iron condor')).toBeInTheDocument()
    expect(within(rows[1]!).getByText('Single')).toBeInTheDocument()
    // The line truncates above the chain (whole on hover) and wraps in the
    // rail, where truncated it lost its strikes and limit; a click selects it.
    const line = within(rows[0]!).getByText(CONDOR)
    expect(line).toHaveAttribute('title', CONDOR)
    expect(line).toHaveClass('truncate', 'select-all', 'num', 'min-[1400px]:whitespace-normal')
    expect(within(rows[0]!).getByRole('button', { name: `Copy order 1: ${CONDOR}` })).toHaveTextContent('Copy')
    expect(within(rows[1]!).getByRole('button', { name: `Copy order 2: ${SINGLE}` })).toBeInTheDocument()
    expect(copyAll()).toBeEnabled()
    expect(screen.getByText('Prices are the net mark per spread — set your own limit in TOS.')).toBeInTheDocument()
    expect(screen.queryByText(/has no quote/)).not.toBeInTheDocument()
  })

  it('copies one order as its bare line, and says so for a moment', async () => {
    const user = userEvent.setup()
    renderWithClient(<ModelOrders legs={LEGS} />)
    await screen.findByText(CONDOR)
    const button = screen.getByRole('button', { name: `Copy order 1: ${CONDOR}` })
    await user.click(button)
    expect(copyText).toHaveBeenCalledWith(CONDOR)
    expect(button).toHaveTextContent('Copied')
    expect(screen.getByRole('status')).toHaveTextContent('Copied 1 order')
    // Only the button that copied says so.
    expect(screen.getByRole('button', { name: `Copy order 2: ${SINGLE}` })).toHaveTextContent('Copy')
    expect(copyAll()).toHaveTextContent('Copy all')
    await waitFor(() => expect(button).toHaveTextContent(/^Copy$/), { timeout: COPIED_MS + 1000 })
    expect(screen.queryByRole('status')).not.toBeInTheDocument()
  })

  it('copies every line, newline-terminated, with Copy all', async () => {
    const user = userEvent.setup()
    renderWithClient(<ModelOrders legs={LEGS} />)
    await screen.findByText(CONDOR)
    await user.click(copyAll())
    expect(copyText).toHaveBeenCalledWith(`${CONDOR}\n${SINGLE}\n`)
    expect(copyAll()).toHaveTextContent('Copied')
    expect(screen.getByRole('status')).toHaveTextContent('Copied 2 orders')
  })

  it('says why a copy failed, and clears it on the next good one', async () => {
    const user = userEvent.setup()
    vi.mocked(copyText).mockRejectedValueOnce(new Error('the clipboard is not available in this browser'))
    renderWithClient(<ModelOrders legs={LEGS} />)
    await screen.findByText(CONDOR)
    const button = screen.getByRole('button', { name: `Copy order 2: ${SINGLE}` })
    await user.click(button)
    const failure = screen.getByText("Couldn't copy: the clipboard is not available in this browser")
    expect(failure).toHaveClass('text-neg')
    expect(button).toHaveTextContent(/^Copy$/)
    expect(screen.queryByRole('status')).not.toBeInTheDocument()

    await user.click(button)
    expect(screen.queryByText(/Couldn't copy/)).not.toBeInTheDocument()
    expect(screen.getByRole('status')).toHaveTextContent('Copied 1 order')
  })

  it('names every contract whose price is modeled rather than quoted', async () => {
    await answerOrders(async () => ({
      ...RESPONSE,
      orders: [{ ...RESPONSE.orders[0]!, unquoted: ['SPX261120C8500'] }, RESPONSE.orders[1]!],
    }))
    renderWithClient(<ModelOrders legs={LEGS} />)
    expect(await screen.findByText('SPX261120C8500 has no quote; its price is modeled.')).toBeInTheDocument()
  })

  it('waits with a … and nothing to copy while the orders load', async () => {
    await answerOrders(() => new Promise(() => {}))
    renderWithClient(<ModelOrders legs={LEGS} />)
    expect(within(block()).getByText('…')).toBeInTheDocument()
    expect(copyAll()).toBeDisabled()
    expect(within(block()).queryByRole('listitem')).not.toBeInTheDocument()
  })

  it('says what the server said when it refused the legs', async () => {
    const refusal = '2026-08-28 expired before 2026-08-31; a modeled leg must be live'
    await answerOrders(async () => {
      throw new ApiError(refusal, 422, null)
    })
    renderWithClient(<ModelOrders legs={LEGS} />)
    const line = await screen.findByText(`Orders unavailable: ${refusal}`)
    expect(line).toHaveClass('text-neg')
    expect(copyAll()).toBeDisabled()
    expect(screen.queryByText('…')).not.toBeInTheDocument()
  })

  it('keeps the lines on screen but refuses to copy them when a refresh fails', async () => {
    let fail = false
    await answerOrders(async () => {
      if (fail) throw new ApiError('no market data — the provider is unreachable', 503, null)
      return RESPONSE
    })
    const { client } = renderWithClient(<ModelOrders legs={LEGS} />)
    await screen.findByText(CONDOR)
    expect(copyAll()).toBeEnabled()

    fail = true
    await client.invalidateQueries({ queryKey: ['model-orders'] })
    const note = await screen.findByText('Orders not refreshed: no market data — the provider is unreachable')
    expect(note).toHaveClass('text-neg')
    // The old lines stay readable, but they may no longer be the model.
    expect(within(block()).getByText(CONDOR)).toBeInTheDocument()
    expect(copyAll()).toBeDisabled()
    for (const button of within(block()).getAllByRole('button', { name: /^Copy order/ })) {
      expect(button).toBeDisabled()
    }

    // The next good answer clears the note and gives the buttons back.
    fail = false
    await client.invalidateQueries({ queryKey: ['model-orders'] })
    await waitFor(() => expect(screen.queryByText(/Orders not refreshed/)).not.toBeInTheDocument())
    expect(copyAll()).toBeEnabled()
  })

  it('keeps keyboard focus on a Copy button when the order is repriced', async () => {
    const user = userEvent.setup()
    let price = '22.20'
    await answerOrders(async () => {
      const line = CONDOR.replace('@22.20', `@${price}`)
      return { ...RESPONSE, orders: [{ ...RESPONSE.orders[0]!, line }, RESPONSE.orders[1]!] }
    })
    const { client } = renderWithClient(<ModelOrders legs={LEGS} />)
    await screen.findByText(CONDOR)
    await user.tab()
    await user.tab()
    const button = screen.getByRole('button', { name: `Copy order 1: ${CONDOR}` })
    expect(button).toHaveFocus()

    price = '22.45'
    await client.invalidateQueries({ queryKey: ['model-orders'] })
    const repriced = CONDOR.replace('@22.20', '@22.45')
    await screen.findByText(repriced)
    // The same element, renamed — not a new row that left focus on <body>.
    expect(button).toHaveAccessibleName(`Copy order 1: ${repriced}`)
    expect(button).toHaveFocus()
  })

  it('has nothing to copy when the legs net to nothing', async () => {
    await answerOrders(async () => ({ orders: [], text: '' }))
    renderWithClient(<ModelOrders legs={LEGS} />)
    expect(await screen.findByText('0 orders')).toBeInTheDocument()
    expect(copyAll()).toBeDisabled()
    expect(screen.queryByText(/net mark per spread/)).not.toBeInTheDocument()
  })
})
