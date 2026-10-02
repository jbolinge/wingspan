import { render, screen, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'
import type { ChainExpiry } from '../../api/types'
import { ExpiryStrip } from './ExpiryStrip'

const EXPIRATIONS: ChainExpiry[] = [
  { expiry: '2026-09-30', dte: 30, root: 'SPXW', settlement: 'pm', kind: 'quarterly' },
  { expiry: '2026-10-15', dte: 45, root: 'SPXW', settlement: 'pm', kind: 'daily' },
  { expiry: '2026-10-16', dte: 46, root: 'SPX', settlement: 'am', kind: 'monthly' },
  { expiry: '2026-10-16', dte: 46, root: 'SPXW', settlement: 'pm', kind: 'weekly' },
  { expiry: '2026-11-20', dte: 81, root: 'SPX', settlement: 'am', kind: 'eom-ish' },
]

function strip(props: Partial<Parameters<typeof ExpiryStrip>[0]> = {}) {
  const onToggle = vi.fn()
  render(
    <ExpiryStrip
      expirations={EXPIRATIONS}
      selected="2026-10-16|SPX"
      held={new Set(['2026-10-16|SPX'])}
      modeled={new Set(['2026-11-20|SPX'])}
      onToggle={onToggle}
      {...props}
    />,
  )
  return { onToggle, group: screen.getByRole('toolbar', { name: 'Expirations' }) }
}

describe('ExpiryStrip', () => {
  it('writes each chip as the date over days-to-expiry and settlement', () => {
    const { group } = strip()
    const chips = within(group).getAllByRole('button')
    expect(chips.map((chip) => chip.textContent)).toEqual([
      '30 SEP 2630d · PM',
      '15 OCT 2645d · PM',
      '16 OCT 26SPX46d · AM',
      '16 OCT 26SPXW46d · PM',
      '20 NOV 2681d · AM',
    ])
  })

  it('presses the chip on screen and toggles by (expiry, root)', async () => {
    const { group, onToggle } = strip()
    const am = within(group).getByRole('button', { name: /^16 OCT 26 · 46d · AM/ })
    expect(am).toHaveAttribute('aria-pressed', 'true')
    expect(within(group).getAllByRole('button', { pressed: true })).toHaveLength(1)
    await userEvent.click(within(group).getByRole('button', { name: /^16 OCT 26 · 46d · PM/ }))
    expect(onToggle).toHaveBeenCalledWith('2026-10-16|SPXW')
  })

  it('says where the position and the model hold legs', () => {
    const { group } = strip()
    expect(within(group).getByRole('button', { name: /position held/ })).toHaveAttribute('data-key', '2026-10-16|SPX')
    expect(within(group).getByRole('button', { name: /modeled/ })).toHaveAttribute('data-key', '2026-11-20|SPX')
  })

  // Seventy-odd chips on a live listing must not be seventy-odd Tab presses
  // on the way to the chain: the strip is one stop, arrows move inside it.
  it('is a single tab stop with arrow keys, Home and End inside it', async () => {
    const user = userEvent.setup()
    const { group, onToggle } = strip()
    const chips = within(group).getAllByRole('button')
    // The stop starts on the first section on screen (16 OCT SPX).
    expect(chips.map((chip) => chip.tabIndex)).toEqual([-1, -1, 0, -1, -1])

    await user.tab()
    expect(chips[2]).toHaveFocus()
    await user.keyboard('{ArrowRight}')
    expect(chips[3]).toHaveFocus()
    await user.keyboard('{End}')
    expect(chips[4]).toHaveFocus()
    await user.keyboard('{ArrowRight}')
    expect(chips[4]).toHaveFocus()
    await user.keyboard('{Home}')
    expect(chips[0]).toHaveFocus()
    await user.keyboard('{ArrowLeft}')
    expect(chips[0]).toHaveFocus()
    // The stop follows focus, so Tab out and back returns to the same chip.
    expect(chips.map((chip) => chip.tabIndex)).toEqual([0, -1, -1, -1, -1])
    // Enter and Space still toggle, as buttons do.
    await user.keyboard('{Enter}')
    expect(onToggle).toHaveBeenLastCalledWith('2026-09-30|SPXW')
    await user.keyboard('{ArrowRight}[Space]')
    expect(onToggle).toHaveBeenLastCalledWith('2026-10-15|SPXW')
  })

  it('presses nothing, and starts the tab stop on the first chip, when no chain is open', () => {
    const { group } = strip({ selected: null })
    expect(within(group).queryAllByRole('button', { pressed: true })).toHaveLength(0)
    expect(within(group).getAllByRole('button').map((chip) => chip.tabIndex)).toEqual([0, -1, -1, -1, -1])
  })

  it('renders a kind it has never heard of', () => {
    const { group } = strip()
    expect(within(group).getByRole('button', { name: /eom-ish/ })).toBeInTheDocument()
  })
})
