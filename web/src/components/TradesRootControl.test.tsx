import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { act, render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it } from 'vitest'
import { setMockMode } from '../api/client'
import { resetMockTradesRoot } from '../api/mock'
import { queryKeys } from '../api/queries'
import type { TradesRootResponse } from '../api/types'
import { resetFirstRunPrompt } from '../lib/firstRun'
import { renderWithClient, resetAppStore } from '../test/utils'
import { TradesRootControl } from './TradesRootControl'

beforeEach(() => {
  resetAppStore()
  resetMockTradesRoot()
  resetFirstRunPrompt()
})

async function tooltipOf(button: HTMLElement): Promise<HTMLElement> {
  const user = userEvent.setup()
  await user.hover(button)
  return screen.findByRole('tooltip')
}

describe('TradesRootControl', () => {
  it('shows the loaded book by basename, with the path and source in the tooltip', async () => {
    renderWithClient(<TradesRootControl />)

    const button = await screen.findByRole('button', { name: 'Trades folder: Trades' })
    const tip = await tooltipOf(button)
    expect(tip).toHaveTextContent('/home/trader/workspace/tos-exports/Trades')
    expect(tip).toHaveTextContent('from the config file')
    expect(tip).toHaveTextContent(/\d+ open \/ \d+ closed/)
    expect(button).toHaveAttribute('aria-describedby', tip.id)
    // The chip replaced the old upload-age one, and is rendered for every
    // source — the configured book here is read on demand, not watched.
    expect(button).toHaveTextContent('not watching')
  })

  it('says so when the book is the demo one', async () => {
    const { client } = renderWithClient(<TradesRootControl />)
    await screen.findByRole('button', { name: 'Trades folder: Trades' })

    const demo: TradesRootResponse = {
      root: '/opt/wingspan/fixtures/demo',
      source: 'demo',
      config_root: null,
      open_trades: 6,
      closed_trades: 2,
      watching: false,
      recent: [],
    }
    act(() => client.setQueryData(queryKeys.tradesRoot, demo))

    const button = await screen.findByRole('button', { name: 'Trades folder: demo' })
    expect(await tooltipOf(button)).toHaveTextContent('demo book')
  })

  /**
   * The state a desktop install sits in before any folder has ever been
   * chosen: no folder anywhere. The control has to say that rather than
   * render an empty name or an em dash.
   */
  it('says nothing is loaded when the server has no book', async () => {
    const { client } = renderWithClient(<TradesRootControl />)
    await screen.findByRole('button', { name: 'Trades folder: Trades' })

    const empty: TradesRootResponse = {
      root: null,
      source: 'empty',
      config_root: null,
      open_trades: 0,
      closed_trades: 0,
      watching: false,
      recent: [],
    }
    act(() => client.setQueryData(queryKeys.tradesRoot, empty))

    const button = await screen.findByRole('button', { name: 'Trades folder: none' })
    const tip = await tooltipOf(button)
    expect(tip).toHaveTextContent('No trades loaded')
    expect(tip).toHaveTextContent('nothing loaded')
    expect(tip).toHaveTextContent('Click to choose a folder on this computer')
  })

  it('opens the folder dialog, then gives focus back when it closes', async () => {
    const user = userEvent.setup()
    renderWithClient(<TradesRootControl />)

    const button = await screen.findByRole('button', { name: 'Trades folder: Trades' })
    expect(button).toHaveAttribute('aria-haspopup', 'dialog')
    expect(button).toHaveAttribute('aria-expanded', 'false')

    await user.click(button)
    const dialog = await screen.findByRole('dialog', { name: 'Choose your trades folder' })
    expect(dialog).toBeInTheDocument()
    expect(button).toHaveAttribute('aria-expanded', 'true')

    await user.keyboard('{Escape}')
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument())
    expect(document.activeElement).toBe(button)
  })

  /**
   * The replaced feature, pinned the other way round: there is no route that
   * lists the server's own folders any more, but there *is* a way to choose
   * a folder on the computer running the SPA.
   */
  it('offers a way to choose a folder on this computer', async () => {
    const user = userEvent.setup()
    renderWithClient(<TradesRootControl />)

    await user.click(await screen.findByRole('button', { name: 'Trades folder: Trades' }))
    await screen.findByRole('dialog')

    expect(screen.getByLabelText('Folder path')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Go' })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Choose folder…' })).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Up to parent folder' })).not.toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Select' })).not.toBeInTheDocument()
  })

  it('a Clear that empties the book does not reopen the folder dialog', async () => {
    const { client } = renderWithClient(<TradesRootControl />)
    await screen.findByRole('button', { name: 'Trades folder: Trades' })
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument()

    const empty: TradesRootResponse = {
      root: null,
      source: 'empty',
      config_root: null,
      open_trades: 0,
      closed_trades: 0,
      watching: false,
      recent: [],
    }
    act(() => client.setQueryData(queryKeys.tradesRoot, empty))

    await screen.findByRole('button', { name: 'Trades folder: none' })
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument()
  })

  /**
   * A Windows-style path pasted by mistake (or a literal backslash inside a
   * Linux folder name — legal there, unlike `/`) must still boil down to a
   * short label: the old `lastIndexOf('/')` implementation finds no forward
   * slash at all in a purely backslash-delimited string, so `label` becomes
   * the *entire* path and the button's `truncate` class chops it visually
   * instead of showing a clean basename.
   */
  it('a path containing a backslash still shows its basename', async () => {
    const { client } = renderWithClient(<TradesRootControl />)
    await screen.findByRole('button', { name: 'Trades folder: Trades' })

    const backslashy: TradesRootResponse = {
      root: 'C:\\Users\\trader\\Backup',
      source: 'local',
      config_root: null,
      open_trades: 1,
      closed_trades: 0,
      chosen_at: '2026-09-03T00:00:00Z',
      watching: true,
      recent: [],
    }
    act(() => client.setQueryData(queryKeys.tradesRoot, backslashy))

    const button = await screen.findByRole('button', { name: 'Trades folder: Backup' })
    expect(button).toBeInTheDocument()
    expect(button).toHaveTextContent('watching')
    expect(button).not.toHaveTextContent('not watching')
  })

  /**
   * The one time the dialog opens itself: the first successful `trades_root`
   * of the process reports no book *and* no `[trades] root` to fall back on,
   * which is where a fresh desktop install starts. The seeded, never-stale
   * client stands in for that first result landing.
   */
  it('opens the folder dialog itself on a first run with no book at all', async () => {
    setMockMode('forced')
    const client = new QueryClient({
      defaultOptions: {
        queries: { retry: false, staleTime: Infinity, gcTime: 0, refetchOnWindowFocus: false },
      },
    })
    const empty: TradesRootResponse = {
      root: null,
      source: 'empty',
      config_root: null,
      open_trades: 0,
      closed_trades: 0,
      watching: false,
      recent: [],
    }
    client.setQueryData(queryKeys.tradesRoot, empty)

    render(
      <QueryClientProvider client={client}>
        <TradesRootControl />
      </QueryClientProvider>,
    )

    expect(
      await screen.findByRole('dialog', { name: 'Choose your trades folder' }),
    ).toBeInTheDocument()
  })

  /**
   * ...and only once per process: a second mount that still sees an empty
   * book (a reload of the same install, before anything was chosen) must not
   * pop the dialog again.
   */
  it('does not open itself a second time in the same process', async () => {
    setMockMode('forced')
    const empty: TradesRootResponse = {
      root: null,
      source: 'empty',
      config_root: null,
      open_trades: 0,
      closed_trades: 0,
      watching: false,
      recent: [],
    }
    const mount = () => {
      const client = new QueryClient({
        defaultOptions: {
          queries: { retry: false, staleTime: Infinity, gcTime: 0, refetchOnWindowFocus: false },
        },
      })
      client.setQueryData(queryKeys.tradesRoot, empty)
      return render(
        <QueryClientProvider client={client}>
          <TradesRootControl />
        </QueryClientProvider>,
      )
    }

    const first = mount()
    await screen.findByRole('dialog', { name: 'Choose your trades folder' })
    first.unmount()

    mount()
    await screen.findByRole('button', { name: 'Trades folder: none' })
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument()
  })
})
