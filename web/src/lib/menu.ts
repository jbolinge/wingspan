/**
 * The native menu bridge: the desktop app's menu bar (`menu.rs`) emits a
 * `menu` Tauri event carrying the clicked item's id as a plain string. This
 * hook is the only place that interprets those ids — everything else about
 * a menu click flows through the same commands and store actions a click in
 * the SPA would use. Outside Tauri (the browser devserver, Playwright) there
 * is no native menu, so the hook does nothing.
 */

import { useQueryClient } from '@tanstack/react-query'
import { listen } from '@tauri-apps/api/event'
import { useEffect } from 'react'
import { call } from '../api/client'
import { adoptNewRoot, queryKeys } from '../api/queries'
import type { TradesRootResponse } from '../api/types'
import { useAppStore } from '../store/appStore'
import { isTauri } from './tauri'
import { useUiSignals } from './uiSignals'

export function useMenuBridge(): void {
  const client = useQueryClient()

  useEffect(() => {
    if (!isTauri()) return

    let cancelled = false
    let cleanup: (() => void) | undefined

    const reveal = (): void => {
      const root = client.getQueryData<TradesRootResponse>(queryKeys.tradesRoot)
      const path = root?.root
      if (path) void call('reveal_folder', { request: { path } }).catch(() => {})
    }

    const rebuild = (command: 'load_demo' | 'leave_demo'): void => {
      void call<TradesRootResponse>(command)
        .then((data) => adoptNewRoot(client, data))
        .catch((error: unknown) => console.error(`wingspan: ${command} failed`, error))
    }

    const fireAndForget = (command: 'open_log_folder' | 'check_for_updates'): void => {
      void call(command).catch((error: unknown) => console.error(`wingspan: ${command} failed`, error))
    }

    /** What the status bar's Reload button does: re-read the book from disk
     *  (which also asks the market loop for a fresh snapshot), then refetch
     *  everything. Invalidating alone would only re-ask the running app for
     *  the book it already has. */
    const reload = (): void => {
      void call('reload')
        .then(() => client.invalidateQueries())
        .catch((error: unknown) => console.error('wingspan: reload failed', error))
    }

    void listen<string>('menu', (event) => {
      const id = event.payload
      switch (id) {
        case 'trades.choose':
          useUiSignals.getState().emit('open-trades-dialog')
          break
        case 'trades.reload':
          reload()
          break
        case 'trades.reveal':
          reveal()
          break
        case 'trades.clear':
          useUiSignals.getState().emit('open-clear-dialog')
          break
        case 'trades.demo':
          rebuild('load_demo')
          break
        case 'trades.leave_demo':
          rebuild('leave_demo')
          break
        // Same as the graph header's maximize / restore toggle: Dashboard is
        // the whole positions tab, Risk graph is the graph alone on it.
        case 'view.dashboard':
          useAppStore.getState().setTab('positions')
          useAppStore.getState().setGraphMaximized(false)
          break
        case 'view.risk_graph':
          useAppStore.getState().setTab('positions')
          useAppStore.getState().setGraphMaximized(true)
          break
        case 'view.closed':
          useAppStore.getState().setTab('closed')
          break
        case 'view.health':
        case 'help.diagnostics':
          useAppStore.getState().setTab('health')
          break
        case 'view.theme':
          useAppStore.getState().toggleTheme()
          break
        case 'help.logs':
          fireAndForget('open_log_folder')
          break
        case 'help.updates':
          fireAndForget('check_for_updates')
          break
        default:
          console.debug(`wingspan: unhandled menu id "${id}"`)
      }
    }).then((unlisten) => {
      if (cancelled) void unlisten()
      else cleanup = unlisten
    })

    return () => {
      cancelled = true
      cleanup?.()
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])
}
