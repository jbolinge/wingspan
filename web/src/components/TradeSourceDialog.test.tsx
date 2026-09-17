import { invoke } from '@tauri-apps/api/core'
import { screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { useState } from 'react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { mockRequest, resetMockTradeSource } from '../api/mock'
import type { TradeSourceResponse } from '../api/types'
import { resetPickedFile, usePickedFile } from '../lib/pickedFile'
import { renderWithClient, resetAppStore } from '../test/utils'
import { TradeSourceDialog } from './TradeSourceDialog'

/** A well-formed link the mock accepts (a placeholder id — never the real one). */
const GOOD_URL = 'https://docs.google.com/spreadsheets/d/1AbCdEfGhIjKlMnOpQrStUvWxYz0123456789abcd/edit'
const NOT_SHARED_URL = 'https://docs.google.com/spreadsheets/d/not-shared-id/edit'
const GOOD_CSV = '/home/trader/Trades/legs.csv'
const MISSING_COLUMNS_CSV = '/home/trader/Trades/missing-columns.csv'

async function openWith(): Promise<{ user: ReturnType<typeof userEvent.setup> }> {
  const user = userEvent.setup()
  renderWithClient(<TradeSourceDialog onClose={() => {}} />)
  await screen.findByRole('button', { name: 'Check' })
  return { user }
}

async function checkSheet(
  user: ReturnType<typeof userEvent.setup>,
  url: string,
  tab?: string,
): Promise<void> {
  await user.type(screen.getByLabelText('Sheet link'), url)
  if (tab !== undefined) {
    await user.clear(screen.getByLabelText('Tab'))
    await user.type(screen.getByLabelText('Tab'), tab)
  }
  await user.click(screen.getByRole('button', { name: 'Check' }))
}

/** Wraps the dialog behind an opener button, the way `TradeSourceControl` does. */
function Opener() {
  const [open, setOpen] = useState(false)
  return (
    <>
      <button type="button" onClick={() => setOpen(true)}>
        Open
      </button>
      {open ? <TradeSourceDialog onClose={() => setOpen(false)} /> : null}
    </>
  )
}

function enterTauri(): void {
  ;(window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ = {}
}

beforeEach(() => {
  resetAppStore()
  resetMockTradeSource()
  resetPickedFile()
  delete (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__
})

afterEach(() => {
  delete (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__
})

describe('TradeSourceDialog', () => {
  it('probing a good sheet link shows the summary, then Load & analyze commits it', async () => {
    const user = userEvent.setup()
    renderWithClient(<Opener />)
    await user.click(screen.getByRole('button', { name: 'Open' }))
    await screen.findByRole('button', { name: 'Check' })
    await checkSheet(user, GOOD_URL)

    const summary = await screen.findByRole('group', { name: 'Source summary' })
    expect(summary).toHaveTextContent('Legs')
    expect(summary).toHaveTextContent(/\d+ open \/ \d+ closed/)

    const loadButton = screen.getByRole('button', { name: 'Load & analyze' })
    await waitFor(() => expect(loadButton).toBeEnabled())
    await user.click(loadButton)

    expect(await screen.findByRole('status')).toHaveTextContent(
      /\d+ open \/ \d+ closed trades loaded from/,
    )
    const source = mockRequest('/trades/source') as TradeSourceResponse
    expect(source.kind).toBe('sheet')
    expect(source.tab).toBe('Legs')

    // The dialog closes itself once the confirmation has been shown.
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument(), {
      timeout: 3000,
    })
  })

  it('keeps Load & analyze disabled until the probe succeeds', async () => {
    const { user } = await openWith()
    expect(screen.getByRole('button', { name: 'Load & analyze' })).toBeDisabled()

    await checkSheet(user, GOOD_URL)

    await waitFor(() =>
      expect(screen.getByRole('button', { name: 'Load & analyze' })).toBeEnabled(),
    )
  })

  it('a probe alone changes nothing', async () => {
    const before = mockRequest('/trades/source') as TradeSourceResponse
    const { user } = await openWith()
    await checkSheet(user, GOOD_URL)
    await screen.findByRole('group', { name: 'Source summary' })

    const after = mockRequest('/trades/source') as TradeSourceResponse
    expect(after.kind).toBe(before.kind)
  })

  it('a sheet-not-shared failure renders the server sentence and the sharing hint', async () => {
    const { user } = await openWith()
    await checkSheet(user, NOT_SHARED_URL)

    const alert = await screen.findByRole('alert')
    expect(alert).toHaveTextContent(/not shared/)
    expect(screen.getByText(/General access → Anyone with the link → Viewer/)).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Load & analyze' })).toBeDisabled()
  })

  it('a sheet-tab-not-found failure renders the tab hint', async () => {
    const { user } = await openWith()
    await checkSheet(user, GOOD_URL, 'Trades')

    const alert = await screen.findByRole('alert')
    expect(alert).toHaveTextContent(/no tab called "Trades"/)
    expect(screen.getByText(/case-sensitive/)).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Load & analyze' })).toBeDisabled()
  })

  it('a probe reporting missing columns keeps Load & analyze disabled and explains why', async () => {
    const user = userEvent.setup()
    renderWithClient(<TradeSourceDialog onClose={() => {}} />)
    // The path input only exists outside Tauri (the default in this suite).
    await user.type(screen.getByLabelText('CSV path'), MISSING_COLUMNS_CSV)
    await user.click(screen.getByRole('button', { name: 'Go' }))

    const summary = await screen.findByRole('group', { name: 'Source summary' })
    expect(summary).toHaveTextContent('Trade, Side')

    const alert = await screen.findByRole('alert')
    expect(alert).toHaveTextContent('Wingspan cannot import this')
    expect(alert).toHaveTextContent('Trade, Side')
    expect(screen.getByRole('button', { name: 'Load & analyze' })).toBeDisabled()
  })

  it('the browser branch probes a typed CSV path', async () => {
    const user = userEvent.setup()
    renderWithClient(<TradeSourceDialog onClose={() => {}} />)

    expect(screen.queryByRole('button', { name: 'Choose folder…' })).not.toBeInTheDocument()
    const input = screen.getByLabelText('CSV path')
    await user.type(input, GOOD_CSV)
    await user.click(screen.getByRole('button', { name: 'Go' }))

    const summary = await screen.findByRole('group', { name: 'Source summary' })
    expect(summary).toHaveTextContent(GOOD_CSV)
    await waitFor(() =>
      expect(screen.getByRole('button', { name: 'Load & analyze' })).toBeEnabled(),
    )
  })

  it('hides the CSV path field inside Tauri, where "Pick a CSV file…" replaces it', async () => {
    enterTauri()
    renderWithClient(<TradeSourceDialog onClose={() => {}} />)

    expect(screen.queryByLabelText('CSV path')).not.toBeInTheDocument()
    expect(await screen.findByRole('button', { name: 'Pick a CSV file…' })).toBeEnabled()
  })

  it('Pick a CSV file… opens the OS chooser and probes what comes back', async () => {
    enterTauri()
    vi.mocked(invoke).mockReset()
    vi.mocked(invoke).mockResolvedValue({ path: GOOD_CSV })
    const user = userEvent.setup()
    renderWithClient(<TradeSourceDialog onClose={() => {}} />)

    await user.click(await screen.findByRole('button', { name: 'Pick a CSV file…' }))

    expect(vi.mocked(invoke).mock.calls[0]?.[0]).toBe('pick_csv_file')
    const summary = await screen.findByRole('group', { name: 'Source summary' })
    expect(summary).toHaveTextContent(GOOD_CSV)
  })

  /**
   * The reported-bug shape from the folder dialog, replayed for the CSV
   * chooser: the native chooser is a window of its own, so the dialog can be
   * closed (Escape, the backdrop, the ✕) while it is still up. The pick has
   * to survive that — `usePickedFile` parks it outside React,
   * `TradeSourceControl` reopens the dialog, and the file gets probed.
   */
  it('a pick that lands after the dialog closed still reaches the app', async () => {
    enterTauri()
    let answer: (picked: { path: string }) => void = () => {}
    vi.mocked(invoke).mockReset()
    vi.mocked(invoke).mockImplementation(async (cmd: string) => {
      if (cmd !== 'pick_csv_file') return null
      return new Promise((resolve) => {
        answer = resolve
      })
    })
    const user = userEvent.setup()
    renderWithClient(<Opener />)
    await user.click(screen.getByRole('button', { name: 'Open' }))
    await user.click(await screen.findByRole('button', { name: 'Pick a CSV file…' }))
    await waitFor(() => expect(invoke).toHaveBeenCalled())

    await user.click(screen.getByRole('button', { name: 'Close' }))
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument())
    answer({ path: GOOD_CSV })

    // Nothing is mounted to claim the pick until something reopens the
    // dialog — mirroring `TradeSourceControl`'s subscription to `usePickedFile`.
    // The mutation's own `onSuccess` (in `usePickCsvFile`) parks it
    // regardless, so give its promise chain a turn to flush.
    await waitFor(() => expect(usePickedFile.getState().pending).not.toBeNull())
  })

  /** The same, without the reopen: a remount claims the parked pick. */
  it('a pick claimed by a freshly mounted dialog is probed', async () => {
    enterTauri()
    usePickedFile.getState().receive({ kind: 'file', path: GOOD_CSV })
    renderWithClient(<TradeSourceDialog onClose={() => {}} />)

    const summary = await screen.findByRole('group', { name: 'Source summary' })
    expect(summary).toHaveTextContent(GOOD_CSV)
  })

  it('a chooser that fails says why, even after the dialog was reopened', async () => {
    enterTauri()
    usePickedFile.getState().receive({
      kind: 'error',
      detail: 'that location is not a file on this computer',
    })
    renderWithClient(<TradeSourceDialog onClose={() => {}} />)

    expect(await screen.findByRole('alert')).toHaveTextContent(
      'that location is not a file on this computer',
    )
    expect(screen.getByRole('button', { name: 'Load & analyze' })).toBeDisabled()
  })

  it('shows Load demo book, present but inert outside Tauri', async () => {
    await openWith()
    const button = screen.getByRole('button', { name: 'Load demo book' })
    expect(button).toBeInTheDocument()
    expect(button).toBeDisabled()
    expect(button).toHaveAttribute('title', 'Available in the Wingspan desktop app')
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
})
