import { fireEvent, render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { useState } from 'react'
import { describe, expect, it, vi } from 'vitest'
import { MINUS, MODEL_DRAG_TYPE } from '../../lib/model'
import { QtyInput } from './QtyInput'

/** jsdom has no DataTransfer; a map with the handful of members the cell uses. */
function dataTransfer(): DataTransfer {
  const data = new Map<string, string>()
  return {
    effectAllowed: 'uninitialized',
    dropEffect: 'none',
    get types() {
      return Array.from(data.keys())
    },
    setData: (type: string, value: string) => void data.set(type, value),
    getData: (type: string) => data.get(type) ?? '',
  } as unknown as DataTransfer
}

const LABEL = 'Model quantity, SPXW 16 Oct 26 7700 call'

/** A QtyInput whose committed value round-trips through state, like the store. */
function Harness({ initial = 0, onCommit }: { initial?: number; onCommit?: (qty: number) => void }) {
  const [value, setValue] = useState(initial)
  return (
    <QtyInput
      value={value}
      label={LABEL}
      onCommit={(qty) => {
        onCommit?.(qty)
        setValue(qty)
      }}
    />
  )
}

const field = () => screen.getByRole('spinbutton', { name: LABEL })

describe('QtyInput', () => {
  it('is blank at zero and shows a signed quantity otherwise', () => {
    const { rerender } = render(<QtyInput value={0} label={LABEL} onCommit={() => {}} />)
    expect(field()).toHaveValue('')
    expect(field()).toHaveAttribute('aria-valuetext', 'none')
    rerender(<QtyInput value={2} label={LABEL} onCommit={() => {}} />)
    expect(field()).toHaveValue('+2')
    rerender(<QtyInput value={-3} label={LABEL} onCommit={() => {}} />)
    expect(field()).toHaveValue(`${MINUS}3`)
    expect(field()).toHaveAttribute('aria-valuenow', '-3')
  })

  it('commits on Enter, not on every keystroke', async () => {
    const user = userEvent.setup()
    const onCommit = vi.fn()
    render(<Harness onCommit={onCommit} />)
    await user.click(field())
    await user.keyboard('-1')
    expect(onCommit).not.toHaveBeenCalled()
    await user.keyboard('{Enter}')
    expect(onCommit).toHaveBeenCalledExactlyOnceWith(-1)
    expect(field()).toHaveValue(`${MINUS}1`)
  })

  it('commits on blur', async () => {
    const user = userEvent.setup()
    const onCommit = vi.fn()
    render(<Harness onCommit={onCommit} />)
    await user.click(field())
    await user.keyboard('4')
    await user.tab()
    expect(onCommit).toHaveBeenCalledExactlyOnceWith(4)
  })

  it('reverts the draft on Esc', async () => {
    const user = userEvent.setup()
    const onCommit = vi.fn()
    render(<Harness initial={2} onCommit={onCommit} />)
    await user.click(field())
    await user.keyboard('9{Escape}')
    expect(field()).toHaveValue('+2')
    fireEvent.blur(field())
    expect(onCommit).not.toHaveBeenCalled()
  })

  it('rounds and clamps what it commits', async () => {
    const user = userEvent.setup()
    const onCommit = vi.fn()
    render(<Harness onCommit={onCommit} />)
    await user.click(field())
    await user.keyboard('2.6{Enter}')
    expect(onCommit).toHaveBeenLastCalledWith(3)
    // Still focused after Enter, so select the text the way a second click
    // into the cell would not.
    await user.clear(field())
    await user.keyboard('-5000{Enter}')
    expect(onCommit).toHaveBeenLastCalledWith(-999)
  })

  it('reverts unreadable text instead of guessing', async () => {
    const user = userEvent.setup()
    const onCommit = vi.fn()
    render(<Harness initial={1} onCommit={onCommit} />)
    await user.click(field())
    await user.keyboard('abc{Enter}')
    expect(onCommit).not.toHaveBeenCalled()
    expect(field()).toHaveValue('+1')
  })

  it('clears to zero when the text is deleted', async () => {
    const user = userEvent.setup()
    const onCommit = vi.fn()
    render(<Harness initial={5} onCommit={onCommit} />)
    await user.click(field())
    await user.keyboard('{Backspace}{Enter}')
    expect(onCommit).toHaveBeenCalledExactlyOnceWith(0)
    expect(field()).toHaveValue('')
  })

  it('steps the draft with the arrow keys, committing only on Enter', async () => {
    const user = userEvent.setup()
    const onCommit = vi.fn()
    render(<Harness initial={1} onCommit={onCommit} />)
    await user.click(field())
    await user.keyboard('{ArrowUp}{ArrowUp}')
    expect(field()).toHaveValue('+3')
    await user.keyboard('{ArrowDown}{ArrowDown}{ArrowDown}{ArrowDown}')
    expect(field()).toHaveValue(`${MINUS}1`)
    expect(onCommit).not.toHaveBeenCalled()
    await user.keyboard('{Enter}')
    expect(onCommit).toHaveBeenCalledExactlyOnceWith(-1)
  })

  it('does not commit an unchanged value', async () => {
    const user = userEvent.setup()
    const onCommit = vi.fn()
    render(<Harness initial={2} onCommit={onCommit} />)
    await user.click(field())
    await user.keyboard('{Enter}')
    await user.click(field())
    await user.keyboard('+2{Enter}')
    expect(onCommit).not.toHaveBeenCalled()
  })

  it('moves down the column on Enter and walks it with Tab', async () => {
    const user = userEvent.setup()
    const onNavigate = vi.fn(() => true)
    render(<QtyInput value={0} label={LABEL} onCommit={() => {}} onNavigate={onNavigate} />)
    await user.click(field())
    await user.keyboard('{Enter}')
    expect(onNavigate).toHaveBeenLastCalledWith(field(), 1)
    await user.keyboard('{Shift>}{Tab}{/Shift}')
    expect(onNavigate).toHaveBeenLastCalledWith(field(), -1)
  })

  // Walking a column by keyboard needs a ring to follow: the cell keeps the
  // app's 2 px :focus-visible outline, and a modeled cell keeps its warn tint
  // while it is being edited instead of looking like an empty one.
  it('keeps the global focus ring and the modeled tint while focused', () => {
    render(<QtyInput value={2} label={LABEL} onCommit={() => {}} />)
    expect(field().className).not.toMatch(/(^|\s)outline-none(\s|$)/)
    expect(field().className).not.toMatch(/focus:bg-/)
    expect(field().className).toContain('bg-transparent')
    expect(field().parentElement).toHaveClass('bg-warn/15')
  })

  it('explains a disabled cell', () => {
    render(
      <QtyInput
        value={0}
        label={LABEL}
        onCommit={() => {}}
        disabled
        disabledReason="No mark — this contract cannot be modeled"
      />,
    )
    expect(field()).toBeDisabled()
    expect(field().parentElement).toHaveAttribute('title', 'No mark — this contract cannot be modeled')
  })

  it('offers a clear glyph on a modeled cell that sets it to zero', async () => {
    const user = userEvent.setup()
    const onCommit = vi.fn()
    render(<Harness initial={-2} onCommit={onCommit} />)
    await user.click(screen.getByRole('button', { name: `Clear ${LABEL}` }))
    expect(onCommit).toHaveBeenCalledExactlyOnceWith(0)
    expect(screen.queryByRole('button', { name: `Clear ${LABEL}` })).not.toBeInTheDocument()
  })
})

describe('dragging a quantity to another cell', () => {
  const SOURCE = 'Model quantity, SPX 16 Oct 26 7500 put'
  const TARGET = 'Model quantity, SPX 16 Oct 26 7400 put'

  function Pair({ onDropFrom }: { onDropFrom: (from: string, qty: number) => void }) {
    return (
      <>
        <QtyInput value={-1} label={SOURCE} onCommit={() => {}} dragKey="SPX261016P7500" onDropFrom={onDropFrom} />
        <QtyInput value={0} label={TARGET} onCommit={() => {}} dragKey="SPX261016P7400" onDropFrom={onDropFrom} />
      </>
    )
  }

  it('tags the browser\'s text drag as a move and lets the drop target move the leg', () => {
    const onDropFrom = vi.fn()
    render(<Pair onDropFrom={onDropFrom} />)
    const dt = dataTransfer()
    fireEvent.dragStart(screen.getByRole('spinbutton', { name: SOURCE }), { dataTransfer: dt })
    expect(dt.effectAllowed).toBe('move')
    expect(dt.types).toContain(MODEL_DRAG_TYPE)

    const target = screen.getByRole('spinbutton', { name: TARGET })
    const over = fireEvent.dragOver(target, { dataTransfer: dt })
    // preventDefault on dragover is what makes the cell a drop target at all.
    expect(over).toBe(false)
    expect(target.parentElement).toHaveAttribute('data-drop-target', 'true')
    const dropped = fireEvent.drop(target, { dataTransfer: dt })
    // Ours, so the browser's own paste of the text is stopped too.
    expect(dropped).toBe(false)
    expect(onDropFrom).toHaveBeenCalledWith('SPX261016P7500', -1)
    expect(target.parentElement).not.toHaveAttribute('data-drop-target')
  })

  it('leaves a plain text drop to the browser and a drop on itself alone', () => {
    const onDropFrom = vi.fn()
    render(<Pair onDropFrom={onDropFrom} />)
    const text = dataTransfer()
    text.setData('text/plain', '-1')
    const target = screen.getByRole('spinbutton', { name: TARGET })
    expect(fireEvent.dragOver(target, { dataTransfer: text })).toBe(true)
    expect(fireEvent.drop(target, { dataTransfer: text })).toBe(true)
    expect(onDropFrom).not.toHaveBeenCalled()

    const own = dataTransfer()
    const source = screen.getByRole('spinbutton', { name: SOURCE })
    fireEvent.dragStart(source, { dataTransfer: own })
    fireEvent.drop(source, { dataTransfer: own })
    expect(onDropFrom).not.toHaveBeenCalled()
  })

  it('never commits what the browser did to the source text during a drag', async () => {
    const onCommit = vi.fn()
    render(
      <QtyInput value={-1} label={SOURCE} onCommit={onCommit} dragKey="SPX261016P7500" onDropFrom={() => {}} />,
    )
    const source = screen.getByRole('spinbutton', { name: SOURCE })
    fireEvent.dragStart(source, { dataTransfer: dataTransfer() })
    // A text drag's default cuts the selection from its source field.
    fireEvent.change(source, { target: { value: '' } })
    fireEvent.dragEnd(source)
    fireEvent.blur(source)
    expect(onCommit).not.toHaveBeenCalled()
    expect(source).toHaveValue(`${MINUS}1`)
  })

  it('is not draggable, and takes no drop, without a drag key', () => {
    const onCommit = vi.fn()
    render(<QtyInput value={0} label={TARGET} onCommit={onCommit} />)
    const dt = dataTransfer()
    dt.setData(MODEL_DRAG_TYPE, JSON.stringify({ from: 'SPX261016P7500', qty: -1 }))
    const target = screen.getByRole('spinbutton', { name: TARGET })
    expect(fireEvent.dragOver(target, { dataTransfer: dt })).toBe(true)
    expect(fireEvent.drop(target, { dataTransfer: dt })).toBe(true)
    expect(onCommit).not.toHaveBeenCalled()
  })
})
