/**
 * The native menu bridge. `listen()` is mocked exactly like `api/events.test.ts`
 * does: capture the handler `useMenuBridge` registers for the `menu` event,
 * then fire it with each id and assert on the store, `call`, and the query
 * cache — never on the real Tauri IPC.
 */

import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { listen } from '@tauri-apps/api/event'
import { act, renderHook } from '@testing-library/react'
import { createElement, type ReactNode } from 'react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { call } from '../api/client'
import { queryKeys } from '../api/queries'
import type { TradeSourceResponse, TradesResponse } from '../api/types'
import { useAppStore } from '../store/appStore'
import { makeGreeks } from '../test/factories'
import { resetAppStore } from '../test/utils'
import { useMenuBridge } from './menu'
import { useUiSignals } from './uiSignals'

// Wraps the real implementation so a test can drive `call` per command while
// still exercising the real `adoptNewRoot` (imported by `menu.ts`, not this
// file) unaffected.
vi.mock('../api/client', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../api/client')>()
  return { ...actual, call: vi.fn(actual.call) }
})

type Handler = (event: { payload: string }) => void
let handler: Handler | null = null
const unlisten = vi.fn()

const EMPTY_TRADES: TradesResponse = {
  as_of: '2026-01-01T00:00:00Z',
  trades: [],
  portfolio_all: {
    count: 0,
    pnl_open: 0,
    pnl_total: 0,
    greeks: makeGreeks(),
    weighted_vega: 0,
    weighted_vega_reference: 'front_expiry',
  },
  warning_count: 0,
}

const CSV_SOURCE: TradeSourceResponse = {
  kind: 'csv',
  label: '/home/trader/Downloads/legs.csv',
  tab: null,
  path: '/home/trader/Downloads/legs.csv',
  open_trades: 1,
  closed_trades: 0,
  legs: 3,
  rows_skipped: 0,
  chosen_at: '2026-09-01T00:00:00Z',
  fetched_at: '2026-09-01T00:00:00Z',
  stale: false,
  detail: null,
  auto_refresh_secs: null,
  recent: [],
}

const SHEET_SOURCE: TradeSourceResponse = {
  kind: 'sheet',
  label: 'docs.google.com/…/1AbCdE…abcd · Legs',
  tab: 'Legs',
  path: null,
  open_trades: 6,
  closed_trades: 2,
  legs: 39,
  rows_skipped: 0,
  chosen_at: '2026-09-01T00:00:00Z',
  fetched_at: '2026-09-01T00:00:00Z',
  stale: false,
  detail: null,
  auto_refresh_secs: null,
  recent: [],
}

const DEMO_SOURCE: TradeSourceResponse = {
  kind: 'demo',
  label: 'fixtures/demo/legs.csv',
  tab: null,
  path: 'fixtures/demo/legs.csv',
  open_trades: 6,
  closed_trades: 2,
  legs: 39,
  rows_skipped: 0,
  chosen_at: null,
  fetched_at: '2026-09-01T00:00:00Z',
  stale: false,
  detail: null,
  auto_refresh_secs: null,
  recent: [],
}

