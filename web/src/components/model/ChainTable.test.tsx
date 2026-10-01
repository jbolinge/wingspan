import { render, screen, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it } from 'vitest'
import type { ChainQuote, OptionChainResponse, OptionContract, Right } from '../../api/types'
import { MINUS, modelQtyByContract } from '../../lib/model'
import { useAppStore, type StrikeCount } from '../../store/appStore'
import { resetAppStore } from '../../test/utils'
import { ChainTable, ITM_CLASS } from './ChainTable'

beforeEach(() => resetAppStore())

function contract(strike: number, right: Right): OptionContract {
  const cp = right === 'call' ? 'C' : 'P'
  return {
    underlying: 'SPX',
    root: 'SPX',
    expiry: '2026-10-16',
    settlement: 'am',
    strike,
    right,
    multiplier: 100,
    osi: `SPX   261016${cp}${String(strike * 1000).padStart(8, '0')}`,
    tos_code: `SPX261016${cp}${strike}`,
  }
}

function quote(strike: number, right: Right, mark: number | null = 25): ChainQuote {
  return {
    contract: contract(strike, right),
    bid: mark === null ? null : mark - 0.5,
    ask: mark === null ? null : mark + 0.5,
    mark,
    last: null,
    iv: mark === null ? null : 0.1712,
    iv_hint: null,
    greeks: mark === null ? null : { delta: 0.4567, gamma: 0.00123, theta: -1.234, vega: 5.678 },
    volume: null,
    open_interest: null,
    quote_as_of: '2026-08-31T16:00:00Z',
  }
}

/** 13 strikes, 7600–7900 by 25, spot between 7725 and 7750. */
const STRIKES = Array.from({ length: 13 }, (_, i) => 7600 + i * 25)

function chain(overrides: Partial<OptionChainResponse> = {}): OptionChainResponse {
  return {
    as_of: '2026-08-31T16:00:00Z',
    source: 'fixture',
    stale: false,
    degraded: [],
    underlying: 'SPX',
    spot: 7740.25,
    expiry: '2026-10-16',
    root: 'SPX',
    settlement: 'am',
    dte: 46,
    forward: 7782.1,
    rows: STRIKES.map((strike) => ({ strike, call: quote(strike, 'call'), put: quote(strike, 'put') })),
    warnings: [],
    ...overrides,
  }
}

/** The table wired to the real store, the way ModelTab wires it. */
function Connected({
  data = chain(),
  positions = new Map<string, number>(),
  strikeCount = null,
}: {
  data?: OptionChainResponse
  positions?: Map<string, number>
  strikeCount?: StrikeCount
}) {
  const legs = useAppStore((s) => s.model.legs)
  return (
    <ChainTable
      chain={data}
      positions={positions}
      modelQty={modelQtyByContract(legs)}
      strikeCount={strikeCount}
      onSetQty={(c, qty) => useAppStore.getState().setModelQty(c, qty)}
      onShowAll={() => useAppStore.getState().setModelStrikeCount(null)}
    />
  )
}

const row = (strike: number): HTMLElement => {
  const found = document.querySelector<HTMLElement>(`tr[data-strike="${strike}"]`)
  if (!found) throw new Error(`no row for ${strike}`)
  return found
}

/** The cells of one side of a row, keyed by their data-col. */
function cells(strike: number, side: 'call' | 'put'): Record<string, HTMLElement> {
  const all = Array.from(row(strike).querySelectorAll<HTMLElement>('td'))
  const half = side === 'call' ? all.slice(0, 10) : all.slice(10)
  return Object.fromEntries(half.map((cell) => [cell.dataset.col ?? '', cell]))
}

