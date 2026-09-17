import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { act, render, screen, waitFor } from '@testing-library/react'
import { beforeEach, describe, expect, it } from 'vitest'
import { setMockMode } from '../api/client'
import { resetMockTradeSource } from '../api/mock'
import { queryKeys } from '../api/queries'
import type { TradeSourceResponse } from '../api/types'
import { resetFirstRunPrompt } from '../lib/firstRun'
import { resetPickedFile, usePickedFile } from '../lib/pickedFile'
import { useUiSignals } from '../lib/uiSignals'
import { renderWithClient, resetAppStore } from '../test/utils'
import { TradeSourceControl } from './TradeSourceControl'

const EMPTY: TradeSourceResponse = {
  kind: 'empty',
  label: null,
  tab: null,
  path: null,
  open_trades: 0,
  closed_trades: 0,
  legs: 0,
  rows_skipped: 0,
  chosen_at: null,
  fetched_at: null,
  stale: false,
  detail: null,
  auto_refresh_secs: null,
  recent: [],
}

const SHEET: TradeSourceResponse = {
  kind: 'sheet',
  label: 'docs.google.com/…/1AbCdE…abcd · Legs',
  tab: 'Legs',
  path: null,
  open_trades: 6,
  closed_trades: 2,
  legs: 20,
  rows_skipped: 0,
  chosen_at: '2026-09-01T00:00:00Z',
  fetched_at: '2026-09-01T00:00:00Z',
  stale: false,
  detail: null,
  auto_refresh_secs: null,
  recent: [],
}

const CSV: TradeSourceResponse = {
  kind: 'csv',
  label: '/home/trader/Trades/legs.csv',
  tab: null,
  path: '/home/trader/Trades/legs.csv',
  open_trades: 3,
  closed_trades: 1,
  legs: 8,
  rows_skipped: 0,
  chosen_at: '2026-09-01T00:00:00Z',
  fetched_at: '2026-09-01T00:00:00Z',
  stale: false,
  detail: null,
  auto_refresh_secs: null,
  recent: [],
}

beforeEach(() => {
  resetAppStore()
  resetMockTradeSource()
  resetFirstRunPrompt()
  resetPickedFile()
  useUiSignals.setState({ last: null })
})