function enterTauri(): void {
  ;(window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {}
}
function leaveTauri(): void {
  delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__
}

function harness(): { client: QueryClient; wrapper: ({ children }: { children: ReactNode }) => ReactNode } {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  const wrapper = ({ children }: { children: ReactNode }) =>
    createElement(QueryClientProvider, { client }, children)
  return { client, wrapper }
}

/** Let the `listen()` promise, and any awaited work a fired handler kicks
 *  off (a mutation, `adoptNewRoot`'s own fetch), settle. */
async function settle(): Promise<void> {
  for (let i = 0; i < 6; i += 1) {
    await act(async () => {
      await Promise.resolve()
    })
  }
}

function fire(id: string): void {
  handler?.({ payload: id })
}

beforeEach(() => {
  resetAppStore()
  useUiSignals.setState({ last: null })
  enterTauri()
  handler = null
  unlisten.mockReset()
  vi.mocked(listen).mockReset()
  vi.mocked(listen).mockImplementation((event: string, h: unknown) => {
    if (event === 'menu') handler = h as Handler
    return Promise.resolve(unlisten)
  })
  vi.mocked(call).mockReset()
  vi.mocked(call).mockImplementation(async (cmd: string) => (cmd === 'trades' ? EMPTY_TRADES : null))
})

afterEach(() => {
  leaveTauri()
})

describe('useMenuBridge', () => {
  it('does nothing outside Tauri: no listener is registered', async () => {
    leaveTauri()
    const { wrapper } = harness()
    renderHook(() => useMenuBridge(), { wrapper })
    await settle()

    expect(listen).not.toHaveBeenCalled()
  })

  it('unlistens on unmount', async () => {
    const { wrapper } = harness()
    const { unmount } = renderHook(() => useMenuBridge(), { wrapper })
    await settle()

    unmount()

    expect(unlisten).toHaveBeenCalled()
  })

  it('trades.import emits the open-source-dialog signal', async () => {
    const { wrapper } = harness()
    renderHook(() => useMenuBridge(), { wrapper })
    await settle()

    act(() => fire('trades.import'))

    expect(useUiSignals.getState().last?.signal).toBe('open-source-dialog')
  })

  it('trades.open_sheet calls open_sheet', async () => {
    const { wrapper } = harness()
    renderHook(() => useMenuBridge(), { wrapper })
    await settle()

    act(() => fire('trades.open_sheet'))
    await settle()

    expect(call).toHaveBeenCalledWith('open_sheet')
  })

  it('trades.clear emits the open-clear-dialog signal', async () => {
    const { wrapper } = harness()
    renderHook(() => useMenuBridge(), { wrapper })
    await settle()

    act(() => fire('trades.clear'))

    expect(useUiSignals.getState().last?.signal).toBe('open-clear-dialog')
  })

  it('trades.reload re-reads the book from disk, then invalidates every query', async () => {
    const { client, wrapper } = harness()
    const spy = vi.spyOn(client, 'invalidateQueries')
    renderHook(() => useMenuBridge(), { wrapper })
    await settle()

    act(() => fire('trades.reload'))
    await settle()

    // Invalidating alone would only re-ask the running app for the book it
    // already holds; `reload` is what re-reads the files.
    expect(call).toHaveBeenCalledWith('reload')
    expect(spy).toHaveBeenCalled()
  })

  it('trades.reveal reveals the CSV source’s parent folder', async () => {
    const { client, wrapper } = harness()
    client.setQueryData(queryKeys.tradeSource, CSV_SOURCE)
    renderHook(() => useMenuBridge(), { wrapper })
    await settle()

    act(() => fire('trades.reveal'))
    await settle()

    expect(call).toHaveBeenCalledWith('reveal_folder', { request: { path: '/home/trader/Downloads' } })
  })

  it('trades.reveal does nothing for a sheet source', async () => {
    const { client, wrapper } = harness()
    client.setQueryData(queryKeys.tradeSource, SHEET_SOURCE)
    renderHook(() => useMenuBridge(), { wrapper })
    await settle()

    act(() => fire('trades.reveal'))
    await settle()

    expect(call).not.toHaveBeenCalledWith('reveal_folder', expect.anything())
  })

  it('trades.reveal does nothing when no source is loaded', async () => {
    const { wrapper } = harness()
    renderHook(() => useMenuBridge(), { wrapper })
    await settle()

    act(() => fire('trades.reveal'))
    await settle()

    expect(call).not.toHaveBeenCalledWith('reveal_folder', expect.anything())
  })

  it('trades.demo loads the demo book and writes queryKeys.tradeSource', async () => {
    vi.mocked(call).mockImplementation(async (cmd: string) => {
      if (cmd === 'load_demo') return DEMO_SOURCE
      if (cmd === 'trades') return EMPTY_TRADES
      return null
    })
    const { client, wrapper } = harness()
    renderHook(() => useMenuBridge(), { wrapper })
    await settle()

    act(() => fire('trades.demo'))
    await settle()

    expect(client.getQueryData(queryKeys.tradeSource)).toEqual(DEMO_SOURCE)
  })

  it('trades.leave_demo leaves the demo book and writes queryKeys.tradeSource', async () => {
    vi.mocked(call).mockImplementation(async (cmd: string) => {
      if (cmd === 'leave_demo') return CSV_SOURCE
      if (cmd === 'trades') return EMPTY_TRADES
      return null
    })
    const { client, wrapper } = harness()
    client.setQueryData(queryKeys.tradeSource, DEMO_SOURCE)
    renderHook(() => useMenuBridge(), { wrapper })
    await settle()

    act(() => fire('trades.leave_demo'))
    await settle()

    expect(client.getQueryData(queryKeys.tradeSource)).toEqual(CSV_SOURCE)
  })

  it('view.risk_graph maximizes the graph on the positions tab; view.dashboard restores it', async () => {
    const { wrapper } = harness()
    renderHook(() => useMenuBridge(), { wrapper })
    await settle()
    useAppStore.getState().setTab('closed')

    act(() => fire('view.risk_graph'))
    expect(useAppStore.getState().tab).toBe('positions')
    expect(useAppStore.getState().graphMaximized).toBe(true)

    useAppStore.getState().setTab('closed')
    act(() => fire('view.dashboard'))
    expect(useAppStore.getState().tab).toBe('positions')
    expect(useAppStore.getState().graphMaximized).toBe(false)
  })

  it('view.closed switches to the closed tab', async () => {
    const { wrapper } = harness()
    renderHook(() => useMenuBridge(), { wrapper })
    await settle()

    act(() => fire('view.closed'))

    expect(useAppStore.getState().tab).toBe('closed')
  })

  it('view.health and help.diagnostics both switch to the health tab', async () => {
    const { wrapper } = harness()
    renderHook(() => useMenuBridge(), { wrapper })
    await settle()

    act(() => fire('view.health'))
    expect(useAppStore.getState().tab).toBe('health')

    useAppStore.getState().setTab('positions')
    act(() => fire('help.diagnostics'))
    expect(useAppStore.getState().tab).toBe('health')
  })

  it('view.theme toggles the theme', async () => {
    const { wrapper } = harness()
    renderHook(() => useMenuBridge(), { wrapper })
    await settle()
    const before = useAppStore.getState().theme

    act(() => fire('view.theme'))

    expect(useAppStore.getState().theme).not.toBe(before)
  })

  it('help.logs calls open_log_folder', async () => {
    const { wrapper } = harness()
    renderHook(() => useMenuBridge(), { wrapper })
    await settle()

    act(() => fire('help.logs'))
    await settle()

    expect(call).toHaveBeenCalledWith('open_log_folder')
  })

  it('help.updates calls check_for_updates', async () => {
    const { wrapper } = harness()
    renderHook(() => useMenuBridge(), { wrapper })
    await settle()

    act(() => fire('help.updates'))
    await settle()

    expect(call).toHaveBeenCalledWith('check_for_updates')
  })

  it('an unknown id is a no-op', async () => {
    const { wrapper } = harness()
    renderHook(() => useMenuBridge(), { wrapper })
    await settle()

    act(() => fire('totally.unknown'))
    await settle()

    expect(call).not.toHaveBeenCalled()
    expect(useUiSignals.getState().last).toBeNull()
  })
})
