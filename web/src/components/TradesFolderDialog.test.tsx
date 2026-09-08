import { invoke } from '@tauri-apps/api/core'
import { screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { useState } from 'react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { mockPickedRoot, mockRequest, resetMockTradesRoot } from '../api/mock'
import { queryKeys } from '../api/queries'
import type { TradesRootResponse } from '../api/types'
import { usePickedFolder } from '../lib/pickedFolder'
import { renderWithClient, resetAppStore } from '../test/utils'
import { TradesFolderDialog } from './TradesFolderDialog'
import { TradesRootControl } from './TradesRootControl'

const GOOD_PATH = '/home/trader/Archive/2025/Trades'
/** Its parent: a real pick, whose layout discovery finds one level down. */
const PARENT_PATH = '/home/trader/Archive/2025'
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

function enterTauri(): void {
  ;(window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ = {}
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

  it('confirming calls set_trades_root and the dialog closes', async () => {
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

  /**
   * The commit carries the folder the *user* picked, not the one discovery
   * settled on.
   *
   * The server's symlink-containment check is bounded by whichever path it is
   * given, so narrowing the path between the probe and the commit can turn a
   * folder that probed clean into a 422 on "Load & analyze" — for a link that
   * never left the folder the user chose.
   */
  it('commits the folder the user picked when the layout is one level down', async () => {
    const { user } = await openWith()
    await probe(user, PARENT_PATH)

    // The summary reports where the trades will be read from...
    const summary = await screen.findByRole('group', { name: 'Folder summary' })
    expect(summary).toHaveTextContent(GOOD_PATH)
    expect(screen.getByText(/the trades layout is one level down/)).toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: 'Load & analyze' }))
    await screen.findByRole('status')

    // ...but the pick is what was committed.
    expect(mockPickedRoot()).toBe(PARENT_PATH)
    const root = mockRequest('/trades/root') as TradesRootResponse
    expect(root.source).toBe('local')
    expect(root.root).toBe(GOOD_PATH)
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

  it('hides the path input inside Tauri, where the OS chooser replaces it', async () => {
    enterTauri()
    renderWithClient(<TradesFolderDialog onClose={() => {}} />)

    expect(screen.queryByLabelText('Folder path')).not.toBeInTheDocument()
    const choose = await screen.findByRole('button', { name: 'Choose folder…' })
    expect(choose).toBeEnabled()
  })

  it('Choose folder… opens the OS chooser and probes what comes back', async () => {
    enterTauri()
    vi.mocked(invoke).mockReset()
    vi.mocked(invoke).mockResolvedValue(GOOD_PATH)
    const user = userEvent.setup()
    renderWithClient(<TradesFolderDialog onClose={() => {}} />)

    await user.click(await screen.findByRole('button', { name: 'Choose folder…' }))

    expect(vi.mocked(invoke).mock.calls[0]?.[0]).toBe('pick_folder')
    const summary = await screen.findByRole('group', { name: 'Folder summary' })
    expect(summary).toHaveTextContent(GOOD_PATH)
    await waitFor(() =>
      expect(screen.getByRole('button', { name: 'Load & analyze' })).toBeEnabled(),
    )
  })

  /**
   * The reported bug: the native chooser is a window of its own, so the
   * dialog can be closed (Escape, the backdrop, the ✕) while it is still up.
   * The pick has to survive that — `usePickFolder` parks it outside React,
   * `TradesRootControl` reopens the dialog, and the folder gets probed.
   */
  it('a pick that lands after the dialog closed still reaches the app', async () => {
    enterTauri()
    let answer: (path: string) => void = () => {}
    vi.mocked(invoke).mockReset()
    vi.mocked(invoke).mockImplementation(async (cmd: string) => {
      // Only the desktop-only commands reach `invoke`; everything else the
      // dialog needs is served by the mock transport.
      if (cmd !== 'pick_folder') return null
      return new Promise<string>((resolve) => {
        answer = resolve
      })
    })
    const user = userEvent.setup()
    renderWithClient(<TradesRootControl />)

    await user.click(await screen.findByRole('button', { name: /^Trades folder:/ }))
    await user.click(await screen.findByRole('button', { name: 'Choose folder…' }))
    await waitFor(() => expect(invoke).toHaveBeenCalled())

    // The user closes the dialog while the chooser is still open, then picks.
    await user.click(screen.getByRole('button', { name: 'Close' }))
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument())
    answer(GOOD_PATH)

    const summary = await screen.findByRole('group', { name: 'Folder summary' })
    expect(summary).toHaveTextContent(GOOD_PATH)
    await waitFor(() =>
      expect(screen.getByRole('button', { name: 'Load & analyze' })).toBeEnabled(),
    )
  })

  /** The same, without the reopen: a remount claims the parked pick. */
  it('a pick claimed by a freshly mounted dialog is probed', async () => {
    enterTauri()
    usePickedFolder.getState().receive({ kind: 'folder', path: GOOD_PATH })
    renderWithClient(<TradesFolderDialog onClose={() => {}} />)

    const summary = await screen.findByRole('group', { name: 'Folder summary' })
    expect(summary).toHaveTextContent(GOOD_PATH)
  })

  it('a chooser that fails says why, even after the dialog was reopened', async () => {
    enterTauri()
    usePickedFolder.getState().receive({
      kind: 'error',
      detail: 'that location is not a folder on this computer',
    })
    renderWithClient(<TradesFolderDialog onClose={() => {}} />)

    expect(await screen.findByRole('alert')).toHaveTextContent(
      'that location is not a folder on this computer',
    )
    expect(screen.getByRole('button', { name: 'Load & analyze' })).toBeDisabled()
  })

  /** While the chooser is up, the two accidental exits are inert. */
  it('Escape and the backdrop do not close the dialog while the chooser is open', async () => {
    enterTauri()
    vi.mocked(invoke).mockReset()
    // A chooser that never answers: the dialog stays in `pick.isPending`.
    vi.mocked(invoke).mockImplementation(() => new Promise(() => {}))
    const user = userEvent.setup()
    renderWithClient(<Opener />)
    await user.click(screen.getByRole('button', { name: 'Open' }))
    await user.click(await screen.findByRole('button', { name: 'Choose folder…' }))
    await waitFor(() => expect(invoke).toHaveBeenCalled())

    await user.keyboard('{Escape}')
    expect(screen.getByRole('dialog')).toBeInTheDocument()

    // The backdrop is the dialog's parent element in the portal.
    const backdrop = screen.getByRole('dialog').parentElement as HTMLElement
    await user.click(backdrop)
    expect(screen.getByRole('dialog')).toBeInTheDocument()

    // ...but a deliberate close still works.
    await user.click(screen.getByRole('button', { name: 'Close' }))
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument())
  })

  it('a cancelled chooser changes nothing and says nothing', async () => {
    enterTauri()
    vi.mocked(invoke).mockReset()
    vi.mocked(invoke).mockResolvedValue(null)
    const user = userEvent.setup()
    renderWithClient(<TradesFolderDialog onClose={() => {}} />)

    await user.click(await screen.findByRole('button', { name: 'Choose folder…' }))
    await waitFor(() => expect(invoke).toHaveBeenCalled())

    expect(screen.queryByRole('group', { name: 'Folder summary' })).not.toBeInTheDocument()
    expect(screen.queryByRole('alert')).not.toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Load & analyze' })).toBeDisabled()
  })

  it('Load demo book is titled to say where it works, outside Tauri', async () => {
    await openWith()
    const button = screen.getByRole('button', { name: 'Load demo book' })
    expect(button).toBeDisabled()
    expect(button).toHaveAttribute('title', 'Available in the Wingspan desktop app')
  })

  it('Load demo book calls load_demo and shows the demo confirmation, inside Tauri', async () => {
    enterTauri()
    const demoRoot: TradesRootResponse = {
      root: '/opt/wingspan/fixtures/demo',
      source: 'demo',
      config_root: null,
      open_trades: 6,
      closed_trades: 2,
      watching: false,
      recent: [],
    }
    vi.mocked(invoke).mockReset()
    vi.mocked(invoke).mockImplementation(async (cmd: string) =>
      cmd === 'load_demo' ? demoRoot : null,
    )
    const user = userEvent.setup()
    renderWithClient(<TradesFolderDialog onClose={() => {}} />)

    const button = await screen.findByRole('button', { name: 'Load demo book' })
    expect(button).toBeEnabled()
    await user.click(button)

    expect(invoke).toHaveBeenCalledWith('load_demo', undefined)
    expect(await screen.findByRole('status')).toHaveTextContent(
      /the synthetic demo book, priced from fixtures/,
    )
  })

  it('offers Leave demo only once the demo book is the one loaded, and calls leave_demo', async () => {
    enterTauri()
    const localRoot: TradesRootResponse = {
      root: GOOD_PATH,
      source: 'local',
      config_root: null,
      open_trades: 3,
      closed_trades: 1,
      watching: true,
      recent: [],
    }
    vi.mocked(invoke).mockReset()
    vi.mocked(invoke).mockImplementation(async (cmd: string) =>
      cmd === 'leave_demo' ? localRoot : null,
    )
    const { client } = renderWithClient(<TradesFolderDialog onClose={() => {}} />)
    await screen.findByRole('button', { name: 'Choose folder…' })
    expect(screen.queryByRole('button', { name: 'Leave demo' })).not.toBeInTheDocument()

    const demoRoot: TradesRootResponse = {
      root: '/opt/wingspan/fixtures/demo',
      source: 'demo',
      config_root: null,
      open_trades: 6,
      closed_trades: 2,
      watching: false,
      recent: [],
    }
    client.setQueryData(queryKeys.tradesRoot, demoRoot)

    const leaveButton = await screen.findByRole('button', { name: 'Leave demo' })
    const user = userEvent.setup()
    await user.click(leaveButton)

    expect(invoke).toHaveBeenCalledWith('leave_demo', undefined)
  })
})
