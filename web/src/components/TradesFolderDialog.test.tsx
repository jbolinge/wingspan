import { screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { useState } from 'react'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { mockRequest, resetMockTradesRoot } from '../api/mock'
import type { TradesRootResponse } from '../api/types'
import { renderWithClient, resetAppStore } from '../test/utils'
import { TradesFolderDialog } from './TradesFolderDialog'

const GOOD_PATH = '/home/trader/Archive/2025/Trades'
const BAD_PATH = '/mnt/nowhere/nothing/here'

async function openWith(): Promise<{ user: ReturnType<typeof userEvent.setup> }> {
  const user = userEvent.setup()
  renderWithClient(<TradesFolderDialog onClose={() => {}} />)
  await screen.findByRole('button', { name: 'Choose folder…' })
  return { user }
}

async function probe(user: ReturnType<typeof userEvent.setup>, path: string): Promise<void> {
  await user.type(screen.getByLabelText('Folder path'), path)
  await user.click(screen.getByRole('button', { name: 'Go' }))
}

/** Wraps the dialog behind an opener button, the way `TradesRootControl` does. */
function Opener() {
  const [open, setOpen] = useState(false)
  return (
    <>
      <button type="button" onClick={() => setOpen(true)}>
        Open
      </button>
      {open ? <TradesFolderDialog onClose={() => setOpen(false)} /> : null}
    </>
  )
}

beforeEach(() => {
  resetAppStore()
  resetMockTradesRoot()
  delete (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__
})

afterEach(() => {
  delete (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__
})

describe('TradesFolderDialog', () => {
  it('probing a good path shows the summary', async () => {
    const { user } = await openWith()
    await probe(user, GOOD_PATH)

    const summary = await screen.findByRole('group', { name: 'Folder summary' })
    expect(summary).toHaveTextContent(GOOD_PATH)
    expect(summary).toHaveTextContent(/\d+ open \/ \d+ closed/)
    // Exact cells, not a substring of the whole grid: the mock reports three
    // statements, and `Watching` reflects the book loaded *now* — a probe has
    // not started watching anything.
    expect(within(summary).getByText('3')).toBeInTheDocument()
    expect(within(summary).getByText('no')).toBeInTheDocument()
  })

  it('keeps Load & analyze disabled until the probe succeeds', async () => {
    const { user } = await openWith()
    expect(screen.getByRole('button', { name: 'Load & analyze' })).toBeDisabled()

    await probe(user, GOOD_PATH)

    await waitFor(() =>
      expect(screen.getByRole('button', { name: 'Load & analyze' })).toBeEnabled(),
    )
  })

  it('confirming calls PUT /trades/root and the dialog closes', async () => {
    const user = userEvent.setup()
    renderWithClient(<Opener />)
    await user.click(screen.getByRole('button', { name: 'Open' }))
    await screen.findByRole('button', { name: 'Choose folder…' })
    await probe(user, GOOD_PATH)
    await waitFor(() =>
      expect(screen.getByRole('button', { name: 'Load & analyze' })).toBeEnabled(),
    )

    await user.click(screen.getByRole('button', { name: 'Load & analyze' }))

    expect(await screen.findByRole('status')).toHaveTextContent(
      /\d+ open \/ \d+ closed trades loaded from/,
    )
    const root = mockRequest('/trades/root') as TradesRootResponse
    expect(root.source).toBe('local')
    expect(root.root).toBe(GOOD_PATH)
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument(), {
      timeout: 3000,
    })
  })

  it('a probe alone changes nothing', async () => {
    const before = mockRequest('/trades/root') as TradesRootResponse
    expect(before.source).toBe('config')

    const { user } = await openWith()
    await probe(user, GOOD_PATH)
    await screen.findByRole('group', { name: 'Folder summary' })

    const after = mockRequest('/trades/root') as TradesRootResponse
    expect(after.source).toBe('config')
    expect(after.root).toBe(before.root)
  })

  it('renders a 422 discovery sentence inline', async () => {
    const { user } = await openWith()
    await probe(user, BAD_PATH)

    const alert = await screen.findByRole('alert')
    expect(alert).toHaveTextContent(`no trades folders under "${BAD_PATH}"`)
    expect(alert).toHaveTextContent('looked for "Open Trades"')
    expect(screen.getByRole('button', { name: 'Load & analyze' })).toBeDisabled()
  })

  it('renders the recent list and probes a clicked entry', async () => {
    mockRequest('/trades/root', { method: 'PUT', body: JSON.stringify({ root: GOOD_PATH }) })
    mockRequest('/trades/root', { method: 'DELETE' })
    expect((mockRequest('/trades/root') as TradesRootResponse).recent).toContain(GOOD_PATH)

    await openWith()
    const entry = await screen.findByRole('button', { name: GOOD_PATH })
    await userEvent.setup().click(entry)

    const summary = await screen.findByRole('group', { name: 'Folder summary' })
    expect(summary).toHaveTextContent(GOOD_PATH)
  })

  it('shows Load demo book, present but inert', async () => {
    await openWith()
    const button = screen.getByRole('button', { name: 'Load demo book' })
    expect(button).toBeInTheDocument()
    expect(button).toBeDisabled()
  })

  it('Escape closes and restores focus to the opener', async () => {
    const user = userEvent.setup()
    renderWithClient(<Opener />)
    const opener = screen.getByRole('button', { name: 'Open' })

    await user.click(opener)
    await screen.findByRole('dialog')

    await user.keyboard('{Escape}')
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument())
    expect(document.activeElement).toBe(opener)
  })

  it('renders the path input outside Tauri, and hides it inside Tauri', async () => {
    await openWith()
    expect(screen.getByLabelText('Folder path')).toBeInTheDocument()
  })

  it('hides the path input inside Tauri and disables Choose folder…', async () => {
    ;(window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ = {}
    renderWithClient(<TradesFolderDialog onClose={() => {}} />)

    expect(screen.queryByLabelText('Folder path')).not.toBeInTheDocument()
    const choose = await screen.findByRole('button', { name: 'Choose folder…' })
    expect(choose).toBeDisabled()
  })
})
