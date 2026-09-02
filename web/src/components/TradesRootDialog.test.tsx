import { screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { setMockMode } from '../api/client'
import { MOCK_TRADE_IDS, mockRequest, resetMockTradesRoot } from '../api/mock'
import type { TradesRootResponse } from '../api/types'
import { useAppStore } from '../store/appStore'
import { renderWithClient, resetAppStore } from '../test/utils'
import { TradesRootDialog } from './TradesRootDialog'

const CONFIG_ROOT: TradesRootResponse = {
  root: '/home/trader/workspace/tos-exports/Trades',
  source: 'config',
  config_root: '/home/trader/workspace/tos-exports/Trades',
  open_trades: 4,
  closed_trades: 2,
}

const OVERRIDE_ROOT: TradesRootResponse = {
  ...CONFIG_ROOT,
  root: '/mnt/backup/trades-2025',
  source: 'override',
}

function list(): HTMLElement {
  return screen.getByRole('list', { name: 'Folders' })
}

/** What the footer says Select would send. */
function selected(): string | null {
  return screen.getByText('Selected').nextElementSibling?.textContent ?? null
}

beforeEach(() => {
  resetAppStore()
  resetMockTradesRoot()
})

describe('TradesRootDialog', () => {
  it('opens as an accessible modal on the active root’s parent', async () => {
    renderWithClient(<TradesRootDialog root={CONFIG_ROOT} onClose={vi.fn()} />)

    const dialog = await screen.findByRole('dialog')
    expect(dialog).toHaveAttribute('aria-modal', 'true')
    expect(within(dialog).getByRole('heading', { name: 'Trades folder' })).toBeInTheDocument()
    // `path` omitted → the server opens on the active root's parent.
    await waitFor(() =>
      expect(screen.getByLabelText('Folder path')).toHaveValue(
        '/home/trader/workspace/tos-exports',
      ),
    )
    expect(document.activeElement).toBe(screen.getByLabelText('Folder path'))
  })

  it('badges only the folders whose discovery probe succeeds', async () => {
    renderWithClient(<TradesRootDialog root={CONFIG_ROOT} onClose={vi.fn()} />)

    const trades = await screen.findByRole('button', { name: /^Trades\b/ })
    expect(within(trades).getByText('contains trades layout')).toBeInTheDocument()
    const archive = screen.getByRole('button', { name: /^archive$/ })
    expect(within(archive).queryByText('contains trades layout')).not.toBeInTheDocument()
  })

  it('hides dot-directories until the toggle is pressed', async () => {
    const user = userEvent.setup()
    renderWithClient(<TradesRootDialog root={CONFIG_ROOT} onClose={vi.fn()} />)

    await screen.findByRole('button', { name: /^Trades\b/ })
    expect(screen.queryByRole('button', { name: /^\.git$/ })).not.toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: 'Hidden' }))
    expect(await screen.findByRole('button', { name: /^\.git$/ })).toBeInTheDocument()
  })

  it('descends on double-click and climbs back through the parent row', async () => {
    const user = userEvent.setup()
    renderWithClient(<TradesRootDialog root={CONFIG_ROOT} onClose={vi.fn()} />)

    await user.dblClick(await screen.findByRole('button', { name: /^Trades\b/ }))
    await waitFor(() =>
      expect(screen.getByLabelText('Folder path')).toHaveValue(
        '/home/trader/workspace/tos-exports/Trades',
      ),
    )
    expect(await within(list()).findByRole('button', { name: /Open Trades/ })).toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: 'Up to parent folder' }))
    await waitFor(() =>
      expect(screen.getByLabelText('Folder path')).toHaveValue(
        '/home/trader/workspace/tos-exports',
      ),
    )
  })

  it('descends when Enter is pressed on a focused folder row', async () => {
    const user = userEvent.setup()
    renderWithClient(<TradesRootDialog root={CONFIG_ROOT} onClose={vi.fn()} />)

    const trades = await screen.findByRole('button', { name: /^Trades\b/ })
    trades.focus()
    await user.keyboard('{Enter}')
    await waitFor(() =>
      expect(screen.getByLabelText('Folder path')).toHaveValue(
        '/home/trader/workspace/tos-exports/Trades',
      ),
    )
  })

  it('jumps to the typed path, and to home', async () => {
    const user = userEvent.setup()
    renderWithClient(<TradesRootDialog root={CONFIG_ROOT} onClose={vi.fn()} />)

    const field = await screen.findByLabelText('Folder path')
    await waitFor(() => expect(field).toHaveValue('/home/trader/workspace/tos-exports'))
    await user.clear(field)
    await user.type(field, '/mnt/backup{Enter}')
    expect(await screen.findByRole('button', { name: /^trades-2025\b/ })).toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: '⌂ Home' }))
    await waitFor(() => expect(screen.getByLabelText('Folder path')).toHaveValue('/home/trader'))
    expect(screen.getByRole('button', { name: /^workspace$/ })).toBeInTheDocument()
  })

  it('renders a problem+json listing failure without leaving the dialog', async () => {
    const user = userEvent.setup()
    renderWithClient(<TradesRootDialog root={CONFIG_ROOT} onClose={vi.fn()} />)

    const field = await screen.findByLabelText('Folder path')
    await waitFor(() => expect(field).toHaveValue('/home/trader/workspace/tos-exports'))
    await user.clear(field)
    await user.type(field, '/does/not/exist{Enter}')

    const alert = await screen.findByRole('alert')
    expect(alert).toHaveTextContent('cannot read /does/not/exist: no such directory')
    expect(screen.getByRole('dialog')).toBeInTheDocument()
  })

  it('still offers Home as the way out of a folder that does not exist', async () => {
    const user = userEvent.setup()
    renderWithClient(<TradesRootDialog root={CONFIG_ROOT} onClose={vi.fn()} />)

    const field = await screen.findByLabelText('Folder path')
    await waitFor(() => expect(field).toHaveValue('/home/trader/workspace/tos-exports'))
    await user.clear(field)
    await user.type(field, '/does/not/exist{Enter}')
    await screen.findByRole('alert')

    // The failed listing carries no `home` of its own; the last one still does.
    await user.click(screen.getByRole('button', { name: '⌂ Home' }))
    await waitFor(() => expect(field).toHaveValue('/home/trader'))
    expect(await screen.findByRole('button', { name: /^workspace$/ })).toBeInTheDocument()
  })

  it('renders the 422 discovery sentence inline and stays open', async () => {
    const user = userEvent.setup()
    const onClose = vi.fn()
    renderWithClient(<TradesRootDialog root={CONFIG_ROOT} onClose={onClose} />)

    const field = await screen.findByLabelText('Folder path')
    await waitFor(() => expect(field).toHaveValue('/home/trader/workspace/tos-exports'))
    await user.clear(field)
    await user.type(field, '/home/trader/Downloads{Enter}')
    await waitFor(() =>
      expect(screen.getByLabelText('Folder path')).toHaveValue('/home/trader/Downloads'),
    )

    await user.click(screen.getByRole('button', { name: 'Select' }))

    const alert = await screen.findByRole('alert')
    expect(alert).toHaveTextContent(
      'no trades folders under /home/trader/Downloads (looked for "Open Trades", "Closed Trades" or "Daily Trade Statement", and one level down)',
    )
    expect(screen.getByRole('dialog')).toBeInTheDocument()
    expect(onClose).not.toHaveBeenCalled()
  })

  it('confirms the reload, then closes, then drops trades the new root lacks', async () => {
    const user = userEvent.setup()
    const onClose = vi.fn()
    useAppStore.getState().setSelected([MOCK_TRADE_IDS[0] as string, 'gone-2019-01-01'])
    renderWithClient(<TradesRootDialog root={CONFIG_ROOT} onClose={onClose} />)

    await user.click(await screen.findByRole('button', { name: /^Trades\b/ }))
    await user.click(screen.getByRole('button', { name: 'Select' }))

    const status = await screen.findByRole('status')
    expect(status).toHaveTextContent(/\d+ open \/ \d+ closed trades loaded/)
    expect(status).toHaveTextContent('/home/trader/workspace/tos-exports/Trades')

    await waitFor(() => expect(onClose).toHaveBeenCalled(), { timeout: 3000 })
    await waitFor(() =>
      expect(useAppStore.getState().selected).toEqual([MOCK_TRADE_IDS[0] as string]),
    )
    // The choice reached the server, not just the cache.
    expect((mockRequest('/trades/root') as TradesRootResponse).source).toBe('override')
  })

  it('offers Reset to config only while an override is active', async () => {
    const user = userEvent.setup()
    const { rerender } = renderWithClient(
      <TradesRootDialog root={CONFIG_ROOT} onClose={vi.fn()} />,
    )
    await screen.findByRole('dialog')
    expect(screen.queryByRole('button', { name: 'Reset to config' })).not.toBeInTheDocument()

    rerender(<TradesRootDialog root={OVERRIDE_ROOT} onClose={vi.fn()} />)
    await user.click(screen.getByRole('button', { name: 'Reset to config' }))
    expect(await screen.findByRole('status')).toHaveTextContent(/trades loaded/)
  })

  /// A listing that is still loading is the *previous* folder's: it keeps the
  /// list from flashing empty, but Select must go by what was asked for, or
  /// clicking it during the round trip re-picks the folder just left.
  it('never offers the folder it navigated away from while the next listing loads', async () => {
    const user = userEvent.setup()
    renderWithClient(<TradesRootDialog root={CONFIG_ROOT} onClose={vi.fn()} />)
    const field = await screen.findByLabelText('Folder path')
    await waitFor(() => expect(field).toHaveValue('/home/trader/workspace/tos-exports'))
    expect(selected()).toBe('/home/trader/workspace/tos-exports')

    // Freeze the next listing in flight.
    const inFlight = vi.fn(() => new Promise<Response>(() => {}))
    setMockMode('off')
    vi.stubGlobal('fetch', inFlight)
    try {
      await user.clear(field)
      await user.type(field, '/mnt/backup{Enter}')
      await waitFor(() => expect(inFlight).toHaveBeenCalled())

      await waitFor(() => expect(list()).toHaveAttribute('aria-busy', 'true'))
      expect(selected()).toBe('/mnt/backup')
      expect(field).toHaveValue('/mnt/backup')
    } finally {
      vi.unstubAllGlobals()
      setMockMode('forced')
    }
  })

  it('does not leave a failed selection’s error next to the reset confirmation', async () => {
    const user = userEvent.setup()
    renderWithClient(<TradesRootDialog root={OVERRIDE_ROOT} onClose={vi.fn()} />)

    const field = await screen.findByLabelText('Folder path')
    await waitFor(() => expect(field).toHaveValue('/home/trader/workspace/tos-exports'))
    await user.clear(field)
    await user.type(field, '/home/trader/Downloads{Enter}')
    await waitFor(() => expect(field).toHaveValue('/home/trader/Downloads'))
    await user.click(screen.getByRole('button', { name: 'Select' }))
    expect(await screen.findByRole('alert')).toHaveTextContent('no trades folders under')

    await user.click(screen.getByRole('button', { name: 'Reset to config' }))
    expect(await screen.findByRole('status')).toHaveTextContent(/trades loaded/)
    expect(screen.queryByRole('alert')).not.toBeInTheDocument()
  })

  it('closes on Escape and restores focus to the opener', async () => {
    const user = userEvent.setup()
    const onClose = vi.fn()
    const opener = document.createElement('button')
    document.body.append(opener)
    opener.focus()

    const { unmount } = renderWithClient(<TradesRootDialog root={CONFIG_ROOT} onClose={onClose} />)
    await screen.findByRole('dialog')
    await user.keyboard('{Escape}')
    expect(onClose).toHaveBeenCalledTimes(1)

    unmount()
    expect(document.activeElement).toBe(opener)
    opener.remove()
  })

  it('keeps Tab inside the dialog', async () => {
    const user = userEvent.setup()
    renderWithClient(<TradesRootDialog root={CONFIG_ROOT} onClose={vi.fn()} />)

    const dialog = await screen.findByRole('dialog')
    await screen.findByRole('button', { name: /^Trades\b/ })
    for (let i = 0; i < 25; i += 1) {
      await user.tab()
      expect(dialog.contains(document.activeElement)).toBe(true)
    }
  })
})