describe('TradeSourceControl', () => {
  it('shows the demo book by default (nothing imported yet, in the mock)', async () => {
    renderWithClient(<TradeSourceControl />)
    expect(
      await screen.findByRole('button', { name: 'Trade source: demo book' }),
    ).toBeInTheDocument()
  })

  it('shows the already-masked label for a sheet, unchanged', async () => {
    const { client } = renderWithClient(<TradeSourceControl />)
    await screen.findByRole('button', { name: 'Trade source: demo book' })

    act(() => client.setQueryData(queryKeys.tradeSource, SHEET))

    expect(
      await screen.findByRole('button', { name: `Trade source: ${SHEET.label}` }),
    ).toBeInTheDocument()
  })

  it('shows the basename of a CSV path', async () => {
    const { client } = renderWithClient(<TradeSourceControl />)
    await screen.findByRole('button', { name: 'Trade source: demo book' })

    act(() => client.setQueryData(queryKeys.tradeSource, CSV))

    expect(
      await screen.findByRole('button', { name: 'Trade source: legs.csv' }),
    ).toBeInTheDocument()
  })

  // A deployer's `[trades] sheet_url` answers `kind: "config"`, not
  // `"sheet"` — and a deployer's `[trades] csv_path` answers the same
  // `"config"`. Only `path` tells the two apart, which is why the label is
  // chosen on `path`: reading `kind === 'sheet'` sent a configured sheet
  // down the file branch, where `basename` chopped the masked label at its
  // last slash and the button read "1AbCdE…abcd · Legs".
  it('shows a configured sheet as a sheet, not as a chopped path', async () => {
    const { client } = renderWithClient(<TradeSourceControl />)
    await screen.findByRole('button', { name: 'Trade source: demo book' })

    act(() => client.setQueryData(queryKeys.tradeSource, { ...SHEET, kind: 'config' }))

    const button = await screen.findByRole('button', { name: `Trade source: ${SHEET.label}` })
    expect(button).toHaveAttribute(
      'title',
      expect.stringContaining('Google Sheet set in the config file'),
    )
  })

  it('shows a configured CSV as a file, with the config-file wording', async () => {
    const { client } = renderWithClient(<TradeSourceControl />)
    await screen.findByRole('button', { name: 'Trade source: demo book' })

    act(() => client.setQueryData(queryKeys.tradeSource, { ...CSV, kind: 'config' }))

    const button = await screen.findByRole('button', { name: 'Trade source: legs.csv' })
    expect(button).toHaveAttribute(
      'title',
      expect.stringContaining('CSV file set in the config file'),
    )
  })

  it('shows none, with "import trades…" as the visible text, when nothing is imported', async () => {
    const { client } = renderWithClient(<TradeSourceControl />)
    await screen.findByRole('button', { name: 'Trade source: demo book' })

    act(() => client.setQueryData(queryKeys.tradeSource, EMPTY))

    const button = await screen.findByRole('button', { name: 'Trade source: none' })
    expect(button).toHaveTextContent('import trades…')
  })

  it('shows the cached chip only while stale, with the age', async () => {
    const { client } = renderWithClient(<TradeSourceControl />)
    await screen.findByRole('button', { name: 'Trade source: demo book' })

    act(() => client.setQueryData(queryKeys.tradeSource, SHEET))
    const button = await screen.findByRole('button', { name: `Trade source: ${SHEET.label}` })
    expect(button).not.toHaveTextContent('cached')

    act(() =>
      client.setQueryData(queryKeys.tradeSource, {
        ...SHEET,
        stale: true,
        fetched_at: new Date(Date.now() - 10_000_000).toISOString(),
      }),
    )
    await waitFor(() => expect(button).toHaveTextContent(/cached · \d/))
  })

  it('omits the age half of the chip when fetched_at cannot be parsed', async () => {
    const { client } = renderWithClient(<TradeSourceControl />)
    await screen.findByRole('button', { name: 'Trade source: demo book' })

    act(() =>
      client.setQueryData(queryKeys.tradeSource, { ...SHEET, stale: true, fetched_at: null }),
    )

    const button = await screen.findByRole('button', { name: `Trade source: ${SHEET.label}` })
    expect(button).toHaveTextContent('cached')
    expect(button).not.toHaveTextContent('cached ·')
  })

  it('opens the dialog when the menu bridge emits open-source-dialog', async () => {
    renderWithClient(<TradeSourceControl />)
    await screen.findByRole('button', { name: 'Trade source: demo book' })
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument()

    act(() => useUiSignals.getState().emit('open-source-dialog'))

    expect(await screen.findByRole('dialog', { name: 'Import trades' })).toBeInTheDocument()
  })

  it('reopens the dialog for a CSV pick parked outside React', async () => {
    renderWithClient(<TradeSourceControl />)
    await screen.findByRole('button', { name: 'Trade source: demo book' })
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument()

    act(() => usePickedFile.getState().receive({ kind: 'file', path: '/tmp/legs.csv' }))

    expect(await screen.findByRole('dialog', { name: 'Import trades' })).toBeInTheDocument()
  })

  /**
   * The one time the dialog opens itself: the first successful `trade_source`
   * result of the process reports nothing imported, which is where a fresh
   * desktop install starts. The seeded, never-stale client stands in for
   * that first result landing.
   */
  it('opens the import dialog itself on a first run with nothing imported', async () => {
    setMockMode('forced')
    const client = new QueryClient({
      defaultOptions: {
        queries: { retry: false, staleTime: Infinity, gcTime: 0, refetchOnWindowFocus: false },
      },
    })
    client.setQueryData(queryKeys.tradeSource, EMPTY)

    render(
      <QueryClientProvider client={client}>
        <TradeSourceControl />
      </QueryClientProvider>,
    )

    expect(await screen.findByRole('dialog', { name: 'Import trades' })).toBeInTheDocument()
  })

  /**
   * ...and only once per process: a second mount that still sees nothing
   * imported (a reload of the same install, before anything was chosen) must
   * not pop the dialog again.
   */
  it('does not open itself a second time in the same process', async () => {
    setMockMode('forced')
    const mount = () => {
      const client = new QueryClient({
        defaultOptions: {
          queries: { retry: false, staleTime: Infinity, gcTime: 0, refetchOnWindowFocus: false },
        },
      })
      client.setQueryData(queryKeys.tradeSource, EMPTY)
      return render(
        <QueryClientProvider client={client}>
          <TradeSourceControl />
        </QueryClientProvider>,
      )
    }

    const first = mount()
    await screen.findByRole('dialog', { name: 'Import trades' })
    first.unmount()

    mount()
    await screen.findByRole('button', { name: 'Trade source: none' })
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument()
  })

  it('a Clear that empties the book does not reopen the import dialog', async () => {
    const { client } = renderWithClient(<TradeSourceControl />)
    await screen.findByRole('button', { name: 'Trade source: demo book' })
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument()

    act(() => client.setQueryData(queryKeys.tradeSource, EMPTY))

    await screen.findByRole('button', { name: 'Trade source: none' })
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument()
  })
})
