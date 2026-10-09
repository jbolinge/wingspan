/**
 * The event tape: Tauri events in the desktop app, server-sent events
 * (`GET /api/v1/events`) in the browser.
 *
 * `snapshot` writes straight into the market-snapshot cache (no refetch round
 * trip) and marks the option chain and the model's order lines stale — the
 * server re-solves its IVs off each new snapshot, and caches the quotes for a
 * refresh cadence, so the refetch is cheap. `trades_changed` invalidates everything derived from the
 * trade files, and `provider` patches the source/degraded fields so the
 * status pill reacts the instant a provider flips. `trades_changed` and
 * `app_ready` also bump the book epoch (`epoch.ts`), which re-registers the
 * modeled contracts a rebuilt runtime has forgotten. In mock mode the same
 * handlers are driven by a simulated tape.
 *
 * The desktop tape has no `open` event of its own: the dot goes live as soon
 * as the subscriptions are registered, and a 15 s `heartbeat` keeps it there —
 * 45 s of silence means the market loop has stopped and the dot goes out.
 */

import { useQueryClient } from '@tanstack/react-query'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { useEffect, useState } from 'react'
import { isTauri } from '../lib/tauri'
import { API_BASE, isMockActive, onMockModeChange } from './client'
import { useBookEpoch } from './epoch'
import { startMockEvents } from './mock'
import { queryKeys, SNAPSHOT_DEPENDENT_KEYS, TRADE_DEPENDENT_KEYS } from './queries'
import type { MarketSnapshot, ProviderEvent } from './types'

export type StreamStatus = 'connecting' | 'open' | 'closed' | 'mock'

/** How long the desktop tape may stay silent before the dot goes out. */
const SILENCE_MS = 45_000

export function useEventStream(): StreamStatus {
  const client = useQueryClient()
  const [sseStatus, setSseStatus] = useState<'connecting' | 'open' | 'closed'>('connecting')
  const [mock, setMock] = useState(isMockActive())
  const supported = typeof EventSource !== 'undefined'

  useEffect(() => onMockModeChange(() => setMock(isMockActive())), [])

  useEffect(() => {
    const applySnapshot = (snapshot: MarketSnapshot): void => {
      client.setQueryData(queryKeys.snapshot, snapshot)
      for (const key of SNAPSHOT_DEPENDENT_KEYS) void client.invalidateQueries({ queryKey: key })
    }

    const invalidateTrades = (): void => {
      for (const key of TRADE_DEPENDENT_KEYS) {
        void client.invalidateQueries({ queryKey: key })
      }
      useBookEpoch.getState().bump()
    }

    if (mock) {
      return startMockEvents((event, payload) => {
        if (event === 'snapshot') applySnapshot(payload as MarketSnapshot)
        else if (event === 'trades_changed') invalidateTrades()
      })
    }

    if (isTauri()) {
      let cancelled = false
      const unlisteners: UnlistenFn[] = []
      let silence: ReturnType<typeof setTimeout> | null = null

      // Registered after teardown is registered too late: unlisten at once.
      const track = (unlisten: UnlistenFn): void => {
        if (cancelled) void unlisten()
        else unlisteners.push(unlisten)
      }

      const armSilence = (): void => {
        if (silence !== null) clearTimeout(silence)
        silence = setTimeout(() => setSseStatus('closed'), SILENCE_MS)
      }

      void Promise.all([
        listen<MarketSnapshot>('snapshot', (event) => applySnapshot(event.payload)),
        listen<ProviderEvent>('provider', (event) => {
          const payload = event.payload
          client.setQueryData<MarketSnapshot>(queryKeys.snapshot, (previous) =>
            previous ? { ...previous, ...payload } : previous,
          )
        }),
        listen('trades_changed', () => invalidateTrades()),
      ]).then((unlistens) => {
        for (const unlisten of unlistens) track(unlisten)
        if (cancelled) return
        // Nothing else says "connected": waiting for the first heartbeat
        // would leave the dot amber for up to 15 s after every launch.
        setSseStatus('open')
        armSilence()
      })

      void listen('heartbeat', () => {
        setSseStatus('open')
        armSilence()
      }).then(track)

      // Belt to the braces of the rebuild commands, which return the new
      // state themselves; a ready that beats this subscription loses nothing.
      void listen('app_ready', () => {
        void client.invalidateQueries()
        useBookEpoch.getState().bump()
      }).then(track)

      void listen<{ detail?: string }>('app_error', (event) => {
        console.error(`wingspan: ${event.payload?.detail ?? 'the app failed to start'}`)
      }).then(track)

      return () => {
        cancelled = true
        if (silence !== null) clearTimeout(silence)
        for (const unlisten of unlisteners) void unlisten()
        unlisteners.length = 0
      }
    }

    if (typeof EventSource === 'undefined') return undefined

    const source = new EventSource(`${API_BASE}/events`)

    const parse = <T,>(event: MessageEvent): T | null => {
      try {
        return JSON.parse(event.data as string) as T
      } catch {
        return null
      }
    }

    const onSnapshot = (event: MessageEvent): void => {
      const payload = parse<MarketSnapshot>(event)
      if (payload) applySnapshot(payload)
    }

    const onTradesChanged = (): void => invalidateTrades()

    const onProvider = (event: MessageEvent): void => {
      const payload = parse<ProviderEvent>(event)
      if (!payload) return
      client.setQueryData<MarketSnapshot>(queryKeys.snapshot, (previous) =>
        previous ? { ...previous, ...payload } : previous,
      )
    }

    source.addEventListener('open', () => setSseStatus('open'))
    source.addEventListener('snapshot', onSnapshot)
    source.addEventListener('trades_changed', onTradesChanged)
    source.addEventListener('provider', onProvider)
    source.addEventListener('error', () => {
      // EventSource reconnects on its own; reflect the gap in the UI meanwhile.
      setSseStatus(source.readyState === EventSource.CLOSED ? 'closed' : 'connecting')
    })

    return () => source.close()
  }, [client, mock])

  if (mock) return 'mock'
  return supported ? sseStatus : 'closed'
}