describe('ChainTable', () => {
  it('mirrors the columns around a dominant strike column, with the forward in its header', () => {
    render(<Connected />)
    const headers = within(screen.getAllByRole('row')[1]!).getAllByRole('columnheader')
    expect(headers.map((h) => h.textContent)).toEqual([
      'V', 'Θ', 'Γ', 'Δ', 'IV', 'Ask', 'Bid', 'Mark', 'Model', 'Pos',
      'Pos', 'Model', 'Mark', 'Bid', 'Ask', 'IV', 'Δ', 'Γ', 'Θ', 'V',
    ])
    expect(screen.getByRole('columnheader', { name: /Strike/ })).toHaveTextContent('F 7782.10')
    expect(within(row(7700)).getByRole('rowheader')).toHaveTextContent('7700')
  })

  it('formats IV as a percent and greeks per contract at 2/4/2/2 dp', () => {
    render(<Connected />)
    const call = cells(7700, 'call')
    expect(call.iv).toHaveTextContent('17.1%')
    expect(call.delta).toHaveTextContent('0.46')
    expect(call.gamma).toHaveTextContent('0.0012')
    expect(call.theta).toHaveTextContent('-1.23')
    expect(call.vega).toHaveTextContent('5.68')
    expect(call.mark).toHaveTextContent('25.00')
  })

  it('shows the held quantity in Pos, signed, blank where nothing is held', () => {
    const positions = new Map([
      ['SPX261016P7650', -1],
      ['SPX261016C7800', 2],
    ])
    render(<Connected positions={positions} />)
    expect(cells(7650, 'put').pos).toHaveTextContent(`${MINUS}1`)
    expect(cells(7650, 'put').pos.className).toContain('text-neg')
    expect(cells(7800, 'call').pos).toHaveTextContent('+2')
    expect(cells(7800, 'call').pos.className).toContain('text-pos')
    expect(cells(7650, 'call').pos).toHaveTextContent('')
    // Pos is read-only: no field in it.
    expect(within(cells(7650, 'put').pos).queryByRole('spinbutton')).toBeNull()
  })

  it('writes what is typed into a Model cell to the store', async () => {
    const user = userEvent.setup()
    render(<Connected />)
    const cell = screen.getByRole('spinbutton', { name: 'Model quantity, SPX 16 Oct 26 7700 put' })
    await user.click(cell)
    await user.keyboard('-2{Enter}')
    const legs = useAppStore.getState().model.legs
    expect(legs).toHaveLength(1)
    expect(legs[0]).toMatchObject({ qty: -2, contract: { tos_code: 'SPX261016P7700' } })
    expect(cell).toHaveValue(`${MINUS}2`)
  })

  it('moves down the same Model column on Enter', async () => {
    const user = userEvent.setup()
    render(<Connected />)
    await user.click(screen.getByRole('spinbutton', { name: 'Model quantity, SPX 16 Oct 26 7700 call' }))
    await user.keyboard('1{Enter}')
    expect(document.activeElement).toBe(
      screen.getByRole('spinbutton', { name: 'Model quantity, SPX 16 Oct 26 7725 call' }),
    )
    // Tab walks the column too, rather than jumping across to the put side.
    await user.tab()
    expect(document.activeElement).toBe(
      screen.getByRole('spinbutton', { name: 'Model quantity, SPX 16 Oct 26 7750 call' }),
    )
  })

  it('shades in-the-money cells: calls below spot, puts above it', () => {
    render(<Connected />)
    expect(cells(7725, 'call').mark.className).toContain(ITM_CLASS)
    expect(cells(7725, 'put').mark.className).not.toContain(ITM_CLASS)
    expect(cells(7750, 'call').mark.className).not.toContain(ITM_CLASS)
    expect(cells(7750, 'put').mark.className).toContain(ITM_CLASS)
    expect(cells(7750, 'put').model.className).toContain(ITM_CLASS)
  })

  // Both a tint and the ITM shade on one cell resolve by stylesheet order,
  // and the shade came later: an in-the-money held leg lost its highlight.
  it('tints a held cell instead of shading it, in the money or not', () => {
    const positions = new Map([
      ['SPX261016C7650', -1], // ITM call
      ['SPX261016P7800', 2], // ITM put
      ['SPX261016P7650', 1], // OTM put
    ])
    render(<Connected positions={positions} />)
    for (const pos of [cells(7650, 'call').pos, cells(7800, 'put').pos, cells(7650, 'put').pos]) {
      expect(pos.className).toContain('bg-accent-soft')
      expect(pos.className).not.toContain(ITM_CLASS)
    }
    // Nothing held: the Pos cell shades with the rest of its side.
    expect(cells(7700, 'call').pos.className).toContain(ITM_CLASS)
    expect(cells(7700, 'put').pos.className).not.toContain(ITM_CLASS)
  })

  // Only the header row stays pinned while stacked sections scroll under it,
  // so it names the expiration — for sighted readers; the caption already
  // does for a screen reader.
  it('names the section in the pinned header row', () => {
    render(<Connected />)
    const label = screen.getByTestId('sticky-section')
    expect(label).toHaveTextContent('SPX · 16 OCT 26 · AM · 46d')
    expect(label).toHaveAttribute('aria-hidden', 'true')
    expect(label.closest('thead')).toHaveClass('sticky')
    expect(screen.getByRole('columnheader', { name: 'Calls' })).toBeInTheDocument()
  })

  it('sets the strike column on its own colour, a step above the ITM shade', () => {
    render(<Connected />)
    expect(within(row(7700)).getByRole('rowheader').className).toContain('bg-strike')
    expect(screen.getByRole('columnheader', { name: /Strike/ }).className).toContain('bg-strike')
  })

  it('draws the spot line between the two strikes that bracket spot', () => {
    render(<Connected />)
    const spotRow = screen.getByTestId('spot-row')
    expect(spotRow).toHaveTextContent('SPX 7740.25')
    expect(spotRow.previousElementSibling).toBe(row(7725))
    expect(spotRow.nextElementSibling).toBe(row(7750))
  })

  it('puts the spot line at the edge when spot is outside the strikes shown', () => {
    render(<Connected data={chain({ spot: 7000 })} />)
    expect(screen.getByTestId('spot-row').nextElementSibling).toBe(row(7600))
  })

  it('rails the strike nearest spot in accent', () => {
    render(<Connected />)
    expect(within(row(7750)).getByRole('rowheader').className).toContain('border-x-accent')
    expect(within(row(7725)).getByRole('rowheader').className).not.toContain('border-x-accent')
  })

  it('limits the rows to strikeCount around spot, with a way to show all', async () => {
    const user = userEvent.setup()
    const { rerender } = render(<Connected strikeCount={16} />)
    expect(screen.getByText('13 of 13 strikes')).toBeInTheDocument()
    // `strikeCount` is typed to the toolbar's choices; 4 here only shrinks the
    // fixture's window the way 16 shrinks a real one.
    rerender(<Connected strikeCount={4 as StrikeCount} />)
    expect(document.querySelectorAll('tr[data-strike]')).toHaveLength(4)
    expect(document.querySelector('tr[data-strike="7700"]')).not.toBeNull()
    expect(document.querySelector('tr[data-strike="7775"]')).not.toBeNull()
    expect(document.querySelector('tr[data-strike="7600"]')).toBeNull()
    await user.click(screen.getByRole('button', { name: 'Show all' }))
    expect(useAppStore.getState().model.strikeCount).toBeNull()
  })

  it('never hides a held or modeled strike, and marks the gap', () => {
    useAppStore.getState().setModelQty(contract(7900, 'call'), 1)
    render(<Connected strikeCount={4 as StrikeCount} positions={new Map([['SPX261016P7600', -1]])} />)
    expect(document.querySelector('tr[data-strike="7600"]')).not.toBeNull()
    expect(document.querySelector('tr[data-strike="7900"]')).not.toBeNull()
    expect(screen.getByText('⋯ 3 strikes ⋯')).toBeInTheDocument()
    expect(screen.getByText('⋯ 4 strikes ⋯')).toBeInTheDocument()
    expect(screen.getByText(/held and modeled strikes always shown/)).toBeInTheDocument()
  })

  it('will not model a contract with no mark', () => {
    const data = chain()
    data.rows[4] = { strike: 7700, call: quote(7700, 'call', null), put: null }
    render(<Connected data={data} />)
    const cell = screen.getByRole('spinbutton', { name: 'Model quantity, SPX 16 Oct 26 7700 call' })
    expect(cell).toBeDisabled()
    expect(cell.parentElement).toHaveAttribute('title', 'No mark — this contract cannot be modeled')
    expect(cells(7700, 'call').mark).toHaveTextContent('—')
    // A side that was never quoted has no field at all.
    expect(screen.queryByRole('spinbutton', { name: 'Model quantity, SPX 16 Oct 26 7700 put' })).toBeNull()
  })

  it('surfaces model-side warnings and a stale chain in the footer', () => {
    render(
      <Connected
        data={chain({
          stale: true,
          degraded: ['schwab: timed out'],
          warnings: [
            { severity: 'warn', code: 'iv_solve_failed', message: 'crossed', trade_id: null, contract: 'SPX261016C7700' },
          ],
        })}
      />,
    )
    expect(screen.getByText('⚠ 1')).toBeInTheDocument()
    expect(screen.getByText('stale')).toHaveAttribute('title', 'schwab: timed out')
  })
})
